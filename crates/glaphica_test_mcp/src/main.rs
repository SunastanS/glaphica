use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const JSONRPC_VERSION: &str = "2.0";
const BRIDGE_POLL_INTERVAL_MS: u64 = 200;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeRequest {
    pub id: String,
    pub workspace_root: String,
    pub record: String,
    pub exit_after_ms: Option<u64>,
    pub detached: bool,
    pub debug_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeResponse {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("glaphica_test_mcp failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let workspace_root = std::env::current_dir().map_err(|error| error.to_string())?;
    if let Some(flag) = args.first().map(String::as_str) {
        if flag == "--bridge-daemon" {
            return run_bridge_daemon(&workspace_root);
        }
        if flag == "--help" || flag == "-h" {
            println!("glaphica_test_mcp");
            println!("  default mode: MCP server over stdio");
            println!("  daemon mode : --bridge-daemon");
            return Ok(());
        }
        eprintln!("glaphica_test_mcp: ignoring unknown startup arg: {flag}");
    }
    run_mcp_server(&workspace_root)
}

fn run_mcp_server(workspace_root: &Path) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        let Some((mode, message)) = read_message(&mut reader).map_err(|error| error.to_string())?
        else {
            return Ok(());
        };

        let Some(request_obj) = message.as_object() else {
            continue;
        };
        let Some(method) = request_obj.get("method").and_then(Value::as_str) else {
            continue;
        };

        let id = request_obj.get("id").cloned();
        if id.is_none() {
            continue;
        }
        let id = id.unwrap_or(Value::Null);

        let params = request_obj
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let response = match method {
            "initialize" => Ok(initialize_result()),
            "tools/list" => Ok(tools_list_result()),
            "tools/call" => tools_call_result(workspace_root, &params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        let payload = match response {
            Ok(result) => json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "result": result,
            }),
            Err((code, message)) => json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "error": {
                    "code": code,
                    "message": message,
                }
            }),
        };

        write_message(&mut writer, mode, &payload).map_err(|error| error.to_string())?;
    }
}

fn run_bridge_daemon(workspace_root: &Path) -> Result<(), String> {
    let bridge_root = bridge_root(workspace_root);
    let requests_dir = bridge_root.join("requests");
    let processing_dir = bridge_root.join("processing");
    let responses_dir = bridge_root.join("responses");
    let logs_dir = bridge_root.join("logs");
    fs::create_dir_all(&requests_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&processing_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&responses_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&logs_dir).map_err(|error| error.to_string())?;

    loop {
        let entries = match fs::read_dir(&requests_dir) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("bridge daemon read_dir failed: {error}");
                thread::sleep(Duration::from_millis(BRIDGE_POLL_INTERVAL_MS));
                continue;
            }
        };

        let mut processed_any = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("bridge daemon read_dir entry failed: {error}");
                    continue;
                }
            };
            let request_path = entry.path();
            if request_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(file_name) = request_path.file_name().map(|name| name.to_owned()) else {
                continue;
            };
            let processing_path = processing_dir.join(file_name);
            if fs::rename(&request_path, &processing_path).is_err() {
                continue;
            }
            processed_any = true;

            let response = match read_json_file::<BridgeRequest>(&processing_path) {
                Ok(request) => handle_bridge_request(workspace_root, &bridge_root, request),
                Err(error) => BridgeResponse {
                    id: "unknown".to_string(),
                    ok: false,
                    message: format!("invalid bridge request file: {error}"),
                },
            };

            let response_path =
                responses_dir.join(format!("{}.json", sanitize_for_filename(&response.id)));
            if let Err(error) = write_json_file(&response_path, &response) {
                eprintln!(
                    "bridge daemon failed writing response {}: {error}",
                    response_path.display()
                );
            }
            if let Err(error) = fs::remove_file(&processing_path) {
                eprintln!(
                    "bridge daemon failed removing processing file {}: {error}",
                    processing_path.display()
                );
            }
        }

        if !processed_any {
            thread::sleep(Duration::from_millis(BRIDGE_POLL_INTERVAL_MS));
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MessageMode {
    ContentLength,
    JsonLine,
}

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<(MessageMode, Value)>> {
    let mut content_length = None;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            if content_length.is_none() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF in message headers",
            ));
        }

        let trimmed = line.trim();
        if trimmed.starts_with('{') {
            return serde_json::from_str(trimmed)
                .map(|value| Some((MessageMode::JsonLine, value)))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()));
        }

        if trimmed.is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            let parsed = value.trim().parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid Content-Length header: {error}"),
                )
            })?;
            content_length = Some(parsed);
        }
    }

    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Content-Length header",
        ));
    };

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(|value| Some((MessageMode::ContentLength, value)))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

