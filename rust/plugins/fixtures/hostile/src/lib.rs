//! A hostile test plugin for Bake's Wasm plugin host tests. Each tool name
//! selects one misbehavior; the host tests call it through the real helper
//! process and check that the host contains it. It is a test fixture, never
//! a plugin to enable.
//!
//! | Tool name | Behavior |
//! |---|---|
//! | `write`, `edit` | Answers `none` (the positive control) |
//! | `spin` | Loops forever without host calls |
//! | `grow` | Allocates and touches `args.mib` MiB of linear memory |
//! | `flood-output` | Writes 4 MiB to stdout and to stderr |
//! | `log-alias` | Logs one 4 KiB string `args.times` times |
//! | `log-huge` | Logs one line of `args.bytes` bytes |
//! | `reason` | Blocks with a reason of `args.bytes` bytes |
//! | `ambient` | Tries ambient authority and blocks with a JSON report |
//! | `sleep` | Logs `before`, sleeps an hour through WASI, logs `after` |
//! | `error` | Answers with an error |
//! | `panic` | Panics, which traps |

// Only a Wasm build exports the plugin; elsewhere the crate is checked and
// unit-tested but nothing calls it.
#![cfg_attr(not(target_family = "wasm"), allow(dead_code))]

use std::io::{Read, Write};

use bake_plugin_sdk::{BlockInfo, Decision, LogLevel, Plugin, ToolCall, log};
use serde_json::{Value, json};

struct Hostile;

impl Plugin for Hostile {
    fn on_tool_call(call: ToolCall) -> Result<Decision, String> {
        let args: Value = serde_json::from_str(&call.args_json).map_err(|e| e.to_string())?;
        let number = |key: &str| args.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
        match call.name.as_str() {
            "spin" => {
                let mut counter = 0u64;
                loop {
                    counter = std::hint::black_box(counter.wrapping_add(1));
                }
            }
            "grow" => {
                let mut memory = vec![0u8; number("mib") << 20];
                for page in memory.chunks_mut(64 * 1024) {
                    page[0] = 1;
                }
                block(format!("grew {}", std::hint::black_box(memory).len()))
            }
            "flood-output" => {
                // 4 MiB per stream in 4 KiB writes; each stream reports the
                // bytes accepted and its first error.
                let chunk = [b'x'; 4096];
                let mut report = Vec::new();
                for stream in [
                    &mut std::io::stdout() as &mut dyn Write,
                    &mut std::io::stderr(),
                ] {
                    let mut written = 0usize;
                    let mut error = None;
                    for _ in 0..1024 {
                        match stream.write_all(&chunk).and_then(|()| stream.flush()) {
                            Ok(()) => written += chunk.len(),
                            Err(e) => {
                                error.get_or_insert(e.to_string());
                            }
                        }
                    }
                    report.push(json!({ "written": written, "error": error }));
                }
                block(json!({ "stdout": report[0], "stderr": report[1] }).to_string())
            }
            "log-alias" => {
                // One allocation; every call lifts the same guest bytes.
                let line = "a".repeat(4096);
                for _ in 0..number("times") {
                    log(LogLevel::Info, &line);
                }
                block("logged".to_owned())
            }
            "log-huge" => {
                log(LogLevel::Info, &"h".repeat(number("bytes")));
                block("logged".to_owned())
            }
            "reason" => block("r".repeat(number("bytes"))),
            "ambient" => block(ambient(&args).to_string()),
            "sleep" => {
                log(LogLevel::Info, "before");
                std::thread::sleep(std::time::Duration::from_secs(3600));
                log(LogLevel::Info, "after");
                block("slept".to_owned())
            }
            "error" => Err("hostile error".to_owned()),
            "panic" => panic!("hostile panic"),
            _ => Ok(Decision::None),
        }
    }
}

fn block(reason: String) -> Result<Decision, String> {
    Ok(Decision::Block(BlockInfo {
        reason,
        terminate: false,
    }))
}

/// Tries each ambient authority a native process would have; every entry
/// is the error text, or `"ok"` when the attempt succeeded.
fn ambient(args: &Value) -> Value {
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("");
    let outcome = |result: std::io::Result<String>| match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "err": error.to_string() }),
    };
    let mut stdin = String::new();
    json!({
        "read_absolute": outcome(std::fs::read_to_string(text("path"))),
        "read_relative": outcome(std::fs::read_to_string("Cargo.toml")),
        "list_root": outcome(std::fs::read_dir("/").map(|entries| entries.count().to_string())),
        "create_file": outcome(std::fs::write("created-by-plugin", b"x").map(|()| String::new())),
        "connect": outcome(std::net::TcpStream::connect(text("addr")).map(|_| String::new())),
        "stdin": outcome(std::io::stdin().read_to_string(&mut stdin).map(|n| n.to_string())),
        "env": std::env::vars().count(),
        "args": std::env::args().count(),
        "cwd": outcome(std::env::current_dir().map(|dir| dir.display().to_string())),
        "spawn": outcome(std::process::Command::new("/bin/sh").output().map(|_| String::new())),
    })
}

// Exported only for Wasm: on the host, where the workspace's tests build
// every crate, the component's export names are not valid linker symbols.
#[cfg(target_family = "wasm")]
bake_plugin_sdk::export_plugin!(Hostile);
