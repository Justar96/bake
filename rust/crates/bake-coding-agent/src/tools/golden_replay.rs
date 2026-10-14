//! Pi's grep and find formatting replayed on the `rg --json` and `fd`
//! output recorded beside Pi's results in
//! `tests/fixtures/pi-tools/golden.json`, without running either binary.
//! Follows the grep and find cases of Pi's `test/tools.test.ts`.

#[path = "../../tests/pi_tools/support.rs"]
mod support;

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::{Value, json};
use support::{golden, scrub, setup, substitute};

use crate::tools::find::{FindCollector, fd_args};
use crate::tools::grep::{GrepFormatter, GrepRequest};
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::process::{Ended, Flow};

fn result_json(outcome: Result<bake_agent::AgentToolResult, String>, dir: &str) -> Value {
    match outcome {
        Ok(result) => json!({
            "result": scrub(&json!({
                "content": result.content,
                "details": result.details.unwrap_or(Value::Null),
            }), dir),
        }),
        Err(error) => json!({ "error": scrub(&json!(error), dir) }),
    }
}

fn expected(case: &Value) -> Value {
    let mut expected = json!({});
    for key in ["result", "error"] {
        if let Some(value) = case.get(key) {
            expected[key] = value.clone();
        }
    }
    expected
}

fn argv(args: &[OsString], dir: &str) -> Value {
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    scrub(&json!(args), dir)
}

/// Splits recorded output as `readline` does.
fn lines(stdout: &str) -> Vec<Vec<u8>> {
    let normalized = stdout.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<Vec<u8>> = normalized
        .split('\n')
        .map(|line| line.as_bytes().to_vec())
        .collect();
    if lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

fn ended(case: &Value, stopped: bool) -> Ended {
    Ended::Exited {
        code: case["code"].as_i64().map(|code| code as i32),
        stderr: case["stderr"].as_str().unwrap_or_default().to_owned(),
        stopped,
    }
}

#[cfg(unix)]
#[test]
fn grep_formats_recorded_rg_output_as_pi() {
    let mut failures = Vec::new();
    for case in golden()["grep"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let dir = setup(&case["files"], name);
        let dir_text = dir.text();
        let args = substitute(&case["args"], &dir_text);
        let path = args["path"].as_str().unwrap_or(".");
        let search_path = resolve_to_cwd(path, dir.path()).expect("path");
        let Ok(metadata) = std::fs::metadata(&search_path) else {
            // Pi stops before running rg.
            assert_eq!(
                case["error"],
                json!(format!("Path not found: <DIR>/{path}"))
            );
            continue;
        };
        let request = GrepRequest {
            pattern: args["pattern"].as_str().expect("pattern").to_owned(),
            search_path,
            is_directory: metadata.is_dir(),
            glob: args["glob"].as_str().map(str::to_owned),
            ignore_case: args["ignoreCase"].as_bool().unwrap_or(false),
            literal: args["literal"].as_bool().unwrap_or(false),
            context: args["context"].as_f64().filter(|c| *c > 0.0).unwrap_or(0.0),
            limit: args["limit"].as_f64().unwrap_or(100.0).max(1.0),
        };
        assert_eq!(
            argv(&request.args(), &dir_text),
            case["argv"],
            "{name} argv"
        );
        let stdout = substitute(&case["stdout"], &dir_text);
        let mut formatter = GrepFormatter::new(request);
        let mut stopped = false;
        for line in lines(stdout.as_str().expect("stdout")) {
            if formatter.line(&line, None) == Flow::Stop {
                stopped = true;
                break;
            }
        }
        let mine = result_json(formatter.finish(ended(case, stopped)), &dir_text);
        if mine != expected(case) {
            failures.push(format!(
                "{name}:\n  bake: {mine}\n  pi:   {}",
                expected(case)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(unix)]
#[test]
fn find_formats_recorded_fd_output_as_pi() {
    let mut failures = Vec::new();
    for case in golden()["find"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let dir = setup(&case["files"], name);
        let dir_text = dir.text();
        let args = substitute(&case["args"], &dir_text);
        let search_path =
            resolve_to_cwd(args["path"].as_str().unwrap_or("."), dir.path()).expect("path");
        let limit = args["limit"].as_f64().unwrap_or(1000.0);
        let pattern = args["pattern"].as_str().expect("pattern");
        assert_eq!(
            argv(&fd_args(pattern, &search_path, limit), &dir_text),
            case["argv"],
            "{name} argv"
        );
        let stdout = substitute(&case["stdout"], &dir_text);
        let mut collector = FindCollector::new(search_path, limit);
        for line in lines(stdout.as_str().expect("stdout")) {
            collector.line(&line);
        }
        let mine = result_json(collector.finish(ended(case, false)), &dir_text);
        if mine != expected(case) {
            failures.push(format!(
                "{name}:\n  bake: {mine}\n  pi:   {}",
                expected(case)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A run the signal aborts reports Pi's abort message whatever the output.
#[test]
fn aborted_runs_reject() {
    let request = GrepRequest {
        pattern: "x".into(),
        search_path: PathBuf::from("/"),
        is_directory: true,
        glob: None,
        ignore_case: false,
        literal: false,
        context: 0.0,
        limit: 1.0,
    };
    assert_eq!(
        GrepFormatter::new(request)
            .finish(Ended::Aborted)
            .err()
            .as_deref(),
        Some("Operation aborted")
    );
    let collector = FindCollector::new(PathBuf::from("/"), 1.0);
    assert_eq!(
        collector.finish(Ended::Aborted).err().as_deref(),
        Some("Operation aborted")
    );
}
