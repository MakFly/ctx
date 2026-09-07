use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, USER_AGENT};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::{ctx_dir, find_ctx};
use crate::db::{connect, get_meta};

#[derive(Debug, Clone, Serialize)]
pub struct Server {
    pub language: &'static str,
    pub name: &'static str,
    pub repository: &'static str,
    pub commands: &'static [&'static str],
    pub args: &'static [&'static str],
    pub fetchable: bool,
    pub note: Option<&'static str>,
}

pub const SERVERS: &[Server] = &[
    Server {
        language: "python",
        name: "BasedPyright",
        repository: "DetachHead/basedpyright",
        commands: &["basedpyright-langserver", "pyright-langserver"],
        args: &["--stdio"],
        fetchable: true,
        note: Some("La wheel GitHub est exécutée avec Bun, sans npm."),
    },
    Server {
        language: "typescript",
        name: "TypeScript 7 native",
        repository: "microsoft/typescript-go",
        commands: &["tsc", "typescript-language-server"],
        args: &["--lsp"],
        fetchable: true,
        note: Some("Le binaire natif officiel couvre JavaScript et TypeScript."),
    },
    Server {
        language: "go",
        name: "gopls",
        repository: "golang/tools",
        commands: &["gopls"],
        args: &[],
        fetchable: false,
        note: Some(
            "GitHub ne publie pas de binaire; ctx ne compile pas silencieusement la source.",
        ),
    },
    Server {
        language: "rust",
        name: "rust-analyzer",
        repository: "rust-lang/rust-analyzer",
        commands: &["rust-analyzer"],
        args: &[],
        fetchable: true,
        note: None,
    },
    Server {
        language: "php",
        name: "Phpactor",
        repository: "phpactor/phpactor",
        commands: &["phpactor"],
        args: &["language-server"],
        fetchable: true,
        note: Some("L'artefact PHAR officiel requiert PHP dans PATH."),
    },
];

#[derive(Debug, Clone)]
struct Reference {
    dst_symbol_id: i64,
    dst_name: String,
    path: String,
    line: usize,
}

pub fn sources() -> Vec<Value> {
    SERVERS
        .iter()
        .map(|server| {
            json!({
                "language": server.language,
                "name": server.name,
                "repository": server.repository,
                "commands": server.commands,
                "args": server.args,
                "fetchable": server.fetchable,
                "note": server.note,
                "github": format!("https://github.com/{}", server.repository),
                "releases": format!("https://github.com/{}/releases", server.repository),
            })
        })
        .collect()
}

