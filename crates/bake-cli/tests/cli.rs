//! Runs the built binary with piped, non-terminal standard streams, and,
//! for the interactive `--offline` case, on a pseudo-terminal. Scope 13's
//! command-line proofs live here: `--export` never writes its input,
//! `--offline` makes no startup connection, keys are printed only by the
//! commands that print one, excluded flags are unknown options, and no
//! built-in model catalog or provider route is reachable (D48). Every
//! server a case uses binds 127.0.0.1 and belongs to that case.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bake"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("bake runs")
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
        assert!(help.contains("bake preview"));
        assert!(help.contains("-p, --print"));
        assert!(out.stderr.is_empty());
    }
    for flag in ["--version", "-v", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success());
        assert_eq!(
            text(&out.stdout),
            format!("bake {}\n", env!("CARGO_PKG_VERSION"))
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
    assert!(stderr.contains("bake --help"));
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

/// The topic names `bake docs` lists.
fn listed_topics() -> Vec<String> {
    let out = run(&["docs"]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let list = text(&out.stdout);
    assert!(list.contains("bake docs <topic>"));
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
    assert!(stderr.contains("bake docs <topic>"));
    let out = run(&["docs", "cli", "extra"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("unexpected argument 'extra' for 'docs'"));
}

/// `bake update` runs against the install the Bake installer made. The
/// test binary is not one, so every run says so instead of reaching the
/// network, and a mistyped option is a usage error.
#[test]
fn update_needs_a_native_install_and_names_a_mistyped_option() {
    let run_update = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bake"))
            .args(args)
            .env_remove("BAKE_NATIVE_ROOT")
            .stdin(Stdio::null())
            .output()
            .expect("bake runs")
    };
    for args in [
        &["update"][..],
        &["update", "--check"],
        &["update", "--rollback"],
    ] {
        let out = run_update(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            text(&out.stderr).contains("needs a native install"),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    let out = run_update(&["update", "--checks"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("unexpected argument '--checks' for 'update'"));
}

#[test]
fn help_mentions_docs() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let help = text(&out.stdout);
    assert!(help.contains("bake docs [topic]"));
    assert!(help.contains("Run 'bake docs'"));
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
            "`ls -le` and `chmod -a# 0` inspect permissions; `bake --typo` still fails, as does `--unknown`."
        ),
        ["--typo", "--unknown"]
    );
}

/// The options of `bake auth`, which parses its own command line
/// ([`bake_coding_agent::cli::auth_command`]) rather than the agent's, so
/// the agent's parser does not accept them. `--help` still names them.
const AUTH_OPTIONS: &[&str] = &["--json", "--credentials", "--no-refresh", "--min-expiry"];

/// The options of `bake update`, which parses its own command line
/// (`src/update.rs`) rather than the agent's. `--help` names them.
const UPDATE_OPTIONS: &[&str] = &["--check", "--force", "--rollback"];

/// Pi's options `bake` excludes. The documentation names them to say
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
                    "{topic} calls {flag} excluded, but bake accepts it"
                );
                continue;
            }
            assert!(
                help.contains(&flag),
                "{topic} names {flag}, which --help omits"
            );
            // Commands of the binary, not options of the agent command line.
            if flag == "--inline"
                || flag == "--self-check"
                || UPDATE_OPTIONS.contains(&flag.as_str())
            {
                continue;
            }
            let args = bake_coding_agent::cli::parse_args(&[flag.clone(), "x".into()]);
            assert!(
                args.unknown_flags.is_empty()
                    && !args
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.message.contains("Unknown option")),
                "{topic} names {flag}, which bake does not accept"
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
        Command::new(env!("CARGO_BIN_EXE_bake"))
            .arg("--export")
            .args(args)
            .current_dir(&root.0)
            .env_remove("BAKE_HOME")
            .env_remove("DSH_HOME")
            .stdin(Stdio::null())
            .output()
            .expect("bake runs")
    };
    let out = export(&[session.as_os_str()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let name = "bake-session-2026-01-01T00-00-00-000Z_s1.html";
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
        Command::new(env!("CARGO_BIN_EXE_bake"))
            .args(args)
            .env("BAKE_HOME", &home)
            .env_remove("DSH_HOME")
            .current_dir(&root.0)
            .stdin(Stdio::null())
            .output()
            .expect("bake runs")
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
    assert!(stderr.contains("bake auth check --provider"), "{stderr}");

    // OAuth providers are deferred (D8 and D48), so there is no bearer
    // token to print, and the key is not offered in its place.
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
        text(&bearer.stderr).contains(
            "auth print-bearer-token is not available in bake yet; OAuth providers, which hold bearer tokens, are planned for a later release"
        ),
        "{}",
        text(&bearer.stderr)
    );
    assert!(!text(&bearer.stderr).contains("sk-faux-stored"));
}

