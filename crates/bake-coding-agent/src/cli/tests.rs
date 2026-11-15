use std::sync::Mutex;

use super::*;

fn sink() -> (SharedWriter, Arc<Mutex<Vec<u8>>>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    (buffer.clone() as SharedWriter, buffer)
}

fn text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buffer.lock().map(|b| b.clone()).unwrap_or_default()).into_owned()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

// Pi `resolveAppMode`: print when asked or when a stream is not a terminal.
#[test]
fn app_mode_follows_pi() {
    let (stderr, errors) = sink();
    assert!(matches!(
        plan(&strings(&["hi"]), true, false, &stderr),
        Action::Print(_, Mode::Text)
    ));
    assert!(matches!(
        plan(&strings(&["--mode", "json", "hi"]), true, true, &stderr),
        Action::Print(_, Mode::Json)
    ));
    assert!(matches!(
        plan(&strings(&["-p", "hi"]), true, true, &stderr),
        Action::Print(_, Mode::Text)
    ));
    assert!(matches!(
        plan(&strings(&["hi", "--inline"]), true, true, &stderr),
        Action::Interactive(args) if args.inline && args.messages == ["hi"]
    ));
    assert!(matches!(
        plan(&strings(&[]), true, true, &stderr),
        Action::Interactive(_)
    ));
    assert_eq!(text(&errors), "");
}

// Bake's `--output-format` and `-` choose print mode in a terminal, as
// `--mode` and `-p` do; `--output-format` takes `--mode`'s place.
#[test]
fn output_formats_and_a_bare_dash_choose_print_mode() {
    let (stderr, errors) = sink();
    for (format, mode) in [
        ("text", Mode::Text),
        ("json", Mode::Result),
        ("stream-json", Mode::StreamJson),
    ] {
        assert!(
            matches!(
                plan(&strings(&["--output-format", format, "hi"]), true, true, &stderr),
                Action::Print(args, found) if found == mode && args.mode == Some(mode)
            ),
            "{format}"
        );
    }
    assert!(matches!(
        plan(&strings(&["-", "hi"]), true, true, &stderr),
        Action::Print(args, Mode::Text) if args.read_stdin && args.messages == ["hi"]
    ));
    assert_eq!(text(&errors), "");
    for (args, message) in [
        (
            vec!["--output-format", "json", "--mode", "json", "hi"],
            "Error: --output-format and --mode choose the same thing; give one of them\n",
        ),
        (
            vec!["--mode", "rpc", "-"],
            "Error: - reads a prompt from standard input, which carries the protocol in RPC mode\n",
        ),
    ] {
        let (stderr, errors) = sink();
        assert_eq!(plan(&strings(&args), true, true, &stderr), Action::Exit(1));
        assert_eq!(text(&errors), message, "{args:?}");
    }
}

/// A pipe whose writer the test holds: it stays open and silent until
/// the writer writes or is dropped.
fn held_pipe() -> (Box<dyn Read + Send>, std::io::PipeWriter) {
    let (reader, writer) = std::io::pipe().expect("pipe");
    (Box::new(reader), writer)
}

// Bake's own: piped input beside another prompt is left unread when it
// stays silent, and read to its end once it sends a byte or ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_pipe_is_left_unread_only_within_a_wait() {
    use std::io::Write as _;
    use std::time::Duration;

    let (silent, _held) = held_pipe();
    assert_eq!(
        read_stdin(silent, Some(Duration::from_millis(20))).await,
        Ok(Piped::Silent)
    );
    let (ended, writer) = held_pipe();
    drop(writer);
    assert_eq!(
        read_stdin(ended, Some(Duration::from_secs(60))).await,
        Ok(Piped::Read(None))
    );
    let (sent, mut writer) = held_pipe();
    writer.write_all(b"  the diff \n").expect("write");
    drop(writer);
    assert_eq!(
        read_stdin(sent, Some(Duration::from_secs(60))).await,
        Ok(Piped::Read(Some("the diff".into())))
    );
    // A byte within the wait keeps the read going to the end, however
    // long that takes; the wait bounds only the first byte.
    let (slow, mut writer) = held_pipe();
    writer.write_all(b"part one, ").expect("write");
    let (go, ready) = std::sync::mpsc::channel::<()>();
    let producer = std::thread::spawn(move || {
        ready.recv().expect("go");
        writer.write_all(b"part two").expect("write");
    });
    // The first part is in the pipe before the read starts; the second
    // comes well after the wait.
    let read = tokio::spawn(read_stdin(slow, Some(Duration::from_millis(300))));
    tokio::time::sleep(Duration::from_millis(600)).await;
    go.send(()).expect("send");
    producer.join().expect("producer");
    assert_eq!(
        read.await.expect("read"),
        Ok(Piped::Read(Some("part one, part two".into())))
    );
    // Without a wait, a silent pipe is waited for.
    let (late, mut writer) = held_pipe();
    let read = tokio::spawn(read_stdin(late, None));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!read.is_finished());
    writer.write_all(b"late prompt").expect("write");
    drop(writer);
    assert_eq!(
        read.await.expect("read"),
        Ok(Piped::Read(Some("late prompt".into())))
    );
}

#[test]
fn the_silent_pipe_warning_names_both_ways_out() {
    let warning = silent_stdin_warning(STDIN_FIRST_BYTE_WAIT);
    assert!(warning.contains("within 1000 ms"), "{warning}");
    assert!(warning.contains("Pass - ") && warning.contains("/dev/null"));
}