pub fn status(root: &Path, path_environment: Option<&str>) -> Result<Value> {
    let install_root = ctx_dir(root).join("lsp");
    let manifest = load_manifest(&install_root);
    let search_path = path_environment
        .map(str::to_owned)
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    let rows = SERVERS
        .iter()
        .map(|server| {
            let local = manifest.get(server.language).and_then(Value::as_object);
            let detected = server
                .commands
                .iter()
                .find_map(|command| find_executable(command, &search_path));
            let command = command_for(root, server.language, Some(&search_path));
            json!({
                "language": server.language,
                "name": server.name,
                "available": command.is_some(),
                "origin": if local.is_some() { Some("github") } else if detected.is_some() { Some("path") } else { None },
                "command": command,
                "repository": server.repository,
                "fetchable": server.fetchable,
                "note": server.note,
                "version": local.and_then(|value| value.get("version")),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"install_root": install_root, "servers": rows}))
}

pub fn command_for(
    root: &Path,
    language: &str,
    path_environment: Option<&str>,
) -> Option<Vec<String>> {
    let install_root = ctx_dir(root).join("lsp");
    let manifest = load_manifest(&install_root);
    if let Some(command) = manifest
        .get(language)
        .and_then(|value| value.get("command"))
        .and_then(Value::as_array)
    {
        return Some(normalize_command(
            language,
            command
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ));
    }
    let server = server(language)?;
    let search_path = path_environment
        .map(str::to_owned)
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    for candidate in server.commands {
        let Some(executable) = find_executable(candidate, &search_path) else {
            continue;
        };
        if (language == "typescript" && *candidate == "typescript-language-server")
            || (language == "python" && *candidate == "pyright-langserver")
        {
            let bun = find_executable("bun", &search_path)?;
            return Some(vec![
                bun.to_string_lossy().into_owned(),
                executable.to_string_lossy().into_owned(),
                "--stdio".to_owned(),
            ]);
        }
        let mut command = vec![executable.to_string_lossy().into_owned()];
        command.extend(server.args.iter().map(|value| (*value).to_owned()));
        return Some(normalize_command(language, command));
    }
    None
}

pub fn fetch(root: &Path, languages: &[String], dry_run: bool, force: bool) -> Result<Value> {
    let selected = selected_languages(languages)?;
    let install_root = ctx_dir(root).join("lsp");
    let mut manifest = load_manifest(&install_root);
    let client = github_client()?;
    let mut results = Vec::new();
    for language in selected {
        let server = server(&language).context("langage LSP inconnu")?;
        if !server.fetchable {
            results.push(json!({
                "language": language,
                "status": "source_only",
                "repository": server.repository,
                "hint": server.note,
            }));
            continue;
        }
        let release: Value = client
            .get(format!(
                "https://api.github.com/repos/{}/releases/latest",
                server.repository
            ))
            .send()?
            .error_for_status()?
            .json()?;
        let assets = release["assets"].as_array().cloned().unwrap_or_default();
        let Some(asset) = select_asset(&language, &assets)? else {
            results.push(json!({
                "language": language,
                "status": "unavailable",
                "version": release["tag_name"],
                "hint": "aucun artefact compatible avec cet OS/CPU",
            }));
            continue;
        };
        let version = release["tag_name"].as_str().unwrap_or_default();
        if !force
            && manifest
                .get(&language)
                .and_then(|value| value.get("version"))
                .and_then(Value::as_str)
                == Some(version)
        {
            results.push(json!({
                "language": language,
                "status": "current",
                "version": version,
                "asset": asset["name"],
            }));
            continue;
        }
        let mut result = json!({
            "language": language,
            "status": if dry_run { "planned" } else { "installed" },
            "version": version,
            "asset": asset["name"],
            "url": asset["browser_download_url"],
            "digest": asset["digest"],
        });
        if !dry_run {
            let command = install_asset(&client, &language, &asset, &install_root)?;
            let name = asset["name"].as_str().unwrap_or_default();
            let _ = fs::remove_file(install_root.join("downloads").join(name));
            manifest.insert(
                language.clone(),
                json!({
                    "name": server.name,
                    "repository": server.repository,
                    "version": version,
                    "asset": name,
                    "digest": asset["digest"],
                    "command": command,
                }),
            );
            result["command"] = json!(command);
        }
        results.push(result);
    }
    if !dry_run {
        fs::create_dir_all(&install_root)?;
        write_manifest(&install_root, &manifest)?;
    }
    Ok(json!({
        "dry_run": dry_run,
        "install_root": install_root,
        "results": results,
    }))
}

pub fn enrich(
    root: &Path,
    languages: &[String],
    max_symbols: usize,
    timeout: Duration,
) -> Result<Value> {
    let result = enrich_inner(root, languages, max_symbols, timeout);
    cleanup_background_pid();
    result
}

fn enrich_inner(
    root: &Path,
    languages: &[String],
    max_symbols: usize,
    timeout: Duration,
) -> Result<Value> {
    let database = find_ctx(root)?.join("index.sqlite");
    let connection = connect(&database, true)?;
    let indexed_root = PathBuf::from(get_meta(&connection, "repo_root", &root.to_string_lossy())?)
        .canonicalize()?;
    let selected = selected_languages(languages)?;
    let mut results = Vec::new();
    let mut total_edges = 0;
    for language in selected {
        let db_languages = db_languages(&language);
        let file_rows = language_files(&connection, db_languages)?;
        if file_rows.is_empty() {
            results.push(json!({"language": language, "status": "no_files", "edges": 0}));
            continue;
        }
        let Some(command) = command_for(&indexed_root, &language, None) else {
            results.push(json!({
                "language": language,
                "status": "missing",
                "edges": 0,
                "hint": format!("ctx lsp fetch --language {language}"),
            }));
            continue;
        };
        match enrich_language(
            &connection,
            &indexed_root,
            &language,
            &command,
            &file_rows,
            max_symbols,
            timeout,
        ) {
            Ok((references, queried)) => {
                let inserted = store_references(&connection, &language, references)?;
                total_edges += inserted;
                results.push(json!({
                    "language": language,
                    "status": "enriched",
                    "symbols": queried,
                    "edges": inserted,
                    "command": command,
                }));
            }
            Err(error) => results.push(json!({
                "language": language,
                "status": "error",
                "edges": 0,
                "error": error.to_string(),
                "command": command,
            })),
        }
    }
    Ok(json!({"database": database, "edges": total_edges, "results": results}))
}

pub fn start_background(
    root: &Path,
    languages: &[String],
    max_symbols: usize,
    timeout: Duration,
) -> Result<Value> {
    let state = find_ctx(root)?.join("lsp");
    fs::create_dir_all(&state)?;
    let log_path = state.join("enrich.log");
    let pid_path = state.join("enrich.pid");
    let log = File::options().create(true).append(true).open(&log_path)?;
    let error_log = log.try_clone()?;
    let mut command = Command::new(env::current_exe()?);
    command
        .args([
            "lsp",
            "enrich",
            &root.to_string_lossy(),
            "--max-symbols",
            &max_symbols.to_string(),
            "--timeout",
            &timeout.as_secs_f64().to_string(),
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(error_log))
        .env("CTX_LSP_BACKGROUND_PID_FILE", &pid_path);
    for language in languages {
        command.args(["--language", language]);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    fs::write(&pid_path, format!("{}\n", child.id()))?;
    Ok(json!({
        "started": true,
        "pid": child.id(),
        "log": log_path,
        "pid_file": pid_path,
    }))
}

fn cleanup_background_pid() {
    let Some(path) = env::var_os("CTX_LSP_BACKGROUND_PID_FILE").map(PathBuf::from) else {
        return;
    };
    let own_pid = std::process::id().to_string();
    for _ in 0..50 {
        if fs::read_to_string(&path).is_ok_and(|value| value.trim() == own_pid) {
            let _ = fs::remove_file(path);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn enrich_language(
    connection: &Connection,
    root: &Path,
    language: &str,
    command: &[String],
    files: &[(i64, String, String)],
    max_symbols: usize,
    timeout: Duration,
) -> Result<(Vec<Reference>, usize)> {
    let configuration = if language == "typescript" {
        json!({
            "disableAutomaticTypeAcquisition": true,
            "tsserver": {"automaticTypeAcquisition": {"enabled": false}},
            "check": {"npmIsInstalled": false},
        })
    } else {
        json!({})
    };
    let mut process = JsonRpcClient::start(command, root, configuration)?;
    let root_uri = url::Url::from_directory_path(root)
        .map_err(|_| anyhow::anyhow!("URI de repo invalide"))?
        .to_string();
    let initialization_options = if language == "php" {
        let cache = find_ctx(root)?.join("lsp/cache/phpactor/index");
        fs::create_dir_all(&cache)?;
        json!({
            "indexer.index_path": cache,
            "indexer.enabled_watchers": ["lsp"],
            "language_server.diagnostics_on_update": false,
        })
    } else if language == "typescript" {
        json!({"disableAutomaticTypingAcquisition": true})
    } else {
        json!({})
    };
    process.request(
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": root.file_name().and_then(|value| value.to_str()).unwrap_or("repo")}],
            "capabilities": {
                "workspace": {"configuration": true, "workspaceFolders": true},
                "textDocument": {"references": {"dynamicRegistration": false}},
            },
            "clientInfo": {"name": "ctx", "version": env!("CARGO_PKG_VERSION")},
            "initializationOptions": initialization_options,
        }),
        timeout,
    )?;
    process.notify("initialized", json!({}), timeout)?;
    let mut contents = HashMap::new();
    for (file_id, relative, _) in files {
        let path = root.join(relative);
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        let uri = url::Url::from_file_path(&path)
            .map_err(|_| anyhow::anyhow!("URI fichier invalide: {}", path.display()))?
            .to_string();
        process.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id(&path, language),
                    "version": 1,
                    "text": text,
                }
            }),
            timeout,
        )?;
        contents.insert(*file_id, (path, text));
    }
    let file_ids = contents.keys().copied().collect::<Vec<_>>();
    if file_ids.is_empty() {
        process.close(timeout);
        return Ok((Vec::new(), 0));
    }
    let placeholders = (1..=file_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let limit_parameter = file_ids.len() + 1;
    let sql = format!(
        "SELECT s.id,s.file_id,s.name,s.start,f.path,f.is_test
         FROM symbols s JOIN files f ON f.id=s.file_id
         WHERE s.file_id IN ({placeholders}) AND s.kind!='variable'
         ORDER BY f.is_test,s.kind='method',f.path,s.start LIMIT ?{limit_parameter}"
    );
    let mut values = file_ids
        .iter()
        .copied()
        .map(rusqlite::types::Value::Integer)
        .collect::<Vec<_>>();
    values.push((max_symbols as i64).into());
    let mut statement = connection.prepare(&sql)?;
    let symbols = statement
        .query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?.max(1) as usize,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut references = Vec::new();
    for (symbol_id, file_id, name, start) in &symbols {
        let (path, text) = &contents[file_id];
        let line = start.saturating_sub(1);
        let uri = url::Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("URI fichier invalide: {}", path.display()))?
            .to_string();
        let locations = process.request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": symbol_character(text, line, name)},
                "context": {"includeDeclaration": false},
            }),
            timeout,
        )?;
        if let Some(locations) = locations.as_array() {
            for location in locations {
                if let Some((path, line)) = reference_location(root, location) {
                    references.push(Reference {
                        dst_symbol_id: *symbol_id,
                        dst_name: name.clone(),
                        path,
                        line,
                    });
                }
            }
        }
    }
    process.close(timeout);
    Ok((references, symbols.len()))
}

