from __future__ import annotations

import gzip
import hashlib
import json
import os
import platform
import shutil
import stat
import sys
import tarfile
import urllib.request
import zipfile
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from .config import ctx_dir


@dataclass(frozen=True)
class Server:
    language: str
    name: str
    repository: str
    commands: tuple[str, ...]
    args: tuple[str, ...]
    fetchable: bool = True
    note: str | None = None


SERVERS: dict[str, Server] = {
    "python": Server(
        "python", "BasedPyright", "DetachHead/basedpyright",
        ("basedpyright-langserver", "pyright-langserver"), ("--stdio",),
        note="La wheel GitHub est exécutée avec Bun, sans npm.",
    ),
    "typescript": Server(
        "typescript", "TypeScript 7 native", "microsoft/typescript-go",
        ("tsc", "typescript-language-server"), ("--lsp",),
        note="Le binaire natif officiel couvre JavaScript et TypeScript.",
    ),
    "go": Server(
        "go", "gopls", "golang/tools", ("gopls",), (), fetchable=False,
        note="GitHub ne publie pas de binaire; ctx ne compile pas silencieusement la source.",
    ),
    "rust": Server(
        "rust", "rust-analyzer", "rust-lang/rust-analyzer",
        ("rust-analyzer",), (),
    ),
    "php": Server(
        "php", "Phpactor", "phpactor/phpactor", ("phpactor",), ("language-server",),
        note="L'artefact PHAR officiel requiert PHP dans PATH.",
    ),
}

GITHUB_API = "https://api.github.com/repos/{repository}/releases/latest"


def sources() -> list[dict[str, Any]]:
    return [
        {
            **asdict(server),
            "commands": list(server.commands),
            "args": list(server.args),
            "github": f"https://github.com/{server.repository}",
            "releases": f"https://github.com/{server.repository}/releases",
        }
        for server in SERVERS.values()
    ]


def status(root: Path, *, path_env: str | None = None) -> dict[str, Any]:
    install_root = ctx_dir(root) / "lsp"
    manifest = _load_manifest(install_root)
    search_path = os.environ.get("PATH", "") if path_env is None else path_env
    rows = []
    for language, server in SERVERS.items():
        local = manifest.get(language)
        detected = next((shutil.which(command, path=search_path) for command in server.commands if shutil.which(command, path=search_path)), None)
        command = command_for(root, language, path_env=search_path)
        available = command is not None
        rows.append({
            "language": language,
            "name": server.name,
            "available": available,
            "origin": "github" if local else ("path" if detected else None),
            "command": command,
            "repository": server.repository,
            "fetchable": server.fetchable,
            "note": server.note,
            "version": local.get("version") if local else None,
        })
    return {"install_root": str(install_root), "servers": rows}


def command_for(root: Path, language: str, *, path_env: str | None = None) -> list[str] | None:
    """Return an argv for a fetched or PATH-provided server."""
    install_root = ctx_dir(root) / "lsp"
    local = _load_manifest(install_root).get(language)
    if isinstance(local, dict) and isinstance(local.get("command"), list):
        return _normalize_command(language, [str(part) for part in local["command"]])
    server = SERVERS[language]
    search_path = os.environ.get("PATH", "") if path_env is None else path_env
    for candidate in server.commands:
        executable = shutil.which(candidate, path=search_path)
        if not executable:
            continue
        if language == "typescript" and candidate == "typescript-language-server":
            bun = shutil.which("bun", path=search_path)
            if not bun:
                return None
            return [bun, executable, "--stdio"]
        if language == "python" and candidate == "pyright-langserver":
            bun = shutil.which("bun", path=search_path)
            if not bun:
                return None
            return [bun, executable, "--stdio"]
        return _normalize_command(language, [executable, *server.args])
    return None


def _normalize_command(language: str, command: list[str]) -> list[str]:
    if language == "typescript" and "--lsp" in command and "--stdio" not in command:
        return [*command, "--stdio"]
    return command


def fetch(root: Path, languages: list[str], *, dry_run: bool = False, force: bool = False) -> dict[str, Any]:
    selected = list(SERVERS) if not languages or "all" in languages else list(dict.fromkeys(languages))
    install_root = ctx_dir(root) / "lsp"
    manifest = _load_manifest(install_root)
    results: list[dict[str, Any]] = []
    for language in selected:
        server = SERVERS[language]
        if not server.fetchable:
            results.append({"language": language, "status": "source_only", "repository": server.repository, "hint": server.note})
            continue
        release = _github_json(GITHUB_API.format(repository=server.repository))
        asset = _select_asset(language, release.get("assets", []))
        if asset is None:
            results.append({"language": language, "status": "unavailable", "version": release.get("tag_name"), "hint": "aucun artefact compatible avec cet OS/CPU"})
            continue
        previous = manifest.get(language, {})
        if previous.get("version") == release.get("tag_name") and not force:
            results.append({"language": language, "status": "current", "version": release.get("tag_name"), "asset": asset["name"]})
            continue
        result = {
            "language": language, "status": "planned" if dry_run else "installed",
            "version": release.get("tag_name"), "asset": asset["name"],
            "url": asset["browser_download_url"], "digest": asset.get("digest"),
        }
        if not dry_run:
            command = _install_asset(language, asset, install_root)
            (install_root / "downloads" / asset["name"]).unlink(missing_ok=True)
            manifest[language] = {
                "name": server.name, "repository": server.repository,
                "version": release.get("tag_name"), "asset": asset["name"],
                "digest": asset.get("digest"), "command": command,
            }
        results.append(result)
    if not dry_run:
        install_root.mkdir(parents=True, exist_ok=True)
        _write_json(install_root / "servers.json", manifest)
    return {"dry_run": dry_run, "install_root": str(install_root), "results": results}