// The flags `bake` excludes (Pi's extensions, themes, and
// `--tui-mode`) are refused before anything is loaded, with Pi's
// unknown-option message and its exit status.
#[test]
fn the_excluded_pi_flags_are_refused_as_unknown_options() {
    for (flags, message) in [
        (
            vec!["--extension", "./ext.ts", "-p", "hi"],
            "Error: Unknown option: --extension\n",
        ),
        (
            vec!["--no-extensions"],
            "Error: Unknown option: --no-extensions\n",
        ),
        (
            vec!["--theme", "./dark.json"],
            "Error: Unknown option: --theme\n",
        ),
        (
            vec!["--use-theme", "light"],
            "Error: Unknown option: --use-theme\n",
        ),
        (vec!["--no-themes"], "Error: Unknown option: --no-themes\n"),
        (
            vec!["--tui-mode", "regular"],
            "Error: Unknown option: --tui-mode\n",
        ),
        (vec!["-e", "./ext.ts"], "Error: Unknown option: -e\n"),
        (vec!["-ne"], "Error: Unknown option: -ne\n"),
    ] {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&flags), true, true, &stderr),
            Action::Exit(1),
            "{flags:?}"
        );
        assert_eq!(text(&errors), message, "{flags:?}");
    }
}

// Bake: the project-trust flags are parsed and then refused, with the
// reason instead of `Unknown option`, and nothing is loaded first.
#[test]
fn the_project_trust_flags_are_refused() {
    for flags in [
        vec!["-a"],
        vec!["--approve", "-p", "hi"],
        vec!["-na"],
        vec!["--no-approve", "task"],
    ] {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&flags), true, true, &stderr),
            Action::Exit(1),
            "{flags:?}"
        );
        assert_eq!(text(&errors), format!("Error: {PROJECT_TRUST_REFUSED}\n"));
    }
}

// Pi's `offlineMode`, with Bake's variable name: the flag, then
// `BAKE_OFFLINE`, read as Pi's `isTruthyEnvFlag` reads `PI_OFFLINE`.
#[test]
fn offline_comes_from_the_flag_or_the_environment() {
    let none = |_: &str| None;
    assert!(!offline_mode(&parse_args(&[]), none));
    assert!(offline_mode(&parse_args(&strings(&["--offline"])), none));
    let set = |name: &'static str, value: &'static str| {
        move |asked: &str| (asked == name).then(|| std::ffi::OsString::from(value.to_owned()))
    };
    for value in ["1", "true", "TRUE", "yes", " Yes "] {
        assert!(
            offline_mode(&parse_args(&[]), set(BAKE_OFFLINE_ENV, value)),
            "{value:?}"
        );
    }
    for value in ["", "   ", "0", "no", "false", "on"] {
        assert!(
            !offline_mode(&parse_args(&[]), set(BAKE_OFFLINE_ENV, value)),
            "{value:?}"
        );
    }
}

// Pi's `createSessionManager` reaches `--resume` only after
// `--no-session`, `--fork`, and `--session`, and `validateForkFlags`
// and `validateSessionIdFlags` refuse it beside `--fork` and
// `--session-id`. Print mode has no terminal for the picker.
#[test]
fn resume_opens_the_picker_only_in_an_interactive_session() {
    let (stderr, errors) = sink();
    assert!(matches!(
        plan(&strings(&["-r"]), true, true, &stderr),
        Action::Interactive(args) if args.resume
    ));
    assert!(matches!(
        plan(&strings(&["--resume", "--continue"]), true, true, &stderr),
        Action::Interactive(_)
    ));
    // Pi takes the other session choice and leaves `--resume` unused,
    // so print mode still runs.
    for flags in [
        vec!["-p", "-r", "--no-session"],
        vec!["-p", "-r", "--session", "abc"],
    ] {
        assert!(
            matches!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Print(..)
            ),
            "{flags:?}"
        );
    }
    assert_eq!(text(&errors), "");
    for flags in [
        vec!["-p", "-r"],
        vec!["--mode", "json", "--resume"],
        vec!["-r", "--fork", "a"],
        vec!["-r", "--session-id", "s1"],
    ] {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&flags), true, true, &stderr),
            Action::Exit(1),
            "{flags:?}"
        );
        assert!(text(&errors).starts_with("Error: "), "{flags:?}");
    }
}

#[test]
fn recovery_flags_require_one_persisted_selected_run() {
    for flags in [
        vec!["-p", "--recoverable", "--no-session", "task"],
        vec!["-p", "--recoverable", "first", "second"],
        vec!["-p", "--resume-run", "run-1"],
        vec!["-p", "--resume-run", "run-1", "--session", "prefix"],
        vec![
            "-p",
            "--resume-run",
            "run-1",
            "--session",
            "session.jsonl",
            "extra",
        ],
        vec![
            "-p",
            "--resume-run",
            "run-1",
            "--session",
            "session.jsonl",
            "--max-model-calls",
            "9",
        ],
        vec![
            "-p",
            "--resume-run",
            "run-1",
            "--session",
            "session.jsonl",
            "--recoverable",
        ],
        vec!["--recoverable", "task"],
    ] {
        let (stderr, errors) = sink();
        assert_eq!(
            plan(&strings(&flags), true, true, &stderr),
            Action::Exit(1),
            "{flags:?}"
        );
        assert!(!text(&errors).is_empty());
    }
    for flags in [
        vec!["-p", "--recoverable", "--max-model-calls", "9", "task"],
        vec!["-p", "--resume-run", "run-1", "--session", "session.jsonl"],
    ] {
        let (stderr, errors) = sink();
        assert!(
            matches!(
                plan(&strings(&flags), true, true, &stderr),
                Action::Print(_, _)
            ),
            "{}",
            text(&errors)
        );
    }
}