/// Scope 13: `--export` reads its input without the session's lock and
/// writes only the page. The session file and its directory are made
/// read-only and dated in the past, and after the export the file is byte
/// for byte the same, with the same modification time, and the directory
/// holds nothing new.
#[cfg(unix)]
#[test]
fn export_never_writes_its_input() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new("export-read-only");
    let sessions = root.0.join("sessions");
    let pages = root.0.join("pages");
    std::fs::create_dir_all(&sessions).expect("sessions");
    std::fs::create_dir_all(&pages).expect("pages");
    let session = sessions.join("2026-01-01T00-00-00-000Z_s1.jsonl");
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
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&session)
        .and_then(|file| file.set_modified(past))
        .expect("date the session");
    std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let listing = |dir: &std::path::Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        names.sort();
        names
    };
    let bytes = std::fs::read(&session).expect("read");
    let before = std::fs::metadata(&session).expect("metadata");
    let names = listing(&sessions);

    let page = pages.join("out.html");
    let out = Command::new(env!("CARGO_BIN_EXE_bake"))
        .arg("--export")
        .arg(&session)
        .arg(&page)
        .current_dir(&pages)
        .env_remove("BAKE_HOME")
        .env_remove("DSH_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("bake runs");
    // Restored first, so the directory can be removed whatever happens.
    let restore = std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o755));
    assert!(out.status.success(), "{}", text(&out.stderr));
    restore.expect("chmod back");
    assert!(
        std::fs::read_to_string(&page)
            .expect("page")
            .starts_with("<!DOCTYPE html>")
    );
    let after = std::fs::metadata(&session).expect("metadata");
    assert_eq!(std::fs::read(&session).expect("read"), bytes);
    assert_eq!(after.modified().ok(), before.modified().ok());
    assert_eq!(after.modified().ok(), Some(past));
    assert_eq!(after.len(), before.len());
    assert_eq!(listing(&sessions), names);
}

