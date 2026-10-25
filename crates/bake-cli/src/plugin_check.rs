//! `bake plugin check <dir> [<tool> <args-json>]`: a diagnostic that
//! starts one Wasm plugin package in its helper process with the default
//! plugin limits, prints what the helper established, optionally asks the
//! plugin about one tool call, and shuts the helper down. It loads nothing
//! into any session and reads no Bake home. `plugin install` runs the same
//! check ([`check_package`]) before it stores a package.
//!
//! The contract, which the process tests check against the built binary:
//!
//! - Standard output gets one line per fact as it happens: the manifest,
//!   `helper ready: … (pid N)`, `calling <tool> (request N)` once the
//!   helper is running the call, each staged log line, then `continue` or
//!   `block: <reason>`. Every plugin- or helper-supplied text, on either
//!   stream, has its control characters escaped.
//! - Exit 0 only when the plugin started, answered (if asked), and the
//!   helper then exited by itself with status 0 and was reaped with its
//!   pipes closed.
//! - Exit 1 with `Error: …` on standard error for any other outcome: a bad
//!   package, a start or call failure, or a shutdown that was not clean or
//!   did not finish. A teardown that did not finish is retried once, then
//!   reported.
//! - Ctrl+C cancels the start or call through its signal, so the helper is
//!   killed and torn down before the command exits with 130.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use bake_ai::{AbortController, AbortSignal};
use bake_coding_agent::plugins::ToolCallVeto;
use bake_coding_agent::plugins::ToolCallView;
use bake_coding_agent::plugins::wasm::{
    CallError, CompileQueue, ExitReport, HelperCommand, HostLimits, HostRequest, HostStatus,
    LimitMechanism, LogLevel, StartError, WasmPackage, WasmPluginHost,
};

/// A tool call to ask the plugin about.
#[derive(Debug, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub args: serde_json::Value,
}

/// Parses `<tool> <args-json>`; the arguments must be a JSON object, as a
/// tool's validated arguments are. `serde_json` refuses nesting past 128
/// levels while parsing.
pub fn parse_call(tool: &str, args: &str) -> Result<Call, String> {
    let args: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("the arguments for 'plugin check' are not JSON: {error}"))?;
    if !args.is_object() {
        return Err("the arguments for 'plugin check' must be a JSON object".to_owned());
    }
    Ok(Call {
        tool: tool.to_owned(),
        args,
    })
}

/// How the check ended.
pub enum Failure {
    /// Ctrl+C.
    Interrupted,
    /// Anything else, with its (escaped) message.
    Error(String),
}

/// Runs the check: 0 on a clean run, 130 after Ctrl+C, 1 otherwise.
pub fn run(dir: &Path, call: Option<&Call>) -> ExitCode {
    exit(check(dir, call))
}

/// The exit status for `result`, reporting a failure on standard error:
/// 0, 130 after Ctrl+C, or 1 with an `Error:` line.
pub fn exit(result: Result<(), Failure>) -> ExitCode {
    let (code, message) = match result {
        Ok(()) => return ExitCode::SUCCESS,
        Err(Failure::Interrupted) => (130, "interrupted".to_owned()),
        Err(Failure::Error(message)) => (1, message),
    };
    // A closed standard error is not a panic.
    let _ = std::io::stderr()
        .lock()
        .write_all(format!("Error: {message}\n").as_bytes());
    ExitCode::from(code)
}

/// Writes one line to standard output at once; a closed output is a
/// failure.
pub fn line(text: &str) -> Result<(), Failure> {
    if crate::write_stdout(&format!("{text}\n")) {
        Ok(())
    } else {
        Err(Failure::Error("cannot write to standard output".to_owned()))
    }
}

fn check(dir: &Path, call: Option<&Call>) -> Result<(), Failure> {
    let package = WasmPackage::read_dir(dir).map_err(|error| fail(&error.to_string()))?;
    check_package(package, call)
}

/// Starts `package` in a helper, as `plugin check` does, optionally asks it
/// about `call`, and requires a clean shutdown.
pub fn check_package(package: WasmPackage, call: Option<&Call>) -> Result<(), Failure> {
    let plugin = package.manifest().plugin();
    line(&format!(
        "plugin {} {} (bake:plugin@{}), vetoes {}",
        plugin.id().as_str(),
        plugin.version(),
        package.manifest().api(),
        plugin.veto_tools().collect::<Vec<_>>().join(", ")
    ))?;
    let command = HelperCommand::current_exe()
        .map_err(|error| fail(&format!("cannot find this executable: {error}")))?;
    let host = WasmPluginHost::new(HostRequest {
        command,
        package,
        limits: HostLimits::default(),
        queue: CompileQueue::new(0),
        generation: 1,
        status: None,
    })
    .map_err(|error| fail(&error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| fail(&format!("cannot start the runtime: {error}")))?;
    runtime.block_on(async {
        let interrupt = AbortController::new();
        let signal = interrupt.signal();
        // Ctrl+C cancels through the signal, so the work tears the helper
        // down before it returns rather than being dropped.
        let mut work = std::pin::pin!(work(&host, call, &signal));
        let result = tokio::select! {
            result = &mut work => result,
            Ok(()) = tokio::signal::ctrl_c() => {
                interrupt.abort();
                work.await
            }
        };
        let result = if interrupt.signal().aborted() {
            Err(Failure::Interrupted)
        } else {
            result
        };
        // The work's own teardown may be unfinished, and a successful run
        // still owns a running helper: shut down, and once more if the
        // first wait ran out.
        let mut exit = host.shutdown().await;
        if !exit.is_complete() {
            exit = host.shutdown().await;
        }
        match result {
            Err(failure) if exit.is_complete() => Err(failure),
            Err(Failure::Error(message)) => Err(Failure::Error(format!(
                "{message}; and the helper's teardown did not finish: {}",
                describe(&exit)
            ))),
            Err(Failure::Interrupted) => Err(Failure::Error(format!(
                "interrupted, and the helper's teardown did not finish: {}",
                describe(&exit)
            ))),
            Ok(()) if clean(&exit) => Ok(()),
            Ok(()) => Err(Failure::Error(format!(
                "the plugin helper did not shut down cleanly: {}",
                describe(&exit)
            ))),
        }
    })
}

async fn work(
    host: &WasmPluginHost,
    call: Option<&Call>,
    signal: &AbortSignal,
) -> Result<(), Failure> {
    host.initialize(Some(signal))
        .await
        .map_err(|error| start_failure(&error))?;
    if let Some(limit) = host.memory_limit() {
        line(&format!(
            "helper ready: {} limit of {} MiB ({} MiB budget) (pid {})",
            match limit.mechanism {
                LimitMechanism::AddressSpace => "address-space",
                LimitMechanism::JobCommit => "committed-memory",
            },
            limit.limit >> 20,
            limit.budget >> 20,
            host.pid()
                .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string())
        ))?;
    }
    if let Some(call) = call {
        ask(host, call, signal).await?;
    }
    Ok(())
}