#[test]
fn argument_errors_exit_before_loading() {
    let (stderr, errors) = sink();
    assert_eq!(
        plan(&strings(&["-x"]), false, false, &stderr),
        Action::Exit(1)
    );
    assert_eq!(
        plan(
            &strings(&["--unknown-flag", "k", "-p"]),
            false,
            false,
            &stderr
        ),
        Action::Exit(1)
    );
    assert_eq!(
        plan(
            &strings(&["--fork", "a", "--continue", "-p"]),
            false,
            false,
            &stderr
        ),
        Action::Exit(1)
    );
    assert_eq!(
        plan(
            &strings(&["--session-id", "bad/id", "-p"]),
            false,
            false,
            &stderr
        ),
        Action::Exit(1)
    );
    assert_eq!(
        plan(&strings(&["--name", "   ", "-p"]), false, false, &stderr),
        Action::Exit(1)
    );
    assert_eq!(
        text(&errors),
        "Error: Unknown option: -x\nError: Unknown option: --unknown-flag\nError: --fork cannot be combined with --continue\nError: Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character\nError: --name requires a non-empty value\n"
    );
    assert_eq!(
        plan(&strings(&["-v", "-h"]), true, true, &stderr),
        Action::Version
    );
    assert_eq!(
        plan(&strings(&["-h", "-p"]), true, true, &stderr),
        Action::Help
    );
}

// Pi `test/initial-message.test.ts`: stdin comes first, then `@file` text,
// then the first message; later messages stay separate prompts.
#[test]
fn initial_message_joins_stdin_file_text_and_the_first_message() {
    let mut messages = strings(&["first", "second"]);
    assert_eq!(
        build_initial_message(Some("piped".into()), "", &mut messages).as_deref(),
        Some("pipedfirst")
    );
    assert_eq!(messages, ["second"]);
    let mut messages = strings(&["review"]);
    assert_eq!(
        build_initial_message(
            Some("stdin\n".into()),
            "<file name=\"a\">\nhi\n</file>\n",
            &mut messages,
        )
        .as_deref(),
        Some("stdin\n<file name=\"a\">\nhi\n</file>\nreview")
    );
    assert!(messages.is_empty());
    assert_eq!(build_initial_message(None, "", &mut Vec::new()), None);
    assert_eq!(
        read_piped_stdin(Box::new(&b"  hello \n"[..])),
        Ok(Some("hello".into()))
    );
    assert_eq!(read_piped_stdin(Box::new(&b" \n"[..])), Ok(None));
}

fn session_args(configure: impl FnOnce(&mut Args)) -> Args {
    let mut args = parse_args(&[]);
    configure(&mut args);
    args
}

/// A session file Pi would list: a header and one user message.
fn write_session(dir: &Path, id: &str, cwd: &str) -> PathBuf {
    let path = dir.join(format!("2026-01-01T00-00-00-000Z_{id}.jsonl"));
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": id,
        "timestamp": "2026-01-01T00:00:00.000Z", "cwd": cwd,
    });
    let message = serde_json::json!({
        "type": "message", "id": "a1", "parentId": null,
        "timestamp": "2026-01-01T00:00:01.000Z",
        "message": { "role": "user", "content": "hi", "timestamp": 1 },
    });
    crate::test_support::write(&path, &format!("{header}\n{message}\n"));
    path
}