struct JsonRpcClient {
    child: Child,
    writer: mpsc::Sender<(Vec<u8>, mpsc::SyncSender<std::io::Result<()>>)>,
    receiver: mpsc::Receiver<Result<Value, String>>,
    pending: HashMap<i64, Value>,
    next_id: i64,
    configuration: Value,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl JsonRpcClient {
    fn start(command: &[String], cwd: &Path, configuration: Value) -> Result<Self> {
        let executable = command.first().context("commande LSP vide")?;
        let mut child = Command::new(executable)
            .args(&command[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("impossible de lancer le serveur LSP: {executable}"))?;
        let mut stdin = child.stdin.take().context("stdin LSP indisponible")?;
        let stdout = child.stdout.take().context("stdout LSP indisponible")?;
        let stderr_stream = child.stderr.take().context("stderr LSP indisponible")?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || read_messages(stdout, sender));
        let (writer, writes) = mpsc::channel::<(Vec<u8>, mpsc::SyncSender<std::io::Result<()>>)>();
        thread::spawn(move || {
            for (payload, reply) in writes {
                let result = stdin.write_all(&payload).and_then(|()| stdin.flush());
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&stderr);
        thread::spawn(move || {
            for line in BufReader::new(stderr_stream).lines().map_while(Result::ok) {
                let mut values = captured.lock().expect("stderr LSP lock");
                values.push(line);
                if values.len() > 20 {
                    values.remove(0);
                }
            }
        });
        Ok(Self {
            child,
            writer,
            receiver,
            pending: HashMap::new(),
            next_id: 1,
            configuration,
            stderr,
        })
    }

    fn notify(&mut self, method: &str, parameters: Value, timeout: Duration) -> Result<()> {
        self.write(
            &json!({"jsonrpc": "2.0", "method": method, "params": parameters}),
            timeout,
        )
    }

    fn request(&mut self, method: &str, parameters: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let deadline = Instant::now() + timeout;
        self.write(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": parameters}),
            timeout,
        )?;
        loop {
            if let Some(message) = self.pending.remove(&id) {
                return rpc_result(message, method);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timeout LSP pendant {method}");
            }
            let message = self
                .receiver
                .recv_timeout(remaining)
                .map_err(|_| anyhow::anyhow!("timeout LSP pendant {method}"))?
                .map_err(|error| {
                    let stderr = self.stderr.lock().expect("stderr LSP lock").join(" | ");
                    anyhow::anyhow!(
                        "serveur LSP arrêté pendant {method}: {}",
                        if stderr.is_empty() { error } else { stderr }
                    )
                })?;
            if message.get("method").is_some() && message.get("id").is_some() {
                self.answer_server_request(
                    &message,
                    deadline.saturating_duration_since(Instant::now()),
                )?;
            } else if let Some(response_id) = message.get("id").and_then(Value::as_i64) {
                if response_id == id {
                    return rpc_result(message, method);
                }
                self.pending.insert(response_id, message);
            }
        }
    }

    fn answer_server_request(&mut self, message: &Value, timeout: Duration) -> Result<()> {
        let response = server_request_response(message, &self.configuration);
        self.write(&response, timeout)
    }

    fn write(&mut self, message: &Value, timeout: Duration) -> Result<()> {
        let payload = serde_json::to_vec(message)?;
        let mut frame = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
        frame.extend(payload);
        let (reply, result) = mpsc::sync_channel(1);
        self.writer
            .send((frame, reply))
            .context("écriture LSP arrêtée")?;
        result
            .recv_timeout(timeout)
            .context("timeout LSP pendant écriture")??;
        Ok(())
    }

    fn close(&mut self, timeout: Duration) {
        let _ = self.request("shutdown", json!({}), timeout.min(Duration::from_secs(3)));
        let _ = self.notify("exit", json!({}), timeout.min(Duration::from_secs(3)));
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for JsonRpcClient {
    fn drop(&mut self) {
        // Also runs for failed initialization, protocol errors and write timeouts.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server_request_response(message: &Value, configuration: &Value) -> Value {
    let id = message["id"].clone();
    let method = message["method"].as_str().unwrap_or_default();
    match method {
        "workspace/configuration" => {
            let count = message["params"]["items"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            json!({"jsonrpc": "2.0", "id": id, "result": vec![configuration.clone(); count]})
        }
        "workspace/workspaceFolders" => {
            json!({"jsonrpc": "2.0", "id": id, "result": []})
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("ctx refuses server request: {method}")},
        }),
    }
}

fn read_messages(stdout: impl Read, sender: mpsc::Sender<Result<Value, String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.send(Err("EOF LSP".to_owned()));
                    return;
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    return;
                }
                Ok(_) if line == "\r\n" || line == "\n" => break,
                Ok(_) => {
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().ok();
                    }
                }
            }
        }
        let Some(length) = length else {
            continue;
        };
        let mut payload = vec![0; length];
        if let Err(error) = reader.read_exact(&mut payload) {
            let _ = sender.send(Err(error.to_string()));
            return;
        }
        match serde_json::from_slice(&payload) {
            Ok(value) => {
                if sender.send(Ok(value)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error.to_string()));
                return;
            }
        }
    }
}