def _github_json(url: str) -> dict[str, Any]:
    request = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json", "User-Agent": "ctx-code"})
    with urllib.request.urlopen(request, timeout=30) as response:
        value = json.load(response)
    if not isinstance(value, dict):
        raise RuntimeError(f"réponse GitHub invalide: {url}")
    return value


def _platform() -> tuple[str, str]:
    os_name = {"linux": "linux", "darwin": "darwin", "win32": "win32"}.get(sys.platform)
    arch = {"x86_64": "x64", "amd64": "x64", "aarch64": "arm64", "arm64": "arm64", "armv7l": "arm"}.get(platform.machine().lower())
    if not os_name or not arch:
        raise RuntimeError(f"plateforme LSP non prise en charge: {sys.platform}/{platform.machine()}")
    return os_name, arch


def _select_asset(language: str, assets: list[dict[str, Any]]) -> dict[str, Any] | None:
    os_name, arch = _platform()
    names: tuple[str, ...]
    if language == "python":
        names = (".whl",)
    elif language == "typescript":
        names = (f"typescript-{os_name}-{arch}.tgz",)
    elif language == "rust":
        rust_arch = {"x64": "x86_64", "arm64": "aarch64", "arm": "arm"}[arch]
        rust_os = {"linux": "unknown-linux-gnu", "darwin": "apple-darwin", "win32": "pc-windows-msvc"}[os_name]
        names = (f"rust-analyzer-{rust_arch}-{rust_os}.gz", f"rust-analyzer-{rust_arch}-{rust_os}.zip")
    elif language == "php":
        names = ("phpactor.phar",)
    else:
        return None
    for asset in assets:
        name = str(asset.get("name", ""))
        if any(name == wanted or (wanted == ".whl" and name.endswith(wanted)) for wanted in names):
            return asset
    return None


def _download(asset: dict[str, Any], destination: Path) -> Path:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".part")
    request = urllib.request.Request(asset["browser_download_url"], headers={"User-Agent": "ctx-code"})
    digest = hashlib.sha256()
    with urllib.request.urlopen(request, timeout=120) as response, temporary.open("wb") as output:
        while chunk := response.read(1024 * 1024):
            output.write(chunk)
            digest.update(chunk)
    expected = str(asset.get("digest") or "")
    if expected.startswith("sha256:") and digest.hexdigest() != expected.removeprefix("sha256:"):
        temporary.unlink(missing_ok=True)
        raise RuntimeError(f"SHA-256 GitHub invalide pour {asset['name']}")
    temporary.replace(destination)
    return destination


def _install_asset(language: str, asset: dict[str, Any], install_root: Path) -> list[str]:
    downloads = install_root / "downloads"
    archive = _download(asset, downloads / asset["name"])
    bin_dir = install_root / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    if language == "python":
        target = install_root / "python"
        _extract_zip(archive, target, prefix="basedpyright/")
        bun = shutil.which("bun")
        if not bun:
            raise RuntimeError("Bun est requis pour exécuter BasedPyright sans npm")
        return [bun, str(target / "basedpyright" / "langserver.index.js"), "--stdio"]
    if language == "typescript":
        target = install_root / "typescript"
        _extract_tar(archive, target, prefix="package/")
        executable = target / "lib" / "tsc"
        _make_executable(executable)
        return [str(executable), "--lsp", "--stdio"]
    if language == "rust":
        executable = bin_dir / ("rust-analyzer.exe" if sys.platform == "win32" else "rust-analyzer")
        if archive.suffix == ".gz":
            with gzip.open(archive, "rb") as source, executable.open("wb") as output:
                shutil.copyfileobj(source, output)
        else:
            with zipfile.ZipFile(archive) as source:
                member = next(name for name in source.namelist() if Path(name).name.startswith("rust-analyzer"))
                executable.write_bytes(source.read(member))
        _make_executable(executable)
        return [str(executable)]
    if language == "php":
        executable = bin_dir / "phpactor.phar"
        shutil.copyfile(archive, executable)
        php = shutil.which("php")
        if not php:
            raise RuntimeError("PHP est requis pour exécuter phpactor.phar")
        return [php, str(executable), "language-server"]
    raise ValueError(f"langage LSP inconnu: {language}")


def _extract_zip(archive: Path, target: Path, *, prefix: str) -> None:
    with zipfile.ZipFile(archive) as source:
        for member in source.infolist():
            if not member.filename.startswith(prefix) or member.is_dir():
                continue
            relative = Path(member.filename)
            destination = _safe_destination(target, relative)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source.read(member))


def _extract_tar(archive: Path, target: Path, *, prefix: str) -> None:
    with tarfile.open(archive, "r:gz") as source:
        for member in source.getmembers():
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            relative = Path(member.name).relative_to(prefix.rstrip("/"))
            destination = _safe_destination(target, relative)
            destination.parent.mkdir(parents=True, exist_ok=True)
            stream = source.extractfile(member)
            if stream is not None:
                with destination.open("wb") as output:
                    shutil.copyfileobj(stream, output)
            if member.mode & stat.S_IXUSR:
                _make_executable(destination)


def _safe_destination(root: Path, relative: Path) -> Path:
    destination = (root / relative).resolve()
    if root.resolve() not in destination.parents:
        raise RuntimeError(f"archive GitHub dangereuse: {relative}")
    return destination


def _make_executable(path: Path) -> None:
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _load_manifest(install_root: Path) -> dict[str, Any]:
    path = install_root / "servers.json"
    if not path.is_file():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
