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
    let out = run(&["--mode", "acp"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Error: Invalid mode \"acp\". Valid values: text, json, rpc\n"
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
        "plugins",
        "skills",
        "hooks",
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

/// Agent flags named by the documentation. Inline examples of external
/// permission-inspection commands have their own options.
fn documented_flags(page: &str) -> Vec<String> {
    let page: String = page
        .split('`')
        .enumerate()
        .map(|(index, span)| {
            if index % 2 == 1 && matches!(span.split_whitespace().next(), Some("ls" | "chmod")) {
                " "
            } else {
                span
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
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
fn external_command_examples_do_not_hide_agent_flags() {
    assert_eq!(
        documented_flags(
            "`ls -le` and `chmod -a# 0` inspect permissions; `bake-rs --typo` still fails, as does `--unknown`."
        ),
        ["--typo", "--unknown"]
    );
}

/// The options of `bake-rs auth`, which parses its own command line
/// ([`bake_coding_agent::cli::auth_command`]) rather than the agent's, so
/// the agent's parser does not accept them. `--help` still names them.
const AUTH_OPTIONS: &[&str] = &["--json", "--credentials", "--no-refresh", "--min-expiry"];

/// Pi's options `bake-rs` excludes. The documentation names them to say
/// they are refused, so the parser must not accept them and `--help` must
/// not offer them.
const EXCLUDED_PI_OPTIONS: &[&str] = &[
    "--extension",
    "-e",
    "--no-extensions",
    "-ne",
    "--theme",
    "--use-theme",
    "--no-themes",
    "--tui-mode",
];

#[test]
fn flags_named_in_the_docs_are_accepted() {
    // Whole words, so `-c` does not pass inside `--continue`.
    let help_text = text(&run(&["--help"]).stdout);
    let help = documented_flags(&help_text);
    let mut checked = 0;
    for topic in listed_topics() {
        let page = text(&run(&["docs", &topic]).stdout);
        for flag in documented_flags(&page) {
            checked += 1;
            if AUTH_OPTIONS.contains(&flag.as_str()) {
                assert!(
                    help_text.contains(&flag),
                    "{topic} names {flag}, which --help omits"
                );
                continue;
            }
            if EXCLUDED_PI_OPTIONS.contains(&flag.as_str()) {
                assert!(
                    !help.contains(&flag),
                    "{topic} calls {flag} excluded, but --help offers it"
                );
                let args = bake_coding_agent::cli::parse_args(&[flag.clone(), "x".into()]);
                assert!(
                    !args.unknown_flags.is_empty()
                        || args
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.message.contains("Unknown option")),
                    "{topic} calls {flag} excluded, but bake-rs accepts it"
                );
                continue;
            }
            assert!(
                help.contains(&flag),
                "{topic} names {flag}, which --help omits"
            );
            // Commands of the binary, not options of the agent command line.
            if flag == "--inline" || flag == "--self-check" {
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

/// A directory under the system temp directory, removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bake-cli-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Pi's `--export` branch of `main.ts`: the page is written beside the
/// working directory under Pi's default name, its path is printed, and a
/// missing file is one `Error: ` line with status 1. It needs no Bake home,
/// so the run is given none.
#[test]
fn export_writes_one_page_and_exits() {
    let root = TempDir::new("export");
    let session = root.0.join("2026-01-01T00-00-00-000Z_s1.jsonl");
    std::fs::write(
        &session,
        concat!(
            r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#,
            "\n",
        ),
    )
    .expect("write session");
    let export = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_bake-rs"))
            .arg("--export")
            .args(args)
            .current_dir(&root.0)
            .env_remove("BAKE_HOME")
            .env_remove("DSH_HOME")
            .stdin(Stdio::null())
            .output()
            .expect("bake-rs runs")
    };
    let out = export(&[session.as_os_str()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let name = "bake-rs-session-2026-01-01T00-00-00-000Z_s1.html";
    // Pi's default name is relative, so the page lands in the working
    // directory and the printed path is the name itself.
    assert_eq!(text(&out.stdout), format!("Exported to: {name}\n"));
    let default = root.0.join(name);
    assert!(
        std::fs::read_to_string(&default)
            .expect("page")
            .starts_with("<!DOCTYPE html>")
    );

    let named = root.0.join("out.html");
    let out = export(&[session.as_os_str(), named.as_os_str()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(std::fs::read_to_string(&named).expect("page").contains("<"));

    let missing = root.0.join("nope.jsonl");
    let out = export(&[missing.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        text(&out.stderr),
        format!("Error: File not found: {}\n", missing.display())
    );
}

/// Pi's `runAuthCommand` branch of `main.ts`: the commands read the Bake
/// home's files, print one line, and exit with Pi's status. The key is in
/// the output of `print-api-key` and of `check --credentials`, whose
/// purpose is to print it, and in nothing else.
#[test]
fn auth_commands_report_a_provider_and_print_its_key() {
    let root = TempDir::new("auth");
    let home = root.0.join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join("models.json"),
        concat!(
            r#"{"providers":{"faux":{"baseUrl":"https://example.invalid","api":"openai-completions","#,
            r#""models":[{"id":"faux-1","contextWindow":1000}]},"#,
            r#""keyless":{"baseUrl":"https://example.invalid","api":"openai-completions","#,
            r#""models":[{"id":"keyless-1","contextWindow":1000}]}}}"#,
        ),
    )
    .expect("models");
    std::fs::write(
        home.join("auth.json"),
        r#"{"faux":{"type":"api_key","key":"sk-faux-stored"}}"#,
    )
    .expect("auth");
    let auth = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bake-rs"))
            .args(args)
            .env("BAKE_HOME", &home)
            .env_remove("DSH_HOME")
            .current_dir(&root.0)
            .stdin(Stdio::null())
            .output()
            .expect("bake-rs runs")
    };
    let ready = auth(&["auth", "check", "--provider", "faux"]);
    assert_eq!(ready.status.code(), Some(0));
    assert_eq!(text(&ready.stdout), "ready\n");
    assert!(!text(&ready.stdout).contains("sk-faux-stored"));

    let json = auth(&["auth", "check", "--provider", "faux", "--json"]);
    assert_eq!(json.status.code(), Some(0));
    assert_eq!(
        text(&json.stdout),
        "{\"status\":\"ready\",\"provider\":\"faux\",\"authType\":\"api_key\"}\n"
    );
    assert!(!text(&json.stdout).contains("sk-faux-stored"));

    let with_key = auth(&[
        "auth",
        "check",
        "--provider",
        "faux",
        "--credentials",
        "--no-refresh",
    ]);
    assert_eq!(with_key.status.code(), Some(0));
    assert_eq!(text(&with_key.stdout), "sk-faux-stored\n");

    let printed = auth(&["auth", "print-api-key", "--provider", "faux"]);
    assert_eq!(printed.status.code(), Some(0));
    assert_eq!(text(&printed.stdout), "sk-faux-stored\n");

    // Pi's statuses: a provider with no key is 1, a bad argument to a check
    // is 2, and a credential that cannot be printed is 1.
    let not_ready = auth(&["auth", "check", "--provider", "keyless"]);
    assert_eq!(not_ready.status.code(), Some(1));
    assert_eq!(text(&not_ready.stdout), "not_ready\n");
    let missing_argument = auth(&["auth", "check"]);
    assert_eq!(missing_argument.status.code(), Some(2));
    assert!(missing_argument.stdout.is_empty());
    let unknown_flag = auth(&["auth", "check", "--provider", "faux", "--theme", "dark"]);
    assert_eq!(unknown_flag.status.code(), Some(1));
    let stderr = text(&unknown_flag.stderr);
    assert!(
        stderr.contains("Unknown option --theme for \"auth check\"."),
        "{stderr}"
    );
    assert!(stderr.contains("bake-rs auth check --provider"), "{stderr}");

    // No OAuth provider is ported, so there is no bearer token to print,
    // and the key is not offered in its place.
    let bearer = auth(&[
        "auth",
        "print-bearer-token",
        "--provider",
        "faux",
        "--min-expiry",
        "30m",
    ]);
    assert_eq!(bearer.status.code(), Some(1));
    assert!(bearer.stdout.is_empty());
    assert!(
        text(&bearer.stderr).contains("not configured with an OAuth bearer token"),
        "{}",
        text(&bearer.stderr)
    );
    assert!(!text(&bearer.stderr).contains("sk-faux-stored"));
}