fn rpc_result(message: Value, method: &str) -> Result<Value> {
    if let Some(error) = message.get("error") {
        bail!(
            "erreur LSP {method}: {}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("erreur inconnue")
        );
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

fn language_files(
    connection: &Connection,
    languages: &[&str],
) -> Result<Vec<(i64, String, String)>> {
    let placeholders = (1..=languages.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT id,path,lang FROM files WHERE lang IN ({placeholders}) ORDER BY path");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(languages), |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn store_references(
    connection: &Connection,
    language: &str,
    references: Vec<Reference>,
) -> Result<usize> {
    let languages = db_languages(language);
    let placeholders = (1..=languages.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    connection.execute(
        &format!(
            "DELETE FROM edges WHERE source='lsp'
             AND file_id IN (SELECT id FROM files WHERE lang IN ({placeholders}))"
        ),
        params_from_iter(languages),
    )?;
    let mut seen = HashSet::new();
    let mut inserted = 0;
    for reference in references {
        let file_id: Option<i64> = connection
            .query_row(
                "SELECT id FROM files WHERE path=?1",
                [&reference.path],
                |row| row.get(0),
            )
            .optional()?;
        let Some(file_id) = file_id else {
            continue;
        };
        let source: Option<i64> = connection
            .query_row(
                "SELECT id FROM symbols
                 WHERE file_id=?1 AND start<=?2 AND end>=?2
                 ORDER BY (end-start),start DESC LIMIT 1",
                params![file_id, reference.line],
                |row| row.get(0),
            )
            .optional()?;
        if !seen.insert((source, reference.dst_name.clone(), file_id, reference.line)) {
            continue;
        }
        connection.execute(
            "INSERT INTO edges(src_symbol_id,dst_name,dst_symbol_id,kind,file_id,line,source,confidence)
             VALUES(?1,?2,?3,'ref',?4,?5,'lsp',1.0)",
            params![
                source,
                reference.dst_name,
                reference.dst_symbol_id,
                file_id,
                reference.line
            ],
        )?;
        inserted += 1;
    }
    Ok(inserted)
}

fn reference_location(root: &Path, location: &Value) -> Option<(String, usize)> {
    let uri = location
        .get("uri")
        .or_else(|| location.get("targetUri"))?
        .as_str()?;
    let path = url::Url::parse(uri)
        .ok()?
        .to_file_path()
        .ok()?
        .canonicalize()
        .ok()?;
    let root = root.canonicalize().ok()?;
    let relative = path.strip_prefix(root).ok()?;
    let range = location
        .get("range")
        .or_else(|| location.get("targetSelectionRange"))?;
    let line = range.pointer("/start/line")?.as_u64()? as usize + 1;
    Some((relative.to_string_lossy().replace('\\', "/"), line))
}

fn symbol_character(text: &str, line: usize, name: &str) -> usize {
    let line = text.lines().nth(line).unwrap_or_default();
    let column = line
        .find(name)
        .unwrap_or_else(|| line.len() - line.trim_start().len());
    line[..column].encode_utf16().count()
}

fn selected_languages(languages: &[String]) -> Result<Vec<String>> {
    let selected: Vec<String> =
        if languages.is_empty() || languages.iter().any(|value| value == "all") {
            SERVERS
                .iter()
                .map(|server| server.language.to_owned())
                .collect()
        } else {
            let mut seen = HashSet::new();
            languages
                .iter()
                .filter(|language| seen.insert((*language).clone()))
                .cloned()
                .collect()
        };
    for language in &selected {
        if server(language.as_str()).is_none() {
            bail!("langage LSP inconnu: {language}");
        }
    }
    Ok(selected)
}

fn db_languages(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &["python"],
        "typescript" => &["javascript", "typescript", "tsx"],
        "go" => &["go"],
        "rust" => &["rust"],
        "php" => &["php"],
        _ => &[],
    }
}

fn language_id<'a>(path: &Path, fallback: &'a str) -> &'a str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("py" | "pyi" | "pyw") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("go") => "go",
        Some("rs") => "rust",
        Some("php" | "phtml") => "php",
        _ => match fallback {
            "python" => "python",
            "typescript" => "typescript",
            "go" => "go",
            "rust" => "rust",
            "php" => "php",
            _ => "plaintext",
        },
    }
}

