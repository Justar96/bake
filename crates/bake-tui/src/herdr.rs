//! Native Herdr lifecycle reports, following
//! <https://herdr.dev/docs/add-herdr-support/> (Herdr 0.9.5).
//! One worker owns the CLI child, coalesces pending reports, and releases
//! the pane on shutdown. No subprocess or socket I/O runs on the UI thread.

use std::ffi::OsString;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bake_coding_agent::agent_session::AgentSession;
use bake_coding_agent::cli::args::parse_args;
use bake_tui_view::runtime::RuntimeUpdate;

const SOURCE: &str = "bake";
const TIMEOUT: Duration = Duration::from_millis(500);

struct Config {
    binary: OsString,
    socket: OsString,
    pane: String,
}

impl Config {
    fn resolve(env: impl Fn(&str) -> Option<OsString>) -> Option<Self> {
        if env("HERDR_ENV")?.to_str()? != "1" {
            return None;
        }
        let binary = env("HERDR_BIN_PATH").filter(|s| !s.is_empty())?;
        let socket = env("HERDR_SOCKET_PATH").filter(|s| !s.is_empty())?;
        let pane = env("HERDR_PANE_ID")?.into_string().ok()?;
        (!pane.is_empty()).then_some(Self {
            binary,
            socket,
            pane,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Session {
    path: Option<String>,
    model: Option<String>,
    thinking: String,
}

impl Session {
    fn read(session: &AgentSession) -> Self {
        Self {
            path: session
                .session_file()
                .and_then(|p| p.into_os_string().into_string().ok()),
            model: session.model().map(|m| format!("{}/{}", m.provider, m.id)),
            thinking: session.thinking_level().as_str().to_owned(),
        }
    }

    fn resume(&self, options: &[String]) -> Option<Vec<String>> {
        let path = self.path.as_ref()?;
        let mut args = vec!["bake".into(), "--session".into(), path.clone()];
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args.extend(["--thinking".into(), self.thinking.clone()]);
        args.extend_from_slice(options);
        // Herdr rejects the entire state report if resume_argv is invalid.
        // Keep reporting state even when a session path cannot be restored.
        valid_resume(&args).then_some(args)
    }
}

fn valid_resume(args: &[String]) -> bool {
    args.len() <= 64
        && args.iter().map(String::len).sum::<usize>() <= 8192
        && args
            .iter()
            .all(|s| !s.contains('\'') && !s.chars().any(char::is_control))
}

fn fresh_session(options: &[String]) -> Vec<String> {
    let mut args = vec!["bake".into(), "--no-session".into()];
    args.extend_from_slice(options);
    if valid_resume(&args) {
        args
    } else {
        // Herdr retains an earlier command when the new report omits one.
        // Replace it even when launch arguments cannot be represented.
        [
            "bake",
            "--no-session",
            "--no-tools",
            "--sandbox",
            "read-only",
            "--approval",
            "never",
            "--offline",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

/// Retain launch restrictions without copying prompts, API keys, or commands.
fn resume_options(args: &[String]) -> Vec<String> {
    let args = parse_args(args);
    let mut options = Vec::new();
    for (flag, value) in [
        ("--permission", args.permission),
        ("--sandbox", args.sandbox.map(|s| s.as_str().to_owned())),
        ("--approval", args.approval.map(|a| a.as_str().to_owned())),
        ("--tools", args.tools.map(|t| t.join(","))),
        ("--exclude-tools", args.exclude_tools.map(|t| t.join(","))),
    ] {
        if let Some(value) = value {
            options.extend([flag.into(), value]);
        }
    }
    for (flag, enabled) in [
        ("--offline", args.offline),
        ("--inline", args.inline),
        ("--no-tools", args.no_tools),
        ("--no-builtin-tools", args.no_builtin_tools),
        ("--no-mcp", args.no_mcp),
        ("--no-skills", args.no_skills),
        ("--no-prompt-templates", args.no_prompt_templates),
        ("--no-context-files", args.no_context_files),
    ] {
        if enabled {
            options.push(flag.into());
        }
    }
    options
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Report {
    state: &'static str,
    session: Session,
}

#[derive(Default)]
struct Projection {
    running: bool,
    approvals: bool,
    dialog: bool,
    session: Session,
    last: Option<Report>,
}

impl Projection {
    fn observe(&mut self, update: &RuntimeUpdate, session: Option<Session>) -> Option<Report> {
        if let Some(session) = session {
            self.session = session;
        }
        match update {
            RuntimeUpdate::TurnStarted { .. } => self.running = true,
            RuntimeUpdate::TurnEnded(_) => self.running = false,
            RuntimeUpdate::Approvals(queue) => self.approvals = !queue.is_empty(),
            RuntimeUpdate::Dialog(dialog) => self.dialog = dialog.is_some(),
            RuntimeUpdate::Replaced(_) => {
                self.running = false;
                self.approvals = false;
                self.dialog = false;
            }
            RuntimeUpdate::Status(_) => {}
            _ => return None,
        }
        let report = Report {
            state: if self.approvals || self.dialog {
                "blocked"
            } else if self.running {
                "working"
            } else {
                "idle"
            },
            session: self.session.clone(),
        };
        if self.last.as_ref() == Some(&report) {
            return None;
        }
        self.last = Some(report.clone());
        Some(report)
    }
}

#[derive(Default)]
struct Mailbox {
    projection: Projection,
    pending: Option<Report>,
    closing: bool,
}

#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    ready: Condvar,
}

#[derive(Clone)]
pub(crate) struct Handle(Arc<Shared>);

impl Handle {
    pub(crate) fn observe(&self, update: &RuntimeUpdate, session: Option<&AgentSession>) {
        let session = session.map(Session::read);
        let mut mailbox = self
            .0
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !mailbox.closing
            && let Some(report) = mailbox.projection.observe(update, session)
        {
            mailbox.pending = Some(report);
            self.0.ready.notify_one();
        }
    }
}

pub(crate) struct Reporter {
    handle: Handle,
    worker: Option<JoinHandle<()>>,
}

impl Reporter {
    pub(crate) fn from_env() -> Option<Self> {
        let config = Config::resolve(|name| std::env::var_os(name))?;
        let options = resume_options(
            &std::env::args_os()
                .skip(1)
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        Self::start(
            move |args| {
                let mut command = Command::new(&config.binary);
                command.args(args).env("HERDR_SOCKET_PATH", &config.socket);
                let _ = invoke(&mut command, TIMEOUT);
            },
            config.pane,
            options,
        )
    }

    fn start(
        mut send: impl FnMut(&[String]) + Send + 'static,
        pane: String,
        options: Vec<String>,
    ) -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let handle = Handle(Arc::clone(&shared));
        let worker = thread::Builder::new()
            .name("bake-herdr".into())
            .spawn(move || {
                let mut seq = 0;
                loop {
                    let report = {
                        let mut mailbox = shared
                            .mailbox
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        while mailbox.pending.is_none() && !mailbox.closing {
                            mailbox = shared
                                .ready
                                .wait(mailbox)
                                .unwrap_or_else(PoisonError::into_inner);
                        }
                        if mailbox.closing {
                            None
                        } else {
                            mailbox.pending.take()
                        }
                    };
                    seq = next_seq(seq);
                    let args = arguments(&pane, report.as_ref(), &options, seq);
                    send(&args);
                    if report.is_none() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            handle,
            worker: Some(worker),
        })
    }

    pub(crate) fn handle(&self) -> Handle {
        self.handle.clone()
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        {
            let mut mailbox = self
                .handle
                .0
                .mailbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            mailbox.closing = true;
            mailbox.pending = None;
            self.handle.0.ready.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn next_seq(previous: u64) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    u64::try_from(now)
        .unwrap_or(u64::MAX)
        .max(previous.saturating_add(1))
}

fn arguments(pane: &str, report: Option<&Report>, options: &[String], seq: u64) -> Vec<String> {
    let mut args = vec![
        "pane".into(),
        if report.is_some() {
            "report-agent"
        } else {
            "release-agent"
        }
        .into(),
        pane.into(),
        "--source".into(),
        SOURCE.into(),
        "--agent".into(),
        "bake".into(),
        "--seq".into(),
        seq.to_string(),
    ];
    if let Some(report) = report {
        args.extend(["--state".into(), report.state.into()]);
        let resume = if let Some(resume) = report.session.resume(options) {
            args.extend([
                "--agent-session-path".into(),
                report.session.path.clone().unwrap_or_default(),
            ]);
            resume
        } else {
            fresh_session(options)
        };
        args.push("--".into());
        args.extend(resume);
    }
    args
}

/// Always reap the CLI child, including after a timeout or worker panic.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn invoke(command: &mut Command, timeout: Duration) -> std::io::Result<std::process::ExitStatus> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child = OwnedChild(child);
    let deadline = Instant::now() + timeout;
    loop {
        match child.0.try_wait()? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Herdr report timed out",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_tui_view::runtime::SessionStatus;
    use bake_tui_view::state::Outcome;
    use std::sync::mpsc;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }

    fn saved(path: &str) -> Session {
        Session {
            path: Some(path.into()),
            model: Some("openai/gpt-5".into()),
            thinking: "low".into(),
        }
    }

    fn status() -> RuntimeUpdate {
        RuntimeUpdate::Status(SessionStatus::default())
    }

    #[test]
    fn reporting_requires_the_complete_herdr_environment() {
        let names = [
            "HERDR_ENV",
            "HERDR_BIN_PATH",
            "HERDR_SOCKET_PATH",
            "HERDR_PANE_ID",
        ];
        let values = ["1", "/opt/herdr", "/tmp/herdr.sock", "w1:p2"];
        for missing in 0..names.len() {
            assert!(
                Config::resolve(|name| names
                    .iter()
                    .position(|n| *n == name)
                    .filter(|i| *i != missing)
                    .map(|i| values[i].into()))
                .is_none()
            );
        }
        let config = Config::resolve(|name| {
            names
                .iter()
                .position(|n| *n == name)
                .map(|i| values[i].into())
        })
        .unwrap();
        assert_eq!(config.pane, "w1:p2");
        assert_eq!(config.binary, "/opt/herdr");
        assert_eq!(config.socket, "/tmp/herdr.sock");
        for bad in ["", "0", "true"] {
            assert!(
                Config::resolve(|name| Some(if name == "HERDR_ENV" { bad } else { "x" }.into()))
                    .is_none()
            );
        }
        for empty in &names[1..] {
            assert!(
                Config::resolve(|name| Some(
                    if name == *empty {
                        ""
                    } else if name == "HERDR_ENV" {
                        "1"
                    } else {
                        "x"
                    }
                    .into()
                ))
                .is_none()
            );
        }
    }

    #[test]
    fn lifecycle_reports_wait_for_all_decisions_before_resuming_work() {
        use bake_tui_view::dialog::{DialogKind, DialogRequest, InfoDialog};
        use bake_tui_view::runtime::Approval;
        let mut projection = Projection::default();
        assert_eq!(projection.observe(&status(), None).unwrap().state, "idle");
        assert!(projection.observe(&status(), None).is_none());
        assert_eq!(
            projection
                .observe(
                    &RuntimeUpdate::TurnStarted {
                        seed: "turn".into()
                    },
                    None
                )
                .unwrap()
                .state,
            "working"
        );
        let approval = Approval {
            generation: 1,
            tool: "bash".into(),
            call_id: "1".into(),
            parent_call_id: None,
            reason: "access".into(),
            arguments: "".into(),
        };
        assert_eq!(
            projection
                .observe(&RuntimeUpdate::Approvals(vec![approval]), None)
                .unwrap()
                .state,
            "blocked"
        );
        let dialog = DialogRequest {
            id: 1,
            kind: DialogKind::Info(InfoDialog {
                title: "Question".into(),
                lines: vec![],
            }),
        };
        assert!(
            projection
                .observe(&RuntimeUpdate::Dialog(Some(dialog)), None)
                .is_none()
        );
        assert!(
            projection
                .observe(&RuntimeUpdate::Approvals(vec![]), None)
                .is_none()
        );
        assert_eq!(
            projection
                .observe(&RuntimeUpdate::Dialog(None), None)
                .unwrap()
                .state,
            "working"
        );
        assert_eq!(
            projection
                .observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), None)
                .unwrap()
                .state,
            "idle"
        );
        assert!(
            projection
                .observe(&RuntimeUpdate::Live(vec![]), None)
                .is_none()
        );
    }

    #[test]
    fn new_persisted_sessions_replace_the_restore_command() {
        let mut projection = Projection::default();
        assert_eq!(
            projection
                .observe(&status(), Some(saved("/tmp/first.jsonl")))
                .unwrap()
                .session
                .path
                .as_deref(),
            Some("/tmp/first.jsonl")
        );
        let next = projection
            .observe(
                &RuntimeUpdate::Replaced(vec![]),
                Some(saved("/tmp/second.jsonl")),
            )
            .unwrap();
        let args = arguments(
            "w1:p1",
            Some(&next),
            &strings(&["--permission", "read-only"]),
            45,
        );
        assert_eq!(
            args,
            strings(&[
                "pane",
                "report-agent",
                "w1:p1",
                "--source",
                "bake",
                "--agent",
                "bake",
                "--seq",
                "45",
                "--state",
                "idle",
                "--agent-session-path",
                "/tmp/second.jsonl",
                "--",
                "bake",
                "--session",
                "/tmp/second.jsonl",
                "--model",
                "openai/gpt-5",
                "--thinking",
                "low",
                "--permission",
                "read-only"
            ])
        );
        let unsaved = projection
            .observe(&status(), Some(Session::default()))
            .unwrap();
        assert!(
            arguments("w1:p1", Some(&unsaved), &[], 46).ends_with(&strings(&[
                "--",
                "bake",
                "--no-session"
            ]))
        );
        assert_eq!(
            arguments("w1:p1", None, &[], 47),
            strings(&[
                "pane",
                "release-agent",
                "w1:p1",
                "--source",
                "bake",
                "--agent",
                "bake",
                "--seq",
                "47"
            ])
        );
    }

    #[test]
    fn resume_keeps_restrictions_and_omits_credentials_and_prompts() {
        let options = resume_options(&strings(&[
            "--api-key",
            "secret",
            "--permission",
            "read-only",
            "--approval",
            "never",
            "--sandbox",
            "read-only",
            "--tools",
            "read,grep",
            "--no-mcp",
            "--inline",
            "--offline",
            "user prompt",
        ]));
        assert_eq!(
            options,
            strings(&[
                "--permission",
                "read-only",
                "--sandbox",
                "read-only",
                "--approval",
                "never",
                "--tools",
                "read,grep",
                "--offline",
                "--inline",
                "--no-mcp"
            ])
        );
    }

    #[test]
    fn invalid_resume_commands_do_not_suppress_state_reports() {
        for path in [
            "/tmp/user's/session.jsonl".to_owned(),
            "/tmp/line\nbreak".to_owned(),
            "x".repeat(8193),
        ] {
            let report = Report {
                state: "working",
                session: saved(&path),
            };
            assert!(report.session.resume(&[]).is_none());
            let args = arguments("w1:p1", Some(&report), &[], 1);
            assert_eq!(args[10], "working");
            assert!(args.ends_with(&strings(&["--", "bake", "--no-session"])));
        }
        assert!(saved("/tmp/ok").resume(&vec!["x".into(); 65]).is_none());
    }

    #[test]
    fn worker_coalesces_queued_states_and_releases_after_the_inflight_report() {
        let (sent, received) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let reporter = Reporter::start(
            move |args| {
                sent.send(args.to_vec()).unwrap();
                held.recv_timeout(Duration::from_secs(2)).unwrap();
            },
            "w1:p1".into(),
            vec![],
        )
        .unwrap();
        let handle = reporter.handle();
        handle.observe(&status(), None);
        let first = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first[10], "idle");
        handle.observe(
            &RuntimeUpdate::TurnStarted {
                seed: "turn".into(),
            },
            None,
        );
        handle.observe(&RuntimeUpdate::TurnEnded(Outcome::Completed), None);
        release.send(()).unwrap();
        let second = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(second[10], "idle");
        let closing = thread::spawn(move || drop(reporter));
        release.send(()).unwrap();
        let last = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(last[1], "release-agent");
        let seq = |args: &[String]| args[8].parse::<u64>().unwrap();
        assert!(seq(&first) < seq(&second) && seq(&second) < seq(&last));
        release.send(()).unwrap();
        closing.join().unwrap();
        handle.observe(&status(), None);
        assert!(received.recv().is_err());
    }

    #[test]
    fn sequence_numbers_increase_even_if_the_wall_clock_moves_back() {
        let now = next_seq(0);
        assert!(now > 0);
        let future = now + 1_000_000_000;
        assert_eq!(next_seq(future), future + 1);
    }

    #[test]
    fn an_unrestorable_session_replaces_any_old_command_and_keeps_restrictions() {
        let options = strings(&["--sandbox", "read-only", "--approval", "never"]);
        assert_eq!(
            fresh_session(&options),
            strings(&[
                "bake",
                "--no-session",
                "--sandbox",
                "read-only",
                "--approval",
                "never"
            ])
        );
        for options in [
            vec!["bad'argument".into()],
            vec!["x".repeat(8193)],
            vec!["x".into(); 65],
        ] {
            let args = fresh_session(&options);
            assert_eq!(
                args,
                strings(&[
                    "bake",
                    "--no-session",
                    "--no-tools",
                    "--sandbox",
                    "read-only",
                    "--approval",
                    "never",
                    "--offline"
                ])
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_or_stalled_cli_calls_end_and_the_child_is_reaped() {
        let status = invoke(
            Command::new("sh").args(["-c", "exit 7"]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(status.code(), Some(7));
        let started = Instant::now();
        let error = invoke(
            Command::new("sh").args(["-c", "exec sleep 60"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_an_owned_cli_child_kills_and_reaps_it() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        drop(OwnedChild(child));
        let mut status = 0;
        // SAFETY: pid is this test's child, and status is a valid output.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let error = std::io::Error::last_os_error().raw_os_error();
        if waited == 0 {
            // A failed cleanup assertion must not leave the test's child alive.
            // SAFETY: an unreaped child cannot have had its pid reused.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
        }
        assert_eq!(waited, -1, "the child remained owned after teardown");
        assert_eq!(error, Some(libc::ECHILD));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(96))]
        #[test]
        fn restore_arguments_always_satisfy_herdr_constraints(path in ".{0,200}") {
            if let Some(args) = saved(&path).resume(&[]) {
                proptest::prop_assert_eq!(&args[0], "bake");
                proptest::prop_assert!(args.len() <= 64);
                proptest::prop_assert!(args.iter().map(String::len).sum::<usize>() <= 8192);
                proptest::prop_assert!(args.iter().all(|a| !a.contains('\'') && !a.chars().any(char::is_control)));
            }
        }
    }
}