/// Scope 13 and D43: Pi's extension and theme flags and `--tui-mode` are
/// not `bake` options, so each is refused as an unknown option before
/// anything starts, and nothing is written to standard output.
#[test]
fn excluded_flags_are_refused_as_unknown_options() {
    for (args, expected) in [
        (&["--extension", "./ext.ts"][..], "--extension"),
        (&["--no-extensions"], "--no-extensions"),
        (&["--theme", "./theme.json"], "--theme"),
        (&["--use-theme", "light"], "--use-theme"),
        (&["--no-themes"], "--no-themes"),
        (&["--tui-mode", "fullscreen"], "--tui-mode"),
        (&["-e", "./ext.ts"], "-e"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_eq!(
            text(&out.stderr),
            format!("Error: Unknown option: {expected}\n"),
            "{args:?}"
        );
    }
}

/// A Bake home whose `faux` provider has a stored key, for the key cases.
struct KeyHome {
    root: TempDir,
}

/// The stored key, which only the commands that print a key may show.
const STORED_KEY: &str = "sk-scope13-stored-secret";
/// The key `models.json` gives a provider inline.
const INLINE_KEY: &str = "sk-scope13-inline-secret";

impl KeyHome {
    fn new(name: &str) -> Self {
        let root = TempDir::new(name);
        for dir in ["home", "project", "user"] {
            std::fs::create_dir_all(root.0.join(dir)).expect("dir");
        }
        // Port 1 refuses connections, so a prompt fails without the
        // network.
        std::fs::write(
            root.0.join("home/models.json"),
            serde_json::json!({
                "providers": {
                    "faux": { "baseUrl": "http://127.0.0.1:1/v1", "api": "openai-responses",
                        "models": [{ "id": "faux-1", "contextWindow": 1000 }] },
                    "inline": { "baseUrl": "http://127.0.0.1:1/v1", "api": "openai-responses",
                        "apiKey": INLINE_KEY,
                        "models": [{ "id": "inline-1", "contextWindow": 1000 }] },
                },
            })
            .to_string(),
        )
        .expect("models");
        std::fs::write(
            root.0.join("home/auth.json"),
            serde_json::json!({ "faux": { "type": "api_key", "key": STORED_KEY } }).to_string(),
        )
        .expect("auth");
        // A refused connection fails the prompt at once, not after retries.
        std::fs::write(
            root.0.join("home/settings.json"),
            serde_json::json!({ "retry": { "enabled": false } }).to_string(),
        )
        .expect("settings");
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bake"));
        command
            .args(args)
            .current_dir(self.root.0.join("project"))
            .env("BAKE_HOME", self.root.0.join("home"))
            .env_remove("DSH_HOME")
            .env_remove("CLIPROXYAPI_API_KEY")
            .env("HOME", self.root.0.join("user"))
            .stdin(Stdio::null());
        command
    }

    /// Runs `args`, writing `input` to standard input when it is given.
    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut command = self.command(args);
        if input.is_none() {
            return command.output().expect("bake runs");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bake starts");
        if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
            use std::io::Write;
            stdin.write_all(input.as_bytes()).expect("write input");
        }
        child.wait_with_output().expect("bake output")
    }
}

/// Scope 13: a key reaches standard output only from `auth print-api-key`
/// and `auth check --credentials`, the commands whose purpose is to print
/// one. Help, listings, checks, failed prompts in every output mode, RPC
/// state and model listings, and every error leave both keys out of both
/// streams.
#[test]
fn keys_are_printed_only_by_the_commands_that_print_one() {
    let home = KeyHome::new("keys");
    let rpc_input = concat!(
        r#"{"id":"s","type":"get_state"}"#,
        "\n",
        r#"{"id":"m","type":"get_available_models"}"#,
        "\n",
        r#"{"id":"st","type":"get_session_stats"}"#,
        "\n",
        r#"{"id":"c","type":"get_commands"}"#,
        "\n",
    );
    let quiet: &[(&[&str], Option<&str>)] = &[
        (&["--help"], None),
        (&["--version"], None),
        (&["--list-models"], None),
        (&["--list-models", "faux"], None),
        (&["auth", "--help"], None),
        (&["auth", "check", "--provider", "faux"], None),
        (&["auth", "check", "--provider", "faux", "--json"], None),
        (&["auth", "check", "--provider", "inline", "--json"], None),
        (&["auth", "check", "--model", "faux/faux-1"], None),
        (&["auth", "print-bearer-token", "--provider", "faux"], None),
        (&["auth", "print-api-key", "--provider", "nope"], None),
        (&["auth", "print-api-key", "--model", "missing"], None),
        (&["--model", "faux/faux-1", "-p", "hello"], None),
        (&["--model", "inline/inline-1", "-p", "hello"], None),
        (&["--model", "faux/faux-1", "--mode", "json", "hello"], None),
        (
            &["--model", "faux/faux-1", "--verbose", "-p", "hello"],
            None,
        ),
        (
            &["--model", "faux/faux-1", "--mode", "rpc"],
            Some(rpc_input),
        ),
        (
            &["--model", "inline/inline-1", "--mode", "rpc"],
            Some(rpc_input),
        ),
        (&["--model", "faux/faux-1", "--bogus"], None),
    ];
    for (args, input) in quiet {
        let out = home.run(args, *input);
        for key in [STORED_KEY, INLINE_KEY] {
            assert!(
                !text(&out.stdout).contains(key) && !text(&out.stderr).contains(key),
                "{args:?} showed a key: {}{}",
                text(&out.stdout),
                text(&out.stderr)
            );
        }
    }
    // The quiet cases did reach the key: the prompt was sent with it and
    // failed on the refused connection, which only a request that found
    // the key makes (without one it fails first, with "No API key found"),
    // and RPC mode listed the models.
    let failed = home.run(&["--model", "faux/faux-1", "-p", "hello"], None);
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    assert!(
        text(&failed.stderr).starts_with(
            "Connection error. error sending request for url (http://127.0.0.1:1/v1/responses)"
        ),
        "{}",
        text(&failed.stderr)
    );
    let listed = home.run(
        &["--model", "faux/faux-1", "--mode", "rpc"],
        Some(rpc_input),
    );
    assert!(
        text(&listed.stdout).contains("\"faux-1\""),
        "{}",
        text(&listed.stdout)
    );
    // The commands that exist to print one print exactly it.
    for (args, key) in [
        (
            &["auth", "print-api-key", "--provider", "faux"][..],
            STORED_KEY,
        ),
        (
            &["auth", "print-api-key", "--provider", "inline"],
            INLINE_KEY,
        ),
        (
            &["auth", "check", "--provider", "faux", "--credentials"],
            STORED_KEY,
        ),
    ] {
        let out = home.run(args, None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), format!("{key}\n"), "{args:?}");
        assert!(!text(&out.stderr).contains(key), "{args:?}");
    }
}

