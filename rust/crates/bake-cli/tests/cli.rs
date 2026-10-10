//! Runs the built binary with piped, non-terminal standard streams.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bake-rs"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("bake-rs runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn help_and_version_work_without_a_terminal() {
    for args in [&[][..], &["--help"], &["help"]] {
        let out = run(args);
        assert!(out.status.success());
        assert!(text(&out.stdout).contains("bake-rs preview"));
        assert!(out.stderr.is_empty());
    }
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout),
        format!("bake-rs {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn preview_refuses_non_terminal_streams_before_writing_any_output() {
    let out = run(&["preview"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "no terminal sequences on stdout");
    assert!(text(&out.stderr).contains("needs an interactive terminal"));
}

#[test]
fn unexpected_arguments_fail_with_usage_guidance() {
    let out = run(&["preview", "now"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("unexpected argument 'now' for 'preview'"));
    assert!(stderr.contains("bake-rs --help"));
}

#[test]
fn session_commands_are_unknown() {
    for command in ["inspect", "stat", "list"] {
        let out = run(&["session", command, "--help"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        let stderr = text(&out.stderr);
        assert!(stderr.contains("unknown command 'session'"));
        assert!(stderr.contains("bake-rs --help"));
    }
}