fn server(language: &str) -> Option<&'static Server> {
    SERVERS.iter().find(|server| server.language == language)
}

fn normalize_command(language: &str, mut command: Vec<String>) -> Vec<String> {
    if language == "typescript"
        && command.iter().any(|value| value == "--lsp")
        && !command.iter().any(|value| value == "--stdio")
    {
        command.push("--stdio".to_owned());
    }
    command
}

fn select_asset(language: &str, assets: &[Value]) -> Result<Option<Value>> {
    let (operating_system, architecture) = platform()?;
    let names = match language {
        "python" => vec![".whl".to_owned()],
        "typescript" => vec![format!("typescript-{operating_system}-{architecture}.tgz")],
        "rust" => {
            let rust_arch = match architecture.as_str() {
                "x64" => "x86_64",
                "arm64" => "aarch64",
                "arm" => "arm",
                _ => architecture.as_str(),
            };
            let rust_os = match operating_system.as_str() {
                "linux" => "unknown-linux-gnu",
                "darwin" => "apple-darwin",
                "win32" => "pc-windows-msvc",
                _ => operating_system.as_str(),
            };
            vec![
                format!("rust-analyzer-{rust_arch}-{rust_os}.gz"),
                format!("rust-analyzer-{rust_arch}-{rust_os}.zip"),
            ]
        }
        "php" => vec!["phpactor.phar".to_owned()],
        _ => return Ok(None),
    };
    Ok(assets.iter().find_map(|asset| {
        let name = asset["name"].as_str()?;
        names
            .iter()
            .any(|wanted| name == wanted || (wanted == ".whl" && name.ends_with(wanted)))
            .then(|| asset.clone())
    }))
}

