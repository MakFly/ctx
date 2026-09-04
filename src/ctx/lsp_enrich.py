from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO
from urllib.parse import unquote, urlparse
from urllib.request import url2pathname

from .config import find_ctx
from .db import connect, get_meta
from .lsp import SERVERS, command_for


LANGUAGE_GROUP = {
    "python": "python",
    "javascript": "typescript",
    "typescript": "typescript",
    "tsx": "typescript",
    "go": "go",
    "rust": "rust",
    "php": "php",
}

LANGUAGE_ID = {
    ".py": "python", ".pyi": "python", ".pyw": "python",
    ".js": "javascript", ".jsx": "javascriptreact", ".mjs": "javascript", ".cjs": "javascript",
    ".ts": "typescript", ".tsx": "typescriptreact", ".mts": "typescript", ".cts": "typescript",
    ".go": "go", ".rs": "rust", ".php": "php", ".phtml": "php",
}


@dataclass(frozen=True)
class Reference:
    dst_symbol_id: int
    dst_name: str
    path: str
    line: int


class JsonRpcClient:
    def __init__(self, command: list[str], cwd: Path, *, env: dict[str, str] | None = None,
                 configuration: dict[str, Any] | None = None) -> None:
        self.process = subprocess.Popen(
            command, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            bufsize=0, env=env,
        )
        if self.process.stdin is None or self.process.stdout is None or self.process.stderr is None:
            raise RuntimeError(f"impossible d'ouvrir le serveur LSP: {command[0]}")
        self.stdin = self.process.stdin
        self.messages: queue.Queue[dict[str, Any] | BaseException] = queue.Queue()
        self.pending: dict[int, dict[str, Any]] = {}
        self.next_id = 1
        self.write_lock = threading.Lock()
        self.stderr: list[str] = []
        self.configuration = configuration or {}
        threading.Thread(target=self._read_messages, args=(self.process.stdout,), daemon=True).start()
        threading.Thread(target=self._read_stderr, args=(self.process.stderr,), daemon=True).start()

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def request(self, method: str, params: dict[str, Any], timeout: float) -> Any:
        request_id = self.next_id
        self.next_id += 1
        self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while True:
            if request_id in self.pending:
                message = self.pending.pop(request_id)
                return self._result(message, method)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"timeout LSP pendant {method}")
            try:
                message = self.messages.get(timeout=remaining)
            except queue.Empty as exc:
                raise TimeoutError(f"timeout LSP pendant {method}") from exc
            if isinstance(message, BaseException):
                detail = " | ".join(self.stderr[-5:]) if self.stderr else str(message)
                code = self.process.poll()
                raise RuntimeError(f"serveur LSP arrêté pendant {method} (exit={code}): {detail}") from message
            if "method" in message and "id" in message:
                self._answer_server_request(message)
            elif "id" in message:
                response_id = message.get("id")
                if response_id == request_id:
                    return self._result(message, method)
                if isinstance(response_id, int):
                    self.pending[response_id] = message

    def close(self, timeout: float = 3.0) -> None:
        if self.process.poll() is None:
            try:
                self.request("shutdown", {}, timeout)
                self.notify("exit")
                self.process.wait(timeout=timeout)
            except (OSError, RuntimeError, TimeoutError, subprocess.TimeoutExpired):
                self.process.terminate()
                try:
                    self.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    self.process.kill()

    def _write(self, message: dict[str, Any]) -> None:
        payload = json.dumps(message, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        framed = f"Content-Length: {len(payload)}\r\n\r\n".encode("ascii") + payload
        with self.write_lock:
            self.stdin.write(framed)
            self.stdin.flush()

    def _read_messages(self, stream: BinaryIO) -> None:
        try:
            while True:
                headers: dict[str, str] = {}
                while "content-length" not in headers:
                    line = stream.readline()
                    if not line:
                        raise EOFError("EOF LSP")
                    key, separator, value = line.decode("ascii", errors="replace").partition(":")
                    if separator:
                        headers[key.lower().strip()] = value.strip()
                while True:
                    line = stream.readline()
                    if not line:
                        raise EOFError("EOF LSP")
                    if line in {b"\r\n", b"\n"}:
                        break
                    key, separator, value = line.decode("ascii", errors="replace").partition(":")
                    if separator:
                        headers[key.lower().strip()] = value.strip()
                length = int(headers["content-length"])
                payload = stream.read(length)
                if len(payload) != length:
                    raise EOFError("message LSP tronqué")
                value = json.loads(payload)
                if isinstance(value, dict):
                    self.messages.put(value)
        except BaseException as exc:
            self.messages.put(exc)

    def _read_stderr(self, stream: BinaryIO) -> None:
        for raw in iter(stream.readline, b""):
            self.stderr.append(raw.decode("utf-8", errors="replace").rstrip())
            self.stderr[:] = self.stderr[-20:]

    def _answer_server_request(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params") or {}
        if method == "workspace/configuration":
            result: Any = [self.configuration for _ in params.get("items", [])]
        elif method == "workspace/workspaceFolders":
            result = []
        else:
            result = None
        self._write({"jsonrpc": "2.0", "id": message["id"], "result": result})

    @staticmethod
    def _result(message: dict[str, Any], method: str) -> Any:
        if "error" in message:
            error = message["error"]
            raise RuntimeError(f"erreur LSP {method}: {error.get('message', error)}")
        return message.get("result")


def enrich(root: Path, languages: list[str], *, max_symbols: int = 500, timeout: float = 60.0) -> dict[str, Any]:
    database = find_ctx(root) / "index.sqlite"
    conn = connect(database, create=True)
    try:
        indexed_root = Path(get_meta(conn, "repo_root", str(root))).resolve()
        selected = list(SERVERS) if not languages or "all" in languages else list(dict.fromkeys(languages))
        results: list[dict[str, Any]] = []
        total_edges = 0
        for language in selected:
            command = command_for(indexed_root, language)
            db_langs = [name for name, group in LANGUAGE_GROUP.items() if group == language]
            placeholders = ",".join("?" for _ in db_langs)
            file_rows = conn.execute(
                f"SELECT id,path,lang FROM files WHERE lang IN ({placeholders}) ORDER BY path", db_langs,
            ).fetchall()
            if not file_rows:
                results.append({"language": language, "status": "no_files", "edges": 0})
                continue
            if command is None:
                results.append({"language": language, "status": "missing", "edges": 0, "hint": f"ctx lsp fetch --language {language}"})
                continue
            try:
                references, queried = _enrich_language(conn, indexed_root, language, command, file_rows, max_symbols, timeout)
                inserted = _store_references(conn, language, references)
                conn.commit()
                total_edges += inserted
                results.append({"language": language, "status": "enriched", "symbols": queried, "edges": inserted, "command": command})
            except Exception as exc:
                conn.rollback()
                results.append({"language": language, "status": "error", "edges": 0, "error": str(exc), "command": command})
        return {"database": str(database), "edges": total_edges, "results": results}
    finally:
        conn.close()
        pid_file = os.environ.get("CTX_LSP_BACKGROUND_PID_FILE")
        if pid_file:
            path = Path(pid_file)
            try:
                if path.read_text(encoding="ascii").strip() == str(os.getpid()):
                    path.unlink()
            except OSError:
                pass


def start_background(root: Path, languages: list[str], *, max_symbols: int = 500,
                     timeout: float = 60.0) -> dict[str, Any]:
    state = find_ctx(root) / "lsp"
    state.mkdir(parents=True, exist_ok=True)
    log_path = state / "enrich.log"
    command = [
        sys.executable, "-m", "ctx.cli", "lsp", "enrich", str(root),
        "--max-symbols", str(max_symbols), "--timeout", str(timeout), "--json",
    ]
    for language in languages:
        command.extend(("--language", language))
    environment = dict(os.environ)
    environment["PYTHONUNBUFFERED"] = "1"
    pid_path = state / "enrich.pid"
    environment["CTX_LSP_BACKGROUND_PID_FILE"] = str(pid_path)
    with log_path.open("ab") as log:
        options: dict[str, Any] = {
            "cwd": root, "stdin": subprocess.DEVNULL, "stdout": log, "stderr": subprocess.STDOUT,
            "env": environment, "close_fds": True,
        }
        if os.name == "posix":
            options["start_new_session"] = True
        process = subprocess.Popen(command, **options)
    pid_path.write_text(f"{process.pid}\n", encoding="ascii")
    return {"started": True, "pid": process.pid, "log": str(log_path), "pid_file": str(pid_path)}


def _enrich_language(conn: Any, root: Path, language: str, command: list[str], file_rows: list[Any],
                     max_symbols: int, timeout: float) -> tuple[list[Reference], int]:
    environment = None
    initialization_options: dict[str, Any] = {}
    configuration: dict[str, Any] = {}
    if language == "typescript":
        environment = dict(os.environ)
        environment["PATH"] = os.pathsep.join(
            directory for directory in environment.get("PATH", "").split(os.pathsep)
            if directory and not any((Path(directory) / name).exists() for name in ("npm", "npm.cmd", "npm.exe"))
        )
        initialization_options["disableAutomaticTypingAcquisition"] = True
        configuration = {
            "disableAutomaticTypeAcquisition": True,
            "tsserver": {"automaticTypeAcquisition": {"enabled": False}},
            "check": {"npmIsInstalled": False},
        }
    if language == "php":
        environment = dict(os.environ)
        cache = find_ctx(root) / "lsp" / "cache"
        (cache / "phpactor" / "index").mkdir(parents=True, exist_ok=True)
        environment["XDG_CACHE_HOME"] = str(cache)
        initialization_options = {
            "indexer.index_path": str(cache / "phpactor" / "index"),
            "indexer.enabled_watchers": ["lsp"],
            "language_server.diagnostics_on_update": False,
        }
    client = JsonRpcClient(command, root, env=environment, configuration=configuration)
    root_uri = root.as_uri()
    try:
        client.request("initialize", {
            "processId": None,
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": root.name}],
            "capabilities": {
                "workspace": {"configuration": True, "workspaceFolders": True},
                "textDocument": {"references": {"dynamicRegistration": False}},
            },
            "clientInfo": {"name": "ctx", "version": "0.1.0"},
            "initializationOptions": initialization_options,
        }, timeout)
        client.notify("initialized")
        contents: dict[int, tuple[Path, str]] = {}
        for row in file_rows:
            path = root / row["path"]
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            contents[int(row["id"])] = (path, text)
            client.notify("textDocument/didOpen", {"textDocument": {
                "uri": path.as_uri(), "languageId": LANGUAGE_ID.get(path.suffix.lower(), language),
                "version": 1, "text": text,
            }})
        file_ids = list(contents)
        if not file_ids:
            return [], 0
        symbols = conn.execute(
            f"SELECT s.id,s.file_id,s.name,s.start,f.path,f.is_test FROM symbols s JOIN files f ON f.id=s.file_id "
            f"WHERE s.file_id IN ({','.join('?' for _ in file_ids)}) AND s.kind!='variable' "
            "ORDER BY f.is_test,s.kind='method',f.path,s.start LIMIT ?",
            (*file_ids, max_symbols),
        ).fetchall()
        references: list[Reference] = []
        for symbol in symbols:
            path, text = contents[int(symbol["file_id"])]
            line = max(0, int(symbol["start"]) - 1)
            character = _symbol_character(text, line, symbol["name"])
            locations = client.request("textDocument/references", {
                "textDocument": {"uri": path.as_uri()},
                "position": {"line": line, "character": character},
                "context": {"includeDeclaration": False},
            }, timeout) or []
            for location in locations if isinstance(locations, list) else []:
                parsed = _reference_location(root, location)
                if parsed is not None:
                    references.append(Reference(int(symbol["id"]), symbol["name"], parsed[0], parsed[1]))
        return references, len(symbols)
    finally:
        client.close()


def _symbol_character(text: str, line: int, name: str) -> int:
    lines = text.splitlines()
    prefix = lines[line] if line < len(lines) else ""
    column = prefix.find(name)
    if column < 0:
        column = len(prefix) - len(prefix.lstrip())
    return len(prefix[:column].encode("utf-16-le")) // 2


def _reference_location(root: Path, location: dict[str, Any]) -> tuple[str, int] | None:
    uri = location.get("uri") or location.get("targetUri")
    range_value = location.get("range") or location.get("targetSelectionRange") or {}
    if not isinstance(uri, str) or not uri.startswith("file:"):
        return None
    parsed = urlparse(uri)
    path = Path(url2pathname(unquote(parsed.path))).resolve()
    try:
        rel = path.relative_to(root).as_posix()
    except ValueError:
        return None
    line = int(range_value.get("start", {}).get("line", 0)) + 1
    return rel, line


def _store_references(conn: Any, language: str, references: list[Reference]) -> int:
    db_langs = [name for name, group in LANGUAGE_GROUP.items() if group == language]
    placeholders = ",".join("?" for _ in db_langs)
    conn.execute(
        f"DELETE FROM edges WHERE source='lsp' AND file_id IN (SELECT id FROM files WHERE lang IN ({placeholders}))",
        db_langs,
    )
    unique: set[tuple[int | None, str, int, int]] = set()
    rows: list[tuple[int | None, str, int, str, int, int, str, float]] = []
    for reference in references:
        file_row = conn.execute("SELECT id FROM files WHERE path=?", (reference.path,)).fetchone()
        if file_row is None:
            continue
        file_id = int(file_row["id"])
        source = conn.execute(
            "SELECT id FROM symbols WHERE file_id=? AND start<=? AND end>=? ORDER BY (end-start),start DESC LIMIT 1",
            (file_id, reference.line, reference.line),
        ).fetchone()
        src_id = int(source["id"]) if source else None
        key = (src_id, reference.dst_name, file_id, reference.line)
        if key in unique:
            continue
        unique.add(key)
        rows.append((src_id, reference.dst_name, reference.dst_symbol_id, "ref", file_id, reference.line, "lsp", 1.0))
    conn.executemany(
        "INSERT INTO edges(src_symbol_id,dst_name,dst_symbol_id,kind,file_id,line,source,confidence) VALUES(?,?,?,?,?,?,?,?)",
        rows,
    )
    return len(rows)