// The session store follows the home the environment names, not the
// process's own Bake home.
#[test]
fn sessions_live_under_the_given_home() {
    let root = crate::test_support::TempDir::new("cli-home");
    let home = root.join("home");
    let cwd = root.join("project");
    std::fs::create_dir_all(&cwd).expect("project");
    let cwd = cwd.to_string_lossy().into_owned();
    let dir = default_session_dir_path(&cwd, &home);

    let mut warnings = Vec::new();
    let created = create_session_manager(&session_args(|_| {}), &cwd, None, &home, &mut warnings)
        .expect("create");
    assert_eq!(created.session_dir(), dir);
    drop(created);

    let file = write_session(&dir, "0193aaaa-0000-7000-8000-000000000000", &cwd);
    let continued = create_session_manager(
        &session_args(|args| args.continue_session = true),
        &cwd,
        None,
        &home,
        &mut warnings,
    )
    .expect("continue");
    assert_eq!(continued.session_file(), Some(file.as_path()));
    // The continued manager holds the file's writer lock until dropped.
    drop(continued);

    let by_prefix = create_session_manager(
        &session_args(|args| args.session = Some("0193aaaa".into())),
        &cwd,
        None,
        &home,
        &mut warnings,
    )
    .expect("by id");
    assert_eq!(by_prefix.session_file(), Some(file.as_path()));
    // A second writer of the held file is refused, by id and by
    // `--continue`, until the holder is dropped.
    let locked = Some(SessionStartError::Error(format!(
        "Session file is in use by another writer: {}",
        file.display()
    )));
    for args in [
        session_args(|args| args.session = Some("0193aaaa".into())),
        session_args(|args| args.continue_session = true),
    ] {
        assert_eq!(
            create_session_manager(&args, &cwd, None, &home, &mut warnings).err(),
            locked
        );
    }
    drop(by_prefix);

    // A session of another project, found under the same home.
    let other = root.join("other");
    let other = other.to_string_lossy().into_owned();
    write_session(
        &default_session_dir_path(&other, &home),
        "0193bbbb-0000-7000-8000-000000000000",
        &other,
    );
    assert_eq!(
        create_session_manager(
            &session_args(|args| args.session = Some("0193bbbb".into())),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .err(),
        Some(SessionStartError::Error(format!(
            "Session found in different project: {other}. Use --fork 0193bbbb to fork it into this directory."
        )))
    );
    let forked = create_session_manager(
        &session_args(|args| {
            args.fork = Some("0193bbbb".into());
            args.session_id = Some("forked-id".into());
        }),
        &cwd,
        None,
        &home,
        &mut warnings,
    )
    .expect("forked");
    assert_eq!(forked.session_id(), "forked-id");
    drop(forked);
    assert_eq!(
        create_session_manager(
            &session_args(|args| {
                args.fork = Some("0193bbbb".into());
                args.session_id = Some("forked-id".into());
            }),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .err(),
        Some(SessionStartError::Plain(
            "Session already exists with id 'forked-id'".into()
        ))
    );
    // Pi prints a missing id without the `Error: ` prefix.
    assert_eq!(
        create_session_manager(
            &session_args(|args| args.session = Some("ffff".into())),
            &cwd,
            None,
            &home,
            &mut warnings,
        )
        .err(),
        Some(SessionStartError::Plain(
            "No session found matching 'ffff'".into()
        ))
    );
}

// Pi `resolvePath(sessionArg, cwd)`: relative to `cwd`, `.` and `..`
// removed, `~/` expanded.
#[test]
fn session_paths_resolve_against_cwd() {
    let home = Path::new("/unused-home");
    let cwd = if cfg!(windows) {
        "C:\\work\\p"
    } else {
        "/work/p"
    };
    let expected = Path::new(cwd).join("s").join("x.jsonl");
    assert!(matches!(
        resolve_session_path("./t/../s/x.jsonl", cwd, None, home),
        ResolvedSession::Path(path) if path == expected
    ));
    if let Some(user) = std::env::home_dir() {
        assert!(matches!(
            resolve_session_path("~/x.jsonl", cwd, None, home),
            ResolvedSession::Path(path) if path == user.join("x.jsonl")
        ));
    }
}

#[test]
fn session_arguments_resolve_as_pi_resolves_them() {
    assert!(is_session_path("a/b"));
    assert!(is_session_path("a\\b"));
    assert!(is_session_path("x.jsonl"));
    assert!(!is_session_path("0193abc"));
}

// Pi v1.5.2's `main`: print and RPC mode refuse a session whose stored
// working directory is gone with Pi's bare message, before anything is
// written to it, `--name` included. Interactive mode, whose continue,
// fork, or cancel prompt is not ported, still opens it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_whose_working_directory_is_gone_is_refused_outside_interactive_mode() {
    let dir = setup_dirs("cli-missing-cwd");
    let gone = dir.join("gone").to_string_lossy().into_owned();
    let file = write_session(
        &dir.join("saved"),
        "0193cccc-0000-7000-8000-000000000000",
        &gone,
    );
    let written = std::fs::read(&file).expect("read");
    let path = file.to_string_lossy().into_owned();
    let args = || {
        parse_args(&strings(&[
            "--session",
            &path,
            "--name",
            "renamed",
            "--permission",
            "danger-full-access",
            "--no-mcp",
        ]))
    };
    let expected = format!(
        "Stored session working directory does not exist: {gone}\nSession file: {path}\nCurrent working directory: {}\n",
        dir.join("work").display()
    );
    for kind in [StartKind::Print, StartKind::Rpc] {
        let (environment, stderr) = setup_environment(dir.path());
        let refused = start_with(environment, args(), kind).await.err();
        assert_eq!(refused, Some(1), "{kind:?}");
        assert_eq!(text(&stderr), expected, "{kind:?}");
        assert_eq!(std::fs::read(&file).expect("read"), written, "{kind:?}");
    }
    let (environment, stderr) = setup_environment(dir.path());
    let Ok(started) = start_interactive(environment, args()).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    assert_eq!(started.session.session_file(), Some(file.clone()));
    assert_eq!(started.session.cwd(), gone);
    started.session.shutdown().await;
}

#[test]
fn text_from_a_file_or_server_is_one_terminal_safe_line() {
    assert_eq!(terminal_safe("plain \"name\" ok"), "plain \"name\" ok");
    assert_eq!(
        terminal_safe("a\u{1b}[31mb\nWarning: c\r\u{7}\u{9b}"),
        "a\\u{1b}[31mb\\u{a}Warning: c\\u{d}\\u{7}\\u{9b}"
    );
    assert_eq!(terminal_safe("x\u{202e}y\u{2066}"), "x\\u{202e}y\\u{2066}");
}

/// A directory tree for one setup: `home` (the Bake home), `work` (the
/// working directory), and `user` (the user's home), private on Unix.
fn setup_dirs(name: &str) -> crate::test_support::TempDir {
    let dir = crate::test_support::TempDir::new(name);
    for sub in ["home", "work", "user"] {
        std::fs::create_dir_all(dir.join(sub)).expect("setup directory");
    }
    #[cfg(unix)]
    for sub in ["", "home", "work", "user"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path().join(sub), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    dir
}

/// Writes `text` at `path`, writable by its owner only, whatever the
/// umask.
fn write_owned(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
}

fn setup_environment(dir: &Path) -> (CliEnvironment, Arc<Mutex<Vec<u8>>>) {
    let (stderr, buffer) = sink();
    let environment = CliEnvironment {
        cwd: dir.join("work"),
        home: Some(dir.join("home")),
        user_home: Some(dir.join("user")),
        docs_command: "bake docs".into(),
        stdin_is_terminal: true,
        stdin: Box::new(std::io::empty()),
        stdout: sink().0,
        stderr,
        extra_providers: Vec::new(),
        sandbox_provider: Arc::new(bake_sandbox::provider::UnavailableProvider::new(None)),
        approver: None,
        warnings: Vec::new(),
        interrupt: no_interrupt(),
        plugins: WasmSetup::default(),
        // Not the environment the tests run under, which may be a
        // spawned agent's: no telecom choice, callsign, or spawn depth,
        // and the team directory in the test's own directory, kept
        // short for the sockets' path limit.
        telecom_env: crate::telecom::TelecomEnv {
            dir: Some(dir.join("t")),
            ..crate::telecom::TelecomEnv::default()
        },
    };
    (environment, buffer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_opt_in_obeys_cli_and_configured_tool_restrictions() {
    use bake_ai::providers::faux::{
        FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_message,
    };
    for (extra, defaults, expected) in [
        (vec![], None, true),
        (vec!["--no-tools"], None, false),
        (vec!["--no-builtin-tools"], None, false),
        (vec!["--tools", "read"], None, false),
        (vec!["--exclude-tools", "task_document"], None, false),
        (vec![], Some(serde_json::json!(["read"])), false),
        (
            vec!["--tools", "+task_document"],
            Some(serde_json::json!([])),
            true,
        ),
    ] {
        let dir = setup_dirs("cli-document-selection");
        let mut settings = serde_json::json!({"taskDocuments":true});
        if let Some(defaults) = defaults {
            settings["defaultTools"] = defaults;
        }
        write_owned(&dir.join("home/settings.json"), &settings.to_string());
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
            assert_eq!(
                bake_ai::transcript::get_current_tools(context.messages())
                    .iter()
                    .any(|tool| tool.name == "task_document"),
                expected
            );
            Ok(faux_assistant_message("done"))
        })]);
        let (mut environment, errors) = setup_environment(dir.path());
        environment.extra_providers.push((
            "faux".into(),
            serde_json::json!({
                "baseUrl":"http://127.0.0.1:9/unused", "api":faux.model().api,
                "models":[{"id":"faux-1"}]
            }),
            Some("test-key".into()),
        ));
        let mut args = vec![
            "--model",
            "faux/faux-1",
            "--permission",
            "danger-full-access",
            "--no-session",
            "--no-mcp",
        ];
        args.extend(extra);
        let Ok(started) = start_interactive(environment, parse_args(&strings(&args))).await else {
            panic!("setup failed: {}", text(&errors));
        };
        started.session.apis().register(faux.clone());
        started
            .session
            .prompt("Continue", Vec::new())
            .await
            .unwrap();
        assert_eq!(faux.call_count(), 1);
        started.session.shutdown().await;
    }
}