fn platform() -> Result<(String, String)> {
    let os = match env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win32",
        value => bail!(
            "plateforme LSP non prise en charge: {value}/{}",
            env::consts::ARCH
        ),
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "arm" => "arm",
        value => bail!("architecture LSP non prise en charge: {os}/{value}"),
    };
    Ok((os.to_owned(), arch.to_owned()))
}

fn install_asset(
    client: &Client,
    language: &str,
    asset: &Value,
    install_root: &Path,
) -> Result<Vec<String>> {
    let name = asset["name"].as_str().context("nom d'asset absent")?;
    let archive = install_root.join("downloads").join(name);
    download(client, asset, &archive)?;
    let bin = install_root.join("bin");
    fs::create_dir_all(&bin)?;
    match language {
        "python" => {
            let target = install_root.join("python");
            extract_zip(&archive, &target, Some("basedpyright/"))?;
            let bun = find_executable("bun", &env::var("PATH").unwrap_or_default())
                .context("Bun est requis pour exécuter BasedPyright sans npm")?;
            Ok(vec![
                bun.to_string_lossy().into_owned(),
                target
                    .join("basedpyright/langserver.index.js")
                    .to_string_lossy()
                    .into_owned(),
                "--stdio".to_owned(),
            ])
        }
        "typescript" => {
            let target = install_root.join("typescript");
            extract_tar_gz(&archive, &target, Some("package/"))?;
            let executable = target.join("lib/tsc");
            make_executable(&executable)?;
            Ok(vec![
                executable.to_string_lossy().into_owned(),
                "--lsp".to_owned(),
                "--stdio".to_owned(),
            ])
        }
        "rust" => {
            let executable = bin.join(if cfg!(windows) {
                "rust-analyzer.exe"
            } else {
                "rust-analyzer"
            });
            if name.ends_with(".gz") {
                let mut source = GzDecoder::new(File::open(&archive)?);
                let mut output = File::create(&executable)?;
                std::io::copy(&mut source, &mut output)?;
            } else {
                let mut archive = zip::ZipArchive::new(File::open(&archive)?)?;
                let index = (0..archive.len())
                    .find(|index| {
                        archive
                            .by_index(*index)
                            .ok()
                            .and_then(|entry| {
                                Path::new(entry.name())
                                    .file_name()
                                    .and_then(|value| value.to_str())
                                    .map(|name| name.starts_with("rust-analyzer"))
                            })
                            .unwrap_or(false)
                    })
                    .context("rust-analyzer absent de l'archive")?;
                let mut source = archive.by_index(index)?;
                let mut output = File::create(&executable)?;
                std::io::copy(&mut source, &mut output)?;
            }
            make_executable(&executable)?;
            Ok(vec![executable.to_string_lossy().into_owned()])
        }
        "php" => {
            let executable = bin.join("phpactor.phar");
            fs::copy(&archive, &executable)?;
            let php = find_executable("php", &env::var("PATH").unwrap_or_default())
                .context("PHP est requis pour exécuter phpactor.phar")?;
            Ok(vec![
                php.to_string_lossy().into_owned(),
                executable.to_string_lossy().into_owned(),
                "language-server".to_owned(),
            ])
        }
        _ => bail!("langage LSP inconnu: {language}"),
    }
}

