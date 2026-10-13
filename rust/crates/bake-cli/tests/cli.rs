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
    for args in [&["preview"][..], &["preview", "--inline"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty(), "no terminal sequences on stdout");
        assert!(text(&out.stderr).contains("needs an interactive terminal"));
    }
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

#[test]
fn inline_preview_is_documented_and_rejects_extra_arguments() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).contains("--inline"));
    let out = run(&["preview", "--inline", "now"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("unexpected argument 'now'"));
}

/// The topic names `bake-rs docs` lists.
fn listed_topics() -> Vec<String> {
    let out = run(&["docs"]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let list = text(&out.stdout);
    assert!(list.contains("bake-rs docs <topic>"));
    list.lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_docs_topic_prints_its_markdown_without_a_terminal() {
    let topics = listed_topics();
    for expected in [
        "cli",
        "home",
        "models",
        "settings",
        "permissions",
        "sessions",
        "context",
    ] {
        assert!(topics.iter().any(|topic| topic == expected), "{expected}");
    }
    for topic in &topics {
        let out = run(&["docs", topic]);
        assert!(out.status.success(), "{topic}");
        assert!(out.stderr.is_empty(), "{topic}");
        let page = text(&out.stdout);
        assert!(page.starts_with("# "), "{topic}");
        assert!(page.lines().count() > 3, "{topic}");
    }
}

#[test]
fn an_unknown_docs_topic_fails_with_the_list() {
    let out = run(&["docs", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("unknown docs topic 'nope'"));
    assert!(stderr.contains("bake-rs docs <topic>"));
    let out = run(&["docs", "cli", "extra"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("unexpected argument 'extra' for 'docs'"));
}

#[test]
fn help_mentions_docs() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let help = text(&out.stdout);
    assert!(help.contains("bake-rs docs [topic]"));
    assert!(help.contains("Run 'bake-rs docs'"));
}

/// Every `-flag` the documentation names, outside the word it uses as a
/// placeholder (`-name`) and session directory names (`--a-b--`).
fn documented_flags(page: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    for word in page.split(|c: char| c.is_whitespace() || "`(),\"".contains(c)) {
        let word = word.trim_end_matches(['.', ':', ';']);
        let name = word.trim_start_matches('-');
        if word.starts_with('-')
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            && word != "-name"
            && !word.ends_with('-')
            && !flags.iter().any(|flag| flag == word)
        {
            flags.push(word.to_owned());
        }
    }
    flags
}

#[test]
fn flags_named_in_the_docs_are_accepted() {
    let help = text(&run(&["--help"]).stdout);
    let mut checked = 0;
    for topic in listed_topics() {
        let page = text(&run(&["docs", &topic]).stdout);
        for flag in documented_flags(&page) {
            checked += 1;
            assert!(
                help.contains(&flag),
                "{topic} names {flag}, which --help omits"
            );
            if flag == "--inline" {
                continue;
            }
            let args = bake_coding_agent::cli::parse_args(&[flag.clone(), "x".into()]);
            assert!(
                args.unknown_flags.is_empty()
                    && !args
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.message.contains("Unknown option")),
                "{topic} names {flag}, which bake-rs does not accept"
            );
        }
    }
    assert!(checked > 15, "only {checked} flags found");
}