// The first request of a session with many skills carries the list
// within `skills.listingTokens`, 4000 when unset: every skill, in order,
// the notice, and the shortened descriptions; an invalid value stops the
// start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_request_carries_the_budgeted_skills_list() {
    use bake_ai::providers::faux::{
        FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_message,
    };
    for (settings, valid) in [("{}", true), (r#"{"skills":{"listingTokens":0}}"#, false)] {
        let dir = setup_dirs("cli-skills-listing");
        write_owned(&dir.join("home/settings.json"), settings);
        let description = ["Use this skill for a task of its kind."; 12].join(" ");
        for index in 0..30 {
            let skill = dir
                .path()
                .join(format!("user/.agents/skills/skill-{index:02}"));
            std::fs::create_dir_all(&skill).expect("skill dir");
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: skill-{index:02}\ndescription: {description}\n---\nBody.\n"),
            )
            .expect("skill");
        }
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        let section = Arc::new(Mutex::new(String::new()));
        let seen = Arc::clone(&section);
        faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
            let prompt = bake_ai::transcript::get_current_system_prompt(context.messages());
            let start = prompt.find("<skills>").expect("skills");
            let end = prompt.find("</skills>").expect("end") + "</skills>".len();
            *seen.lock().unwrap() = prompt[start..end].to_owned();
            Ok(faux_assistant_message("done"))
        })]);
        let (mut environment, errors) = setup_environment(dir.path());
        environment.extra_providers.push((
            "faux".into(),
            serde_json::json!({
                "baseUrl":"http://127.0.0.1:9/unused", "api":faux.model().api,
                "models":[{"id":"faux-1"}]
            }),
            Some("test-key".into()),
        ));
        let args = [
            "--model",
            "faux/faux-1",
            "--permission",
            "danger-full-access",
            "--no-session",
            "--no-mcp",
        ];
        let started = start_interactive(environment, parse_args(&strings(&args))).await;
        if !valid {
            assert!(started.is_err());
            assert!(
                text(&errors)
                    .contains("Invalid skills.listingTokens setting: expected a positive integer."),
                "{}",
                text(&errors)
            );
            continue;
        }
        let Ok(started) = started else {
            panic!("setup failed: {}", text(&errors));
        };
        started.session.apis().register(faux.clone());
        started.session.prompt("Go", Vec::new()).await.unwrap();
        started.session.shutdown().await;
        let section = section.lock().unwrap().clone();
        assert!(
            section.contains(crate::skills::SHORTENED_NOTICE),
            "{section}"
        );
        let names: Vec<usize> = (0..30)
            .map(|index| {
                section
                    .find(&format!("<name>skill-{index:02}</name>"))
                    .expect("every skill listed")
            })
            .collect();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "in order");
        assert!(section.contains("…</description>"), "{section}");
        assert!(!section.contains(&format!("<description>{description}</description>")));
        assert!(bake_ai::utils::estimate::estimate_text_tokens(&section) <= 4000 + 5);
    }
}

// A model profile's `tools` apply after `defaultTools`; `--tools`
// chooses the tools alone; and a profile naming no tool Bake has stops
// the start with the reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_profile_tools_follow_default_tools_and_yield_to_tools() {
    use bake_ai::providers::faux::{
        FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_message,
    };
    for (extra, tools, expected) in [
        (vec![], "[\"+grep\", \"-write\"]", Ok(vec!["read", "grep"])),
        (
            vec!["--tools", "read,write"],
            "[\"+grep\", \"-write\"]",
            Ok(vec!["read", "write"]),
        ),
        (
            vec![],
            "[\"+nope\"]",
            Err("Invalid modelProfiles[\"faux/faux-1\"].tools setting: unknown tool \"nope\"."),
        ),
    ] {
        let dir = setup_dirs("cli-model-profile-tools");
        let settings = format!(
            r#"{{"defaultTools":["read","write"],"modelProfiles":{{"faux/faux-1":{{"tools":{tools}}}}}}}"#
        );
        write_owned(&dir.join("home/settings.json"), &settings);
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        let declared = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&declared);
        faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
            *seen.lock().unwrap() = bake_ai::transcript::get_current_tools(context.messages())
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            Ok(faux_assistant_message("done"))
        })]);
        let (mut environment, errors) = setup_environment(dir.path());
        environment.extra_providers.push((
            "faux".into(),
            serde_json::json!({
                "baseUrl":"http://127.0.0.1:9/unused", "api":faux.model().api,
                "models":[{"id":"faux-1"}]
            }),
            Some("test-key".into()),
        ));
        let mut args = vec![
            "--model",
            "faux/faux-1",
            "--permission",
            "danger-full-access",
            "--no-session",
            "--no-mcp",
        ];
        args.extend(extra);
        let started = start_interactive(environment, parse_args(&strings(&args))).await;
        match expected {
            Ok(expected) => {
                let Ok(started) = started else {
                    panic!("setup failed: {}", text(&errors));
                };
                started.session.apis().register(faux.clone());
                started.session.prompt("Go", Vec::new()).await.unwrap();
                assert_eq!(*declared.lock().unwrap(), expected);
                started.session.shutdown().await;
            }
            Err(reason) => {
                assert!(started.is_err());
                assert!(text(&errors).contains(reason), "{}", text(&errors));
            }
        }
    }
}