fn download(client: &Client, asset: &Value, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension(format!(
        "{}.part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
    ));
    let url = asset["browser_download_url"]
        .as_str()
        .context("URL d'asset absente")?;
    let bytes = client
        .get(url)
        .header(USER_AGENT, "ctx-code")
        .send()?
        .error_for_status()?
        .bytes()?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if let Some(expected) = asset["digest"]
        .as_str()
        .and_then(|value| value.strip_prefix("sha256:"))
        && actual != expected
    {
        bail!(
            "SHA-256 GitHub invalide pour {}",
            asset["name"].as_str().unwrap_or("asset")
        );
    }
    fs::write(&temporary, &bytes)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn extract_zip(archive: &Path, target: &Path, prefix: Option<&str>) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() || prefix.is_some_and(|prefix| !entry.name().starts_with(prefix)) {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else {
            bail!("archive GitHub dangereuse: {}", entry.name());
        };
        let destination = safe_destination(target, &relative)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(destination)?;
        std::io::copy(&mut entry, &mut output)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, target: &Path, prefix: Option<&str>) -> Result<()> {
    let decoder = GzDecoder::new(File::open(archive)?);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        if prefix.is_some_and(|prefix| !path.starts_with(prefix)) {
            continue;
        }
        let relative = match prefix {
            Some(prefix) => path
                .strip_prefix(prefix.trim_end_matches('/'))?
                .to_path_buf(),
            None => path,
        };
        let destination = safe_destination(target, &relative)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&destination)?;
    }
    Ok(())
}