fn write_message<W: Write>(writer: &mut W, mode: MessageMode, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match mode {
        MessageMode::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
            writer.write_all(&body)?;
            writer.flush()
        }
        MessageMode::JsonLine => {
            writer.write_all(&body)?;
            writer.write_all(b"\n")?;
            writer.flush()
        }
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "glaphica-test-mcp",
            "version": "0.2.0"
        }
    })
}

fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "replay_record_gui",
                "description": "Submit replay request to external bridge daemon and run GUI replay there",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "record": {
                            "type": "string",
                            "description": "File name or relative path under test/records, such as draw_a_circle_input.json"
                        },
                        "exit_after_ms": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional hard timeout in milliseconds"
                        },
                        "detached": {
                            "type": "boolean",
                            "description": "If true, daemon starts replay and returns immediately"
                        },
                        "debug_lines": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 200,
                            "description": "How many trailing stdout/stderr lines to return on failure in sync mode"
                        },
                        "bridge_timeout_ms": {
                            "type": "integer",
                            "minimum": 1000,
                            "description": "Timeout waiting for bridge daemon response"
                        }
                    },
                    "required": ["record"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

fn tools_call_result(
    workspace_root: &Path,
    params: &Map<String, Value>,
) -> Result<Value, (i64, String)> {
    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "missing tools/call param: name".to_string()))?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    match tool_name {
        "replay_record_gui" => Ok(replay_record_gui(workspace_root, &args)),
        _ => Err((-32602, format!("unknown tool: {tool_name}"))),
    }
}

fn replay_record_gui(workspace_root: &Path, args: &Map<String, Value>) -> Value {
    let Some(record) = args.get("record").and_then(Value::as_str) else {
        return tool_error("missing required argument: record");
    };
    if !is_safe_relative_path(record) {
        return tool_error("record must be a safe relative path under test/records");
    }
    let replay_file = workspace_root.join("test").join("records").join(record);
    if !replay_file.is_file() {
        return tool_error(format!("record file not found: {}", replay_file.display()));
    }

    let request = BridgeRequest {
        id: next_request_id(),
        workspace_root: workspace_root.display().to_string(),
        record: record.to_string(),
        exit_after_ms: args.get("exit_after_ms").and_then(Value::as_u64),
        detached: args
            .get("detached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        debug_lines: args
            .get("debug_lines")
            .and_then(Value::as_u64)
            .unwrap_or(60)
            .clamp(1, 200) as usize,
    };
    let bridge_timeout_ms = args
        .get("bridge_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(120_000);

    match submit_bridge_request(workspace_root, &request, bridge_timeout_ms) {
        Ok(response) if response.ok => tool_text(response.message),
        Ok(response) => tool_error(response.message),
        Err(error) => tool_error(error),
    }
}

fn submit_bridge_request(
    workspace_root: &Path,
    request: &BridgeRequest,
    timeout_ms: u64,
) -> Result<BridgeResponse, String> {
    let bridge_root = bridge_root(workspace_root);
    let requests_dir = bridge_root.join("requests");
    let responses_dir = bridge_root.join("responses");
    fs::create_dir_all(&requests_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&responses_dir).map_err(|error| error.to_string())?;

    let request_path = requests_dir.join(format!("{}.json", sanitize_for_filename(&request.id)));
    write_json_file(&request_path, request)?;

    let response_path = responses_dir.join(format!("{}.json", sanitize_for_filename(&request.id)));
    let timeout = Duration::from_millis(timeout_ms);
    let started = SystemTime::now();
    loop {
        if response_path.is_file() {
            let response = read_json_file::<BridgeResponse>(&response_path)?;
            fs::remove_file(&response_path).map_err(|error| error.to_string())?;
            return Ok(response);
        }
        let elapsed = match started.elapsed() {
            Ok(elapsed) => elapsed,
            Err(_) => Duration::from_millis(0),
        };
        if elapsed >= timeout {
            return Err(format!(
                "bridge daemon timed out after {}ms; request_id={}; bridge_dir={}",
                timeout_ms,
                request.id,
                bridge_root.display()
            ));
        }
        thread::sleep(Duration::from_millis(BRIDGE_POLL_INTERVAL_MS));
    }
}

fn handle_bridge_request(
    workspace_root: &Path,
    bridge_root: &Path,
    request: BridgeRequest,
) -> BridgeResponse {
    let request_workspace = PathBuf::from(&request.workspace_root);
    if request_workspace != workspace_root {
        return BridgeResponse {
            id: request.id,
            ok: false,
            message: format!(
                "workspace root mismatch: daemon={} request={}",
                workspace_root.display(),
                request_workspace.display()
            ),
        };
    }
    if !is_safe_relative_path(&request.record) {
        return BridgeResponse {
            id: request.id,
            ok: false,
            message: "record path is not a safe relative path".to_string(),
        };
    }
    let replay_file = workspace_root
        .join("test")
        .join("records")
        .join(&request.record);
    if !replay_file.is_file() {
        return BridgeResponse {
            id: request.id,
            ok: false,
            message: format!("record file not found: {}", replay_file.display()),
        };
    }

    let (command_path, command_args) =
        build_replay_command(workspace_root, &replay_file, request.exit_after_ms);
    let command_text = format!("{} {}", command_path.display(), command_args.join(" "));

    if request.detached {
        let logs_dir = bridge_root.join("logs");
        if let Err(error) = fs::create_dir_all(&logs_dir) {
            return BridgeResponse {
                id: request.id,
                ok: false,
                message: format!(
                    "failed creating bridge logs dir {}: {error}",
                    logs_dir.display()
                ),
            };
        }
        let log_path = logs_dir.join(format!("{}.log", sanitize_for_filename(&request.id)));
        let stdout_file = match File::create(&log_path) {
            Ok(file) => file,
            Err(error) => {
                return BridgeResponse {
                    id: request.id,
                    ok: false,
                    message: format!(
                        "failed creating bridge log file {}: {error}",
                        log_path.display()
                    ),
                };
            }
        };
        let stderr_file = match stdout_file.try_clone() {
            Ok(file) => file,
            Err(error) => {
                return BridgeResponse {
                    id: request.id,
                    ok: false,
                    message: format!(
                        "failed cloning bridge log file handle {}: {error}",
                        log_path.display()
                    ),
                };
            }
        };
        let mut command = Command::new(&command_path);
        command
            .current_dir(workspace_root)
            .args(&command_args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return BridgeResponse {
                    id: request.id,
                    ok: false,
                    message: format!(
                        "failed starting detached replay: {error}; command={command_text}"
                    ),
                };
            }
        };
        return BridgeResponse {
            id: request.id,
            ok: true,
            message: format!(
                "started replay process pid={} command={} logs={}",
                child.id(),
                command_text,
                log_path.display()
            ),
        };
    }

    let mut command = Command::new(&command_path);
    command
        .current_dir(workspace_root)
        .args(&command_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return BridgeResponse {
                id: request.id,
                ok: false,
                message: format!("failed running replay: {error}; command={command_text}"),
            };
        }
    };
    if output.status.success() {
        return BridgeResponse {
            id: request.id,
            ok: true,
            message: format!(
                "replay finished successfully record={}",
                replay_file.display()
            ),
        };
    }
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    BridgeResponse {
        id: request.id,
        ok: false,
        message: format!(
            "replay process failed status={}; command={}; stdout_tail:\n{}\nstderr_tail:\n{}",
            output.status,
            command_text,
            tail_lines(&stdout_text, request.debug_lines),
            tail_lines(&stderr_text, request.debug_lines)
        ),
    }
}