// What the model is told about who can answer, and the tools that
// follow from it: with an approver the session is interactive and
// declares `ask_user` and, unless the `visualize` setting is false,
// `visualize`; without one, as in `-p`, neither. With `subagents`
// and `telecom` both on, the rules say which way of delegating is for
// what.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prompt_states_who_can_answer_and_how_to_delegate() {
    use bake_ai::providers::faux::{
        FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_message,
    };
    const INTERACTIVE: &str = "<run>\nThis is an interactive session.";
    const PRINT: &str = "<run>\nThis is a non-interactive run:";
    for (answerer, team) in [(false, false), (true, false), (true, true)] {
        let dir = setup_dirs("cli-run-mode");
        // The team run also turns the `visualize` setting off.
        let settings =
            serde_json::json!({ "subagents": team, "telecom": team, "visualize": !team });
        write_owned(&dir.join("home/settings.json"), &settings.to_string());
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        let declared = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&declared);
        faux.set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
            *seen.lock().unwrap() = bake_ai::transcript::get_current_tools(context.messages())
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            Ok(faux_assistant_message("done"))
        })]);
        let (mut environment, errors) = setup_environment(dir.path());
        environment.extra_providers.push((
            "faux".into(),
            serde_json::json!({
                "baseUrl":"http://127.0.0.1:9/unused", "api":faux.model().api,
                "models":[{"id":"faux-1"}]
            }),
            Some("test-key".into()),
        ));
        if answerer {
            environment.approver = Some(Arc::new(crate::permissions::NeverApprover));
        }
        let args = [
            "--model",
            "faux/faux-1",
            "--permission",
            "danger-full-access",
            "--no-session",
            "--no-mcp",
        ];
        let Ok(started) = start_interactive(environment, parse_args(&strings(&args))).await else {
            panic!("setup failed: {}", text(&errors));
        };
        started.session.apis().register(faux.clone());
        started
            .session
            .prompt("Continue", Vec::new())
            .await
            .unwrap();
        let head = started.session.messages()[0]
            .as_system()
            .cloned()
            .expect("system");
        let system = bake_ai::utils::text::get_system_message_text(&head);
        let declared = declared.lock().unwrap().clone();
        let has = |name: &str| declared.iter().any(|tool| tool == name);
        assert_eq!(system.contains(INTERACTIVE), answerer, "{system}");
        assert_eq!(system.contains(PRINT), !answerer, "{system}");
        assert_eq!(has("ask_user"), answerer, "{declared:?}");
        assert_eq!(has("visualize"), answerer && !team, "{declared:?}");
        assert_eq!(has("subagent") && has("spawn_agent"), team, "{declared:?}");
        assert_eq!(
            system.contains(&format!("- {}\n", crate::spawned::WITH_SUBAGENTS_GUIDELINE)),
            team,
            "{system}"
        );
        started.session.shutdown().await;
    }
}

// Every tool Bake itself registers declares how it is scheduled, by
// name: with every opt-in on, each composed tool has the effect and mode
// of this table. The session's default (`Exec` for a tool that declares
// nothing) only ever produces `Exec`, so a tool here that is not `Exec`
// did not rely on it; the `Exec` ones declare it in their constructors
// (`bash` in `tests/pi_tools`, `task_document` and `schedule_*` in
// `agent_session::tests`). A new tool must be added to the table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_bake_tool_declares_its_scheduling() {
    use bake_agent::ToolEffect::{Exec, Read, Write};
    use bake_agent::ToolExecutionMode::{Parallel, Sequential};
    use bake_ai::providers::faux::{FauxProvider, FauxProviderOptions, faux_assistant_message};
    let expected = [
        ("read", Some(Read), None),
        ("bash", Some(Exec), None),
        ("edit", Some(Write), None),
        ("write", Some(Write), None),
        ("grep", Some(Read), None),
        ("find", Some(Read), None),
        ("ls", Some(Read), None),
        ("job_output", None, Some(Parallel)),
        ("job_list", None, Some(Parallel)),
        ("job_kill", None, Some(Parallel)),
        ("web_search", Some(Read), None),
        ("web_fetch", Some(Read), None),
        ("visualize", Some(Read), None),
        ("codemode", None, Some(Parallel)),
        ("ask_user", Some(Read), None),
        ("telecom", None, Some(Parallel)),
        ("spawn_agent", None, Some(Parallel)),
        ("stop_agent", None, Some(Parallel)),
        ("subagent", None, Some(Parallel)),
        ("send_message", None, Some(Parallel)),
        ("interrupt_agent", None, Some(Parallel)),
        ("list_agents", Some(Read), None),
        ("list_subagent_models", Some(Read), None),
        ("task_document", Some(Exec), Some(Sequential)),
        ("schedule_create", Some(Exec), Some(Sequential)),
        ("schedule_list", Some(Exec), Some(Sequential)),
        ("schedule_delete", Some(Exec), Some(Sequential)),
    ];
    let dir = setup_dirs("cli-tool-scheduling");
    let settings = serde_json::json!({
        "subagents": true,
        "telecom": true,
        "visualize": true,
        "schedules": true,
        "taskDocuments": true,
    });
    write_owned(&dir.join("home/settings.json"), &settings.to_string());
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    faux.set_responses(vec![faux_assistant_message("done").into()]);
    let (mut environment, errors) = setup_environment(dir.path());
    // Two models, so `list_subagent_models` is composed too.
    environment.extra_providers.push((
        "faux".into(),
        serde_json::json!({
            "baseUrl": "http://127.0.0.1:9/unused", "api": faux.model().api,
            "models": [{ "id": "faux-1" }, { "id": "faux-2" }]
        }),
        Some("test-key".into()),
    ));
    environment.approver = Some(Arc::new(crate::permissions::NeverApprover));
    let args = [
        "--model",
        "faux/faux-1",
        "--permission",
        "danger-full-access",
        "--no-session",
        "--no-mcp",
        "--tools",
        "*",
    ];
    let Ok(started) = start_interactive(environment, parse_args(&strings(&args))).await else {
        panic!("setup failed: {}", text(&errors));
    };
    started.session.apis().register(faux.clone());
    started.session.prompt("go", Vec::new()).await.unwrap();
    let mut composed: Vec<_> = started
        .session
        .agent()
        .tools()
        .iter()
        .map(|tool| (tool.name.clone(), tool.effect, tool.execution_mode))
        .collect();
    started.session.shutdown().await;
    let mut expected: Vec<_> = expected
        .iter()
        .map(|(name, effect, mode)| ((*name).to_owned(), *effect, *mode))
        .collect();
    composed.sort_by(|a, b| a.0.cmp(&b.0));
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(composed, expected);
}