fn safe_destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("archive GitHub dangereuse: {}", relative.display());
    }
    Ok(root.join(relative))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn github_client() -> Result<Client> {
    Ok(Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(USER_AGENT, "ctx-code".parse()?);
            headers.insert(ACCEPT, "application/vnd.github+json".parse()?);
            headers
        })
        .timeout(Duration::from_secs(120))
        .build()?)
}

fn load_manifest(install_root: &Path) -> Map<String, Value> {
    fs::read_to_string(install_root.join("servers.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn write_manifest(install_root: &Path, manifest: &Map<String, Value>) -> Result<()> {
    fs::write(
        install_root.join("servers.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(manifest.clone()))?
        ),
    )?;
    Ok(())
}

fn find_executable(command: &str, search_path: &str) -> Option<PathBuf> {
    env::split_paths(search_path)
        .map(|directory| directory.join(command))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::{reference_location, select_asset, server_request_response, sources, status};

    #[cfg(target_os = "linux")]
    #[test]
    fn timeouts_cover_blocked_writes_and_reap_failed_servers() {
        use super::JsonRpcClient;
        use std::{
            path::Path,
            time::{Duration, Instant},
        };
        let root = tempfile::tempdir().unwrap();
        for bytes in [0, 262_144] {
            let mut client = JsonRpcClient::start(
                &[
                    "python3".to_owned(),
                    "-c".to_owned(),
                    "import time; time.sleep(30)".to_owned(),
                ],
                root.path(),
                json!({}),
            )
            .unwrap();
            let pid = client.child.id();
            let started = Instant::now();
            let error = client
                .request(
                    "initialize",
                    json!({"text": "x".repeat(bytes)}),
                    Duration::from_millis(200),
                )
                .unwrap_err();
            assert!(error.to_string().contains("timeout"));
            if bytes > 0 {
                assert!(error.to_string().contains("écriture"));
            }
            drop(client);
            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "LSP child was not reaped"
            );
        }
    }

    #[test]
    fn sources_are_pinned_to_expected_upstream_repositories() {
        let repositories = sources()
            .into_iter()
            .map(|value| {
                (
                    value["language"].as_str().unwrap().to_owned(),
                    value["repository"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(repositories["python"], "DetachHead/basedpyright");
        assert_eq!(repositories["typescript"], "microsoft/typescript-go");
        assert_eq!(repositories["go"], "golang/tools");
        assert_eq!(repositories["rust"], "rust-lang/rust-analyzer");
        assert_eq!(repositories["php"], "phpactor/phpactor");
    }

    #[test]
    fn detects_path_server() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("gopls");
        fs::write(&executable, "").unwrap();
        let value = status(temporary.path(), Some(temporary.path().to_str().unwrap())).unwrap();
        let go = value["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["language"] == "go")
            .unwrap();
        assert_eq!(go["available"], true);
        assert_eq!(go["origin"], "path");
    }

    #[test]
    fn selects_platform_release_asset() {
        let (os, arch) = super::platform().unwrap();
        let expected = format!("typescript-{os}-{arch}.tgz");
        let assets = vec![
            json!({"name": "wrong-asset.tgz"}),
            json!({"name": expected}),
        ];
        assert_eq!(
            select_asset("typescript", &assets).unwrap().unwrap()["name"],
            expected
        );
    }

    #[test]
    fn refuses_unsupported_server_requests() {
        let response = server_request_response(
            &json!({"jsonrpc": "2.0", "id": 7, "method": "window/showMessageRequest"}),
            &json!({}),
        );
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("refuses")
        );
    }

    #[test]
    fn rejects_reference_locations_outside_repository() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let location = json!({
            "uri": url::Url::from_file_path(outside.path()).unwrap().to_string(),
            "range": {"start": {"line": 0, "character": 0}},
        });
        assert!(reference_location(root.path(), &location).is_none());
    }
}