fn build_replay_command(
    workspace_root: &Path,
    replay_file: &Path,
    exit_after_ms: Option<u64>,
) -> (PathBuf, Vec<String>) {
    let release_bin = workspace_root
        .join("target")
        .join("release")
        .join("glaphica");
    if release_bin.is_file() {
        let mut args = vec![
            "--replay-input".to_string(),
            replay_file.display().to_string(),
        ];
        if let Some(ms) = exit_after_ms {
            args.push("--exit-after-ms".to_string());
            args.push(ms.to_string());
        }
        return (release_bin, args);
    }

    let mut args = vec![
        "run".to_string(),
        "-p".to_string(),
        "glaphica".to_string(),
        "--".to_string(),
        "--replay-input".to_string(),
        replay_file.display().to_string(),
    ];
    if let Some(ms) = exit_after_ms {
        args.push("--exit-after-ms".to_string());
        args.push(ms.to_string());
    }
    (PathBuf::from("cargo"), args)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice::<T>(&bytes).map_err(|error| error.to_string())
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temp_path = path.with_extension("json.tmp");
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    fs::write(&temp_path, body).map_err(|error| error.to_string())?;
    fs::rename(&temp_path, path).map_err(|error| error.to_string())
}

fn bridge_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("test").join("mcp_bridge")
}

fn next_request_id() -> String {
    format!(
        "r-{}-{}",
        now_unix_ms(),
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn sanitize_for_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn is_safe_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return false;
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return false;
        }
    }
    true
}

fn tool_text(text: String) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ]
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": message.into()
            }
        ]
    })
}

fn tail_lines(text: &str, max_lines: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return "<empty>".to_string();
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn now_unix_ms() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis(),
        Err(_) => 0,
    }
}