fn start_failure(error: &StartError) -> Failure {
    match error {
        StartError::Cancelled { .. } => Failure::Interrupted,
        other => fail(&other.to_string()),
    }
}

async fn ask(host: &WasmPluginHost, call: &Call, signal: &AbortSignal) -> Result<(), Failure> {
    let view = ToolCallView {
        tool_call_id: "plugin-check",
        tool_name: &call.tool,
        args: &call.args,
    };
    // `calling <tool> (request N)` is printed once the helper has started
    // running the call, so a reader knows the call is in flight.
    let mut status = host.status();
    let mut pending = std::pin::pin!(host.on_tool_call(view, Some(signal)));
    let result = tokio::select! {
        result = &mut pending => result,
        started = status.wait_for(|state| matches!(state, HostStatus::Calling(_))) => {
            // Copy the state out: the watch's read guard must not be held
            // while the call runs and the host updates its status.
            let started = started.map(|state| *state).ok();
            if let Some(HostStatus::Calling(id)) = started {
                line(&format!("calling {} (request {id})", escape(&call.tool)))?;
            }
            pending.await
        }
    };
    let checked = result.map_err(|error| match error {
        CallError::Cancelled { .. } => Failure::Interrupted,
        other => fail(&other.to_string()),
    })?;
    for entry in &checked.logs {
        let level = match entry.level {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        };
        line(&format!("log {level}: {}", escape(&entry.text)))?;
    }
    if checked.dropped_logs > 0 {
        line(&format!("log lines dropped: {}", checked.dropped_logs))?;
    }
    match checked.veto {
        ToolCallVeto::Continue => line("continue"),
        ToolCallVeto::Block { reason, terminate } => line(&format!(
            "block{}: {}",
            if terminate { " and terminate" } else { "" },
            escape(&reason)
        )),
    }
}

/// A failure whose text, which may quote the plugin or the helper, is
/// escaped for the terminal.
pub fn fail(message: &str) -> Failure {
    Failure::Error(escape(message))
}

/// Whether the helper exited by itself with status 0, was reaped, and its
/// pipes closed.
fn clean(exit: &ExitReport) -> bool {
    exit.spawned && !exit.killed && exit.code == Some(0) && exit.is_complete()
}

fn describe(exit: &ExitReport) -> String {
    escape(&format!(
        "killed {}, code {:?}, signal {:?}, reaped {}, pipes closed {}{}",
        exit.killed,
        exit.code,
        exit.signal,
        exit.reaped,
        exit.drained,
        if exit.stderr.is_empty() {
            String::new()
        } else {
            format!(", stderr {:?}", exit.stderr)
        }
    ))
}

/// Text with control characters escaped, so it cannot drive the terminal.
pub fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_need_an_object() {
        assert_eq!(
            parse_call("write", r#"{"path":".env"}"#),
            Ok(Call {
                tool: "write".to_owned(),
                args: serde_json::json!({ "path": ".env" })
            })
        );
        assert!(
            parse_call("write", "[1]")
                .unwrap_err()
                .contains("JSON object")
        );
        assert!(parse_call("write", "{").unwrap_err().contains("not JSON"));
        // serde_json's own depth limit refuses deep arguments while parsing.
        let deep = format!("{{\"a\":{}1{}}}", "[".repeat(200), "]".repeat(200));
        assert!(parse_call("write", &deep).unwrap_err().contains("not JSON"));
    }

    #[test]
    fn plugin_text_cannot_drive_the_terminal() {
        assert_eq!(escape("a\u{1b}[2Jb\n"), "a\\u{1b}[2Jb\\n");
        let Failure::Error(message) = fail("the plugin failed: \u{1b}[31mred\u{7}") else {
            panic!("an error");
        };
        assert_eq!(message, "the plugin failed: \\u{1b}[31mred\\u{7}");
    }

    #[test]
    fn only_a_self_exited_reaped_and_drained_helper_is_clean() {
        let done = ExitReport {
            spawned: true,
            code: Some(0),
            reaped: true,
            drained: true,
            ..ExitReport::default()
        };
        assert!(clean(&done));
        assert!(!clean(&ExitReport {
            killed: true,
            ..done.clone()
        }));
        assert!(!clean(&ExitReport {
            code: Some(1),
            ..done.clone()
        }));
        assert!(!clean(&ExitReport {
            drained: false,
            ..done.clone()
        }));
        assert!(!clean(&ExitReport {
            reaped: false,
            code: None,
            ..done
        }));
        assert!(!clean(&ExitReport::default()));
    }
}