// `bash.foregroundSeconds` reaches `bash`: its description names the
// configured budget, and an invalid value stops the start with the
// reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_foreground_budget_setting_reaches_bash() {
    use bake_ai::providers::faux::{FauxProvider, FauxProviderOptions, faux_assistant_message};
    for (value, expected) in [
        (
            serde_json::json!(45),
            Ok("a command still running after 45 seconds continues"),
        ),
        (
            serde_json::json!(null),
            Ok("a command still running after 300 seconds continues"),
        ),
        (
            serde_json::json!(0),
            Err("Invalid bash.foregroundSeconds setting: expected a positive integer."),
        ),
    ] {
        let dir = setup_dirs("cli-foreground-budget");
        let settings = if value.is_null() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "bash": { "foregroundSeconds": value } })
        };
        write_owned(&dir.join("home/settings.json"), &settings.to_string());
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        faux.set_responses(vec![faux_assistant_message("done").into()]);
        let (mut environment, errors) = setup_environment(dir.path());
        environment.extra_providers.push((
            "faux".into(),
            serde_json::json!({
                "baseUrl": "http://127.0.0.1:9/unused", "api": faux.model().api,
                "models": [{ "id": "faux-1" }]
            }),
            Some("test-key".into()),
        ));
        let args = [
            "--model",
            "faux/faux-1",
            "--permission",
            "danger-full-access",
            "--no-session",
            "--no-mcp",
        ];
        let started = start_interactive(environment, parse_args(&strings(&args))).await;
        match (started, expected) {
            (Ok(started), Ok(sentence)) => {
                started.session.apis().register(faux.clone());
                started.session.prompt("go", Vec::new()).await.unwrap();
                let bash = started
                    .session
                    .agent()
                    .tools()
                    .into_iter()
                    .find(|tool| tool.name == "bash")
                    .expect("bash");
                assert!(bash.description.contains(sentence), "{}", bash.description);
                started.session.shutdown().await;
            }
            (Err(_), Err(reason)) => {
                assert!(text(&errors).contains(reason), "{}", text(&errors));
            }
            (started, expected) => {
                panic!(
                    "{value}: started {}, expected {expected:?}: {}",
                    started.is_ok(),
                    text(&errors)
                )
            }
        }
    }
}

// Interactive setup and every replacement session share print mode's
// consumer: each reads the home's `mcp.json` and starts its servers,
// and `--no-mcp` starts none. The server's program does not exist, so
// nothing runs; under `danger-full-access` its admission needs no
// backend.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_and_replacement_sessions_start_the_homes_servers() {
    use crate::mcp::McpServerState;
    let dir = setup_dirs("cli-mcp-consumer");
    let missing = dir.join("no-such-program");
    let config = serde_json::json!({ "mcpServers": {
            "absent": { "command": missing.to_string_lossy() },
            "off": { "command": "x", "enabled": false },
            // Refused by the egress policy without any lookup or
            // connection: a public literal over plain http.
            "remote": { "url": "http://93.184.216.34/mcp" },
            "bad\u{1b}name": { "command": "x" },
        } });
    write_owned(&dir.join("home").join("mcp.json"), &config.to_string());
    let args = |extra: &[&str]| {
        let mut args = vec!["--sandbox", "danger-full-access", "--no-session"];
        args.extend_from_slice(extra);
        parse_args(&strings(&args))
    };

    let (environment, stderr) = setup_environment(dir.path());
    let Ok(started) = start_interactive(environment, args(&[])).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    let warnings = text(&stderr);
    assert!(warnings.contains("Warning: MCP: "), "{warnings}");
    assert!(
        warnings.contains("invalid server name \"bad\\u{1b}name\""),
        "{warnings}"
    );
    assert!(!warnings.contains('\u{1b}'), "{warnings}");
    let wait = |servers: crate::mcp::McpServers| async move {
        servers.wait_for_startup(|| false).await;
        servers.statuses()
    };
    let servers = started
        .session
        .mcp_servers()
        .expect("servers started")
        .clone();
    let statuses = wait(servers).await;
    let names: Vec<_> = statuses.iter().map(|status| status.name.as_str()).collect();
    assert_eq!(names, ["absent", "off", "remote"]);
    assert!(
        matches!(&statuses[0].state, McpServerState::Failed { error } if error.contains("failed to start")),
        "{statuses:?}"
    );
    assert_eq!(statuses[1].state, McpServerState::Disabled);
    assert!(
        matches!(&statuses[2].state, McpServerState::Refused { reason } if reason.contains("clear text")),
        "{statuses:?}"
    );

    // A replacement starts no server while the session it replaces
    // runs; the caller starts them once that session is shut down.
    let mut replacement = started
        .factory
        .replace(&started.session, SessionTarget::New)
        .await
        .expect("replacement");
    assert!(replacement.session.mcp_servers().is_none());
    assert!(replacement.has_pending_servers());
    started.session.shutdown().await;
    let old = started
        .session
        .mcp_servers()
        .expect("old servers")
        .statuses();
    assert!(
        old.iter().all(|status| matches!(
            status.state,
            McpServerState::Failed { .. }
                | McpServerState::Disabled
                | McpServerState::Refused { .. }
                | McpServerState::Closed
        )),
        "{old:?}"
    );
    replacement.start_servers().expect("start");
    assert!(!replacement.has_pending_servers());
    let servers = replacement
        .session
        .mcp_servers()
        .expect("replacement servers")
        .clone();
    assert_eq!(wait(servers).await.len(), 3);
    // Once only.
    replacement.start_servers().expect("nothing left to start");
    replacement.session.shutdown().await;

    // A replacement shut down unused never starts its servers.
    let (environment, stderr) = setup_environment(dir.path());
    let Ok(started) = start_interactive(environment, args(&[])).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    let mut unused = started
        .factory
        .replace(&started.session, SessionTarget::New)
        .await
        .expect("replacement");
    unused.session.shutdown().await;
    assert!(unused.start_servers().is_err());
    assert!(unused.session.mcp_servers().is_none());
    started.session.shutdown().await;

    let (environment, stderr) = setup_environment(dir.path());
    let Ok(started) = start_interactive(environment, args(&["--no-mcp"])).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    assert!(started.session.mcp_servers().is_none());
    assert_eq!(text(&stderr), "");
    let replacement = started
        .factory
        .replace(&started.session, SessionTarget::New)
        .await
        .expect("replacement");
    assert!(replacement.session.mcp_servers().is_none());
    replacement.session.shutdown().await;
    started.session.shutdown().await;

    // `--no-tools` alone leaves nothing an MCP tool could be declared
    // as, so no server starts; with `--tools` naming one, they do.
    for (extra, starts) in [
        (&["-nt"][..], false),
        (&["-nt", "-t", "mcp__absent__x"][..], true),
    ] {
        let (environment, stderr) = setup_environment(dir.path());
        let Ok(started) = start_interactive(environment, args(extra)).await else {
            panic!("setup failed: {}", text(&stderr));
        };
        assert_eq!(started.session.mcp_servers().is_some(), starts, "{extra:?}");
        started.session.shutdown().await;
    }
}