/// A loopback CLIProxyAPI that records the request line of every
/// connection: `GET /v1/models` is the catalog a startup refresh fetches,
/// and `POST /v1/responses` a model request, answered with the text
/// stream in `fixtures/`. Stopped and joined on drop.
struct Proxy {
    port: u16,
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Proxy {
    fn start() -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("address").port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    let _ = socket.set_nonblocking(false);
                    let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 8192];
                    // The whole request is read before the answer, so the
                    // close that follows it resets nothing unread.
                    let complete = |request: &[u8]| {
                        let end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")?;
                        let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        (request.len() >= end + 4 + length).then_some(())
                    };
                    while complete(&request).is_none() {
                        match socket.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    seen.lock().expect("seen").push(line.clone());
                    let (kind, body) = if line.starts_with("GET /v1/models") {
                        (
                            "application/json",
                            r#"{"object":"list","data":[{"id":"proxy-model","object":"model"}]}"#
                                .to_owned(),
                        )
                    } else {
                        (
                            "text/event-stream",
                            format!("{}\n", include_str!("fixtures/openai-responses-text.sse")),
                        )
                    };
                    let _ = socket.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                }
            })
        };
        Self {
            port,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }

    fn catalog_requests(&self) -> usize {
        self.seen()
            .iter()
            .filter(|line| line.starts_with("GET /v1/models"))
            .count()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A Bake home signed in to `proxy`, as `/login` leaves one: the route in
/// `settings.json` and its key in `auth.json`, bound by one generation.
fn signed_in_home(name: &str, proxy: &Proxy) -> TempDir {
    let root = TempDir::new(name);
    for dir in ["home", "project", "user"] {
        std::fs::create_dir_all(root.0.join(dir)).expect("dir");
    }
    std::fs::write(
        root.0.join("home/settings.json"),
        serde_json::json!({
            "cliproxyapi": {
                "displayName": "CLIProxyAPI",
                "apiKeyEnv": "CLIPROXYAPI_API_KEY",
                "api": "openai-responses",
                "baseURL": format!("http://127.0.0.1:{}/v1", proxy.port),
                "models": [{ "id": "proxy-model", "name": "Proxy Model", "contextWindow": 200000,
                    "maxTokens": 32000, "input": ["text"] }],
                "retryPolicy": { "mode": "normal", "backoff": { "maxDelayMs": 60000 } },
                "compat": { "sendSessionAffinityHeaders": true },
                "account": "generation-1",
            },
            "retry": { "enabled": false },
        })
        .to_string(),
    )
    .expect("settings");
    std::fs::write(
        root.0.join("home/auth.json"),
        serde_json::json!({ "cliproxyapi": { "type": "api_key", "key": "sk-proxy-test", "account": "generation-1" } })
            .to_string(),
    )
    .expect("auth");
    root
}

fn home_command(root: &TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bake"));
    command
        .args(args)
        .current_dir(root.0.join("project"))
        .env("BAKE_HOME", root.0.join("home"))
        .env_remove("DSH_HOME")
        .env_remove("BAKE_OFFLINE")
        .env_remove("DSH_OFFLINE")
        .env_remove("CLIPROXYAPI_API_KEY")
        .env("HOME", root.0.join("user"));
    // The terminal running the tests chooses no status indicators or
    // Herdr reports for the `bake` under test.
    for name in [
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_SOCKET_PATH",
    ] {
        command.env_remove(name);
    }
    command
}

/// Scope 13 and Pi's `offlineMode`: print and RPC mode start no network
/// operation of their own, with `--offline` or without it. Each run reads
/// the signed-in account (RPC mode lists its model) and the proxy sees no
/// connection.
#[test]
fn offline_print_and_rpc_modes_make_no_startup_connection() {
    use std::io::Write;
    let proxy = Proxy::start();
    let root = signed_in_home("offline-modes", &proxy);
    for offline in [true, false] {
        let flag: &[&str] = if offline { &["--offline"] } else { &[] };
        let mut args = vec!["--mode", "rpc", "--model", "cliproxyapi/proxy-model"];
        args.extend_from_slice(flag);
        let mut child = home_command(&root, &args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bake starts");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"{\"id\":\"m\",\"type\":\"get_available_models\"}\n")
            .expect("write");
        let out = child.wait_with_output().expect("bake output");
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(
            text(&out.stdout).contains("\"proxy-model\""),
            "{}",
            text(&out.stdout)
        );

        let mut args = vec!["--list-models", "proxy"];
        args.extend_from_slice(flag);
        let out = home_command(&root, &args)
            .stdin(Stdio::null())
            .output()
            .expect("bake runs");
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(
            text(&out.stdout).contains("proxy-model"),
            "{}",
            text(&out.stdout)
        );
    }
    assert_eq!(proxy.catalog_requests(), 0);
    assert_eq!(proxy.seen(), Vec::<String>::new());
}

/// An interactive `bake` on a pseudo-terminal, which answers the cursor
/// and device queries a terminal answers; killed and reaped on drop.
#[cfg(target_os = "linux")]
struct Terminal {
    child: std::process::Child,
    screen: std::sync::Arc<std::sync::Mutex<String>>,
    input: std::fs::File,
    reader: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl Terminal {
    fn start(mut command: Command) -> Self {
        use std::io::{Read, Write};
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        let (mut master, mut slave) = (0, 0);
        let size = libc::winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        assert_eq!(opened, 0, "openpty");
        // Neither descriptor is inherited as such: the child gets the
        // terminal as its standard streams only.
        for fd in [master, slave] {
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        command
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().expect("slave")))
            .stdout(Stdio::from(slave.try_clone().expect("slave")))
            .stderr(Stdio::from(slave));
        // The terminal becomes the child's controlling terminal, in a
        // session of its own.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("bake starts");
        // The parent's copies of the terminal end with the command.
        drop(command);
        let input = std::fs::File::from(master);
        let mut output = input.try_clone().expect("master");
        let mut answers = input.try_clone().expect("master");
        let screen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let reader = {
            let screen = screen.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                // The child's end closing reads as an error (EIO).
                while let Ok(read) = output.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&chunk[..read]).into_owned();
                    if text.contains("\x1b[6n") {
                        let _ = answers.write_all(b"\x1b[1;1R");
                    }
                    if text.contains("\x1b[c") || text.contains("\x1b[0c") {
                        let _ = answers.write_all(b"\x1b[?62;22c");
                    }
                    screen.lock().expect("screen").push_str(&text);
                }
            })
        };
        Self {
            child,
            screen,
            input,
            reader: Some(reader),
        }
    }

    fn screen(&self) -> String {
        self.screen.lock().expect("screen").clone()
    }

    /// Waits until `ready` holds, failing with what was drawn.
    fn wait(&self, what: &str, ready: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "no {what}; the terminal shows {:?}",
                self.screen()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn send(&mut self, keys: &str) {
        use std::io::Write;
        self.input.write_all(keys.as_bytes()).expect("type");
    }

    /// Stops `bake` with `SIGTERM` and returns its status. One that has
    /// not ended within the patience is killed with its process group
    /// (`setsid` made it the group's leader), reaped, and fails the test.
    fn stop(mut self) -> std::process::ExitStatus {
        let pid = self.child.id() as i32;
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("bake's status") {
                return status;
            }
            if std::time::Instant::now() >= deadline {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                let _ = self.child.wait();
                panic!(
                    "bake did not end within 30 seconds of SIGTERM; killed. The terminal shows {:?}",
                    self.screen()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The reader ends once the child's end of the terminal is closed.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Scope 13 and Pi's `offlineMode`, end to end in a terminal: an
/// interactive launch signed in to a loopback CLIProxyAPI refreshes its
/// catalog at startup, which is the control, and with `--offline` it
/// does not, though the session still reaches the proxy for its prompt.
#[cfg(target_os = "linux")]
#[test]
fn offline_stops_the_interactive_startup_refresh() {
    let proxy = Proxy::start();
    let root = signed_in_home("offline-terminal", &proxy);
    let launch = |offline: bool| {
        let mut args = vec!["--model", "cliproxyapi/proxy-model"];
        if offline {
            args.push("--offline");
        }
        Terminal::start(home_command(&root, &args))
    };

    let online = launch(false);
    online.wait("startup catalog refresh", || proxy.catalog_requests() > 0);
    online.stop();
    let refreshed = proxy.catalog_requests();

    let mut offline = launch(true);
    offline.wait("composer", || offline.screen().contains('❯'));
    offline.send("hello\r");
    offline.wait("the prompt's model request", || {
        proxy
            .seen()
            .iter()
            .any(|line| line.starts_with("POST /v1/responses"))
    });
    // The answer's words are drawn one cursor move apart; the bar then
    // reports the turn done.
    offline.wait("the completed turn", || {
        let screen = offline.screen();
        screen.contains("Hello") && screen.contains("loop.") && screen.contains("Completed")
    });
    offline.stop();
    assert_eq!(
        proxy.catalog_requests(),
        refreshed,
        "the offline launch refreshed the catalog: {:?}",
        proxy.seen()
    );
}

/// The terminal's indicators and Herdr's reports, end to end in a
/// terminal that names itself Ghostty, with a stand-in Herdr CLI that
/// records its calls: the title is saved and set, a named session renames
/// it, a turn shows progress and reports `working`, a turn that ends while
/// the terminal is out of focus notifies, and `SIGTERM` puts the title back,
/// stops focus reports, clears the display tokens, and releases the pane.
#[cfg(target_os = "linux")]
#[test]
fn ghostty_and_herdr_follow_a_named_session_through_a_turn() {
    use std::os::unix::fs::PermissionsExt;
    let proxy = Proxy::start();
    let root = signed_in_home("terminal-status", &proxy);
    let calls = root.0.join("herdr-calls");
    let herdr = root.0.join("herdr");
    std::fs::write(
        &herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display()
        ),
    )
    .expect("herdr stand-in");
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let recorded = || std::fs::read_to_string(&calls).unwrap_or_default();
    let mut command = home_command(&root, &["--model", "cliproxyapi/proxy-model", "--offline"]);
    command
        .env("TERM_PROGRAM", "ghostty")
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p1")
        .env("HERDR_BIN_PATH", &herdr)
        .env("HERDR_SOCKET_PATH", root.0.join("herdr.sock"));
    let mut terminal = Terminal::start(command);
    terminal.wait("composer", || terminal.screen().contains('❯'));
    let screen = terminal.screen();
    assert!(
        screen.contains("\x1b[22;0t\x1b]0;bake - project\x07"),
        "{screen:?}"
    );
    assert!(screen.contains("\x1b[?1004h"), "{screen:?}");
    terminal.wait("Herdr's idle report", || {
        recorded().contains("--state idle")
    });

    terminal.send("/name auth work\r");
    terminal.wait("the renamed title", || {
        terminal
            .screen()
            .contains("\x1b]0;bake - auth work - project\x07")
    });
    // The composer takes keys again once the command's frame is armed.
    terminal.wait("the composer after the command", || {
        let screen = terminal.screen();
        screen
            .rfind("Preparing…")
            .is_some_and(|at| screen[at..].contains("Type a draft"))
    });
    // The terminal reports losing focus, as Ghostty does when its window
    // or tab is left.
    terminal.send("\x1b[O");
    terminal.send("hello\r");
    terminal.wait("the completed turn's notification", || {
        terminal
            .screen()
            .contains("\x1b]777;notify;bake - auth work;Ready for your next prompt\x07")
    });
    let screen = terminal.screen();
    let started = screen.find("\x1b]9;4;3\x07").expect("progress shown");
    let cleared = screen.rfind("\x1b]9;4;0\x07").expect("progress cleared");
    assert!(started < cleared, "{screen:?}");
    assert!(screen.contains("\x1b]0;◐ bake - auth work - project\x07"));
    // Herdr's reports run on their own worker, so the turn's idle report
    // can land after the terminal has already drawn its end.
    terminal.wait("Herdr's tokens and the turn's idle report", || {
        let calls = recorded();
        calls.contains("--token model=proxy-model")
            && calls.contains("--token session=auth work")
            && calls
                .find("--state working")
                .is_some_and(|working| calls[working..].contains("--state idle"))
    });
    assert!(recorded().contains("--token activity="), "{}", recorded());

    let before = terminal.screen().len();
    let output = std::sync::Arc::clone(&terminal.screen);
    terminal.stop();
    let restored = output.lock().expect("screen")[before..].to_owned();
    assert!(restored.contains("\x1b[23;0t"), "{restored:?}");
    assert!(restored.contains("\x1b[?1004l"), "{restored:?}");
    let calls = recorded();
    let last: Vec<&str> = calls.lines().rev().take(2).collect();
    assert!(
        last[0].starts_with("pane release-agent w1:p1 --source bake"),
        "{calls}"
    );
    assert!(
        last[1].starts_with("pane report-metadata w1:p1 --source bake"),
        "{calls}"
    );
    assert!(
        last[1].contains("--clear-token model") && !last[1].contains("--token "),
        "{calls}"
    );
}

/// D8 and D48: `bake` carries no built-in model catalog and no provider
/// route of its own. With an empty home nothing is listed, a catalog
/// provider's model does not resolve, and an auth check does not find the
/// provider: only `models.json` and the CLIProxyAPI account add routes.
#[test]
fn no_built_in_model_catalog_or_provider_route_is_reachable() {
    let root = TempDir::new("no-catalog");
    for dir in ["home", "project", "user"] {
        std::fs::create_dir_all(root.0.join(dir)).expect("dir");
    }
    let listed = home_command(&root, &["--list-models"])
        .stdin(Stdio::null())
        .output()
        .expect("bake runs");
    assert_eq!(listed.status.code(), Some(0));
    assert!(
        text(&listed.stdout).starts_with("No models available."),
        "{}",
        text(&listed.stdout)
    );
    let prompt = home_command(
        &root,
        &[
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-4-5",
            "-p",
            "hi",
        ],
    )
    .stdin(Stdio::null())
    .output()
    .expect("bake runs");
    // With no catalog there is no model at all; a built-in catalog would
    // find the model and fail on its missing key instead.
    assert_eq!(prompt.status.code(), Some(1));
    assert!(prompt.stdout.is_empty());
    assert!(
        text(&prompt.stderr).starts_with("Error: No models available."),
        "{}",
        text(&prompt.stderr)
    );
    let check = home_command(
        &root,
        &["auth", "check", "--provider", "anthropic", "--json"],
    )
    .stdin(Stdio::null())
    .output()
    .expect("bake runs");
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(
        text(&check.stdout),
        "{\"status\":\"not_ready\",\"provider\":\"anthropic\",\"reason\":\"provider_not_found\"}\n"
    );
}
