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
    for args in [&[][..], &["--help"], &["help"], &["-h"], &["-p", "--help"]] {
        let out = run(args);
        assert!(out.status.success());
        let help = text(&out.stdout);
        assert!(help.contains("bake-rs preview"));
        assert!(help.contains("-p, --print"));
        assert!(out.stderr.is_empty());
    }
    for flag in ["--version", "-v", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success());
        assert_eq!(
            text(&out.stdout),
            format!("bake-rs {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
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
fn unknown_options_fail_as_pi_reports_them() {
    let out = run(&["--bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(text(&out.stderr), "Error: Unknown option: --bogus\n");
    let out = run(&["-x", "hi"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "Error: Unknown option: -x\n");
    let out = run(&["--mode", "rpc"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Error: Invalid mode \"rpc\". Valid values: text, json\n"
    );
}