// Every session setup builds runs the hooks files `settings.json`
// names, and warns about a file it cannot use. A steered prompt runs
// `UserPromptSubmit` without a model, so the hook's block shows.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_run_the_hooks_files_settings_names() {
    let dir = setup_dirs("cli-hooks");
    let hooks = serde_json::json!({ "hooks": { "UserPromptSubmit": [{ "hooks": [
            { "type": "command", "command": "echo 'not today' >&2; exit 2" },
        ] }] } });
    write_owned(&dir.join("user").join("claude.json"), &hooks.to_string());
    write_owned(
        &dir.join("home").join("settings.json"),
        &serde_json::json!({ "hooks": { "claudeCode": "~/claude.json", "codex": "missing.json" } })
            .to_string(),
    );
    let args = parse_args(&strings(&[
        "--sandbox",
        "danger-full-access",
        "--no-session",
    ]));
    let (environment, stderr) = setup_environment(dir.path());
    let Ok(started) = start_interactive(environment, args).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    let warnings = text(&stderr);
    assert!(
        warnings.contains(&format!(
            "Warning: Codex hooks: {}: not found. None of its hooks run",
            dir.join("home").join("missing.json").display()
        )),
        "{warnings}"
    );
    let blocked = Err("Prompt blocked by UserPromptSubmit hook: not today".to_owned());
    assert_eq!(started.session.steer("hello", Vec::new()).await, blocked);
    let replacement = started
        .factory
        .replace(&started.session, SessionTarget::New)
        .await
        .expect("replacement");
    assert_eq!(
        replacement.session.steer("hello", Vec::new()).await,
        blocked
    );
    replacement.session.shutdown().await;
    started.session.shutdown().await;
}

// A file the session's own tools could change is refused whole, before
// any server starts.
// Where who may change `mcp.json` cannot be shown yet, the shared
// consumer refuses it unread and starts nothing.
#[cfg(not(unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unchecked_platform_starts_no_server() {
    let dir = setup_dirs("cli-mcp-unchecked");
    write_owned(
        &dir.join("home").join("mcp.json"),
        r#"{"mcpServers":{"a":{"command":"x"}}}"#,
    );
    let (environment, stderr) = setup_environment(dir.path());
    let args = parse_args(&strings(&[
        "--sandbox",
        "danger-full-access",
        "--no-session",
    ]));
    let Ok(started) = start_interactive(environment, args).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    assert!(started.session.mcp_servers().is_none());
    assert!(
        text(&stderr).contains(crate::mcp::home::UNCHECKED_PLATFORM),
        "{}",
        text(&stderr)
    );
    started.session.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_home_in_the_working_directory_starts_no_server() {
    let dir = setup_dirs("cli-mcp-workspace-home");
    let (mut environment, stderr) = setup_environment(dir.path());
    let home = dir.join("work").join(".bake");
    std::fs::create_dir_all(&home).expect("home");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    write_owned(
        &home.join("mcp.json"),
        r#"{"mcpServers":{"a":{"command":"x"}}}"#,
    );
    environment.home = Some(home);
    let args = parse_args(&strings(&[
        "--sandbox",
        "danger-full-access",
        "--no-session",
    ]));
    let Ok(started) = start_interactive(environment, args).await else {
        panic!("setup failed: {}", text(&stderr));
    };
    assert!(started.session.mcp_servers().is_none());
    let warnings = text(&stderr);
    assert!(
        warnings.contains("inside the session's working directory")
            && warnings.contains("No MCP server was started from it"),
        "{warnings}"
    );
    started.session.shutdown().await;
}
