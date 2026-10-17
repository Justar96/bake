//! The stdio MCP server the tests start: this test binary, re-executed with
//! `BAKE_MCP_SESSION_FIXTURE=server`. It reads newline-delimited JSON-RPC
//! with the standard library, independently of the client under test, and
//! appends what it sees to `$FIXTURE_DIR/events`, one line per event, so a
//! test can wait for each step.
//!
//! `FIXTURE_MODE` holds words that change it: `stall` never answers
//! `initialize`; `stubborn` ignores `SIGTERM` and the end of its input;
//! `grandchild` starts a descendant before reading anything; `fail_list`
//! answers `tools/list` with an error once `$FIXTURE_DIR/release-list`
//! exists (or after 30 s), so the test can observe it running first; `bad_schema` lists a tool whose input
//! schema is not an object; `no_tools` declares no `tools` capability.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// The variable that makes this binary a fixture process.
pub const ROLE: &str = "BAKE_MCP_SESSION_FIXTURE";

/// Runs the fixture role this process was started for, if any.
pub fn run_if_fixture() -> bool {
    match std::env::var(ROLE).as_deref() {
        Ok("server") => {
            server();
            true
        }
        Ok("sleeper") => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => false,
    }
}

fn event(dir: &Path, line: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events"))
    {
        let _ = writeln!(file, "{line}");
    }
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, message: &Value) {
    let mut out = out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn answer(out: &Out, id: &Value, result: Value) {
    send(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    );
}

fn text(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

/// The grandchildren this server started, kept, never waited for.
static GRANDCHILDREN: Mutex<Vec<std::process::Child>> = Mutex::new(Vec::new());

fn spawn_sleeper(dir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match std::process::Command::new(exe).env(ROLE, "sleeper").spawn() {
        // Never waited for: the server's tree owns it. Its handle is kept
        // for the server's life, so on Windows too its pid cannot name
        // another process while the server runs (`probe.rs`).
        Ok(child) => {
            event(dir, &format!("grandchild {}", child.id()));
            GRANDCHILDREN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(child);
        }
        Err(error) => event(dir, &format!("grandchild-failed {error}")),
    }
}

fn tools(variant: char, mode: &str) -> Value {
    let object = |properties: Value| json!({ "type": "object", "properties": properties });
    let mut tools = vec![
        json!({ "name": "env", "description": "Read a variable", "inputSchema": object(json!({ "name": { "type": "string" } })) }),
        json!({ "name": "slow", "description": "Wait", "inputSchema": object(json!({ "release": { "type": "string" } })) }),
        json!({ "name": "change", "description": "Change the tool list", "inputSchema": object(json!({})) }),
        json!({ "name": "break_list", "description": "Break the tool list", "inputSchema": object(json!({})) }),
        json!({ "name": "spawn", "description": "Start a descendant", "inputSchema": object(json!({})) }),
        json!({ "name": "fail", "description": "Fail", "inputSchema": object(json!({})) }),
        json!({ "name": "exit", "description": "Exit", "inputSchema": object(json!({})) }),
        json!({ "name": "write", "description": "Write a file", "inputSchema": object(json!({ "path": { "type": "string" } })) }),
    ];
    match variant {
        'B' => tools.push(json!({ "name": "extra", "description": "Added later", "inputSchema": {} })),
        'X' => {
            tools.push(json!({ "name": "env", "inputSchema": {} }));
        }
        _ => tools.insert(
            0,
            json!({ "name": "echo", "description": "Echo text", "inputSchema": object(json!({ "text": { "type": "string" } })) }),
        ),
    }
    if mode.contains("bad_schema") {
        tools.push(json!({ "name": "scalar", "inputSchema": { "type": "string" } }));
    }
    Value::Array(tools)
}

fn server() {
    let dir = PathBuf::from(std::env::var("FIXTURE_DIR").unwrap_or_default());
    let mode = std::env::var("FIXTURE_MODE").unwrap_or_default();
    event(&dir, &format!("pid {}", std::process::id()));
    #[cfg(unix)]
    if mode.contains("stubborn") {
        // SAFETY: setting a signal's disposition to ignore, before any
        // thread is started.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    if mode.contains("grandchild") {
        spawn_sleeper(&dir);
    }
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut variant = 'A';
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            event(&dir, "malformed");
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                event(&dir, "initialize");
                if mode.contains("stall") {
                    continue;
                }
                let capabilities = if mode.contains("no_tools") {
                    json!({})
                } else {
                    json!({ "tools": { "listChanged": true } })
                };
                answer(
                    &out,
                    &id,
                    json!({
                        "protocolVersion": params["protocolVersion"],
                        "capabilities": capabilities,
                        "serverInfo": { "name": "fixture", "version": "1" },
                    }),
                );
            }
            Some("tools/list") => {
                event(&dir, &format!("list {variant}"));
                if mode.contains("fail_list") {
                    let release = dir.join("release-list");
                    for _ in 0..3000 {
                        if release.exists() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    send(
                        &out,
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": "list failed" } }),
                    );
                } else {
                    answer(&out, &id, json!({ "tools": tools(variant, &mode) }));
                }
            }
            Some("tools/call") => {
                let name = params["name"].as_str().unwrap_or_default().to_owned();
                let arguments = &params["arguments"];
                event(&dir, &format!("call {name} {id}"));
                match name.as_str() {
                    "echo" => answer(
                        &out,
                        &id,
                        text(format!(
                            "echo: {}",
                            arguments["text"].as_str().unwrap_or_default()
                        )),
                    ),
                    "extra" => answer(&out, &id, text("extra")),
                    "env" => {
                        let name = arguments["name"].as_str().unwrap_or_default();
                        let value = std::env::var(name).unwrap_or_else(|_| "<unset>".into());
                        answer(&out, &id, text(value));
                    }
                    "slow" => {
                        // Answered only once the test creates the release
                        // file, if it names one; reading goes on meanwhile.
                        if let Some(release) = arguments["release"].as_str() {
                            let release = PathBuf::from(release);
                            let out = Arc::clone(&out);
                            let dir = dir.clone();
                            std::thread::spawn(move || {
                                while !release.exists() {
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                event(&dir, &format!("released {id}"));
                                answer(&out, &id, text("slow done"));
                            });
                        }
                    }
                    "change" | "break_list" => {
                        variant = if name == "change" { 'B' } else { 'X' };
                        send(
                            &out,
                            &json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }),
                        );
                        answer(&out, &id, text(name));
                    }
                    "spawn" => {
                        spawn_sleeper(&dir);
                        answer(&out, &id, text("spawned"));
                    }
                    "fail" => answer(
                        &out,
                        &id,
                        json!({ "content": [{ "type": "text", "text": "boom" }], "isError": true }),
                    ),
                    "exit" => {
                        answer(&out, &id, text("bye"));
                        event(&dir, "exit");
                        std::process::exit(0);
                    }
                    "write" => {
                        let path = arguments["path"].as_str().unwrap_or_default();
                        let outcome = match std::fs::write(path, "x") {
                            Ok(()) => "wrote".to_owned(),
                            Err(error) => format!("error: {error}"),
                        };
                        answer(&out, &id, text(outcome));
                    }
                    _ => send(
                        &out,
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "unknown tool" } }),
                    ),
                }
            }
            Some("notifications/cancelled") => {
                event(&dir, &format!("cancelled {}", params["requestId"]));
            }
            Some("ping") => answer(&out, &id, json!({})),
            _ => {}
        }
    }
    event(&dir, "eof");
    if mode.contains("stubborn") {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
