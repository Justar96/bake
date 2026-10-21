//! The GitHub CLI, which `/share` and `/bug` upload through, as Pi's
//! `session-share.ts` runs it: `gh auth status` first, then the upload.
//!
//! Each run is a [`bake_coding_agent::exec::exec_command`] inside a
//! background task of the session ([`AgentSession::spawn_background`]),
//! so `gh` and every process it starts are one tree owned through the
//! session's work gate. The command awaits the task for `gh`'s result.
//! Its abort signal (Esc on its sheet, or the driver's shutdown, which
//! aborts it first) and a dropped run cancel the task: `exec` then kills
//! the tree and the task ends once the tree is gone. The session's
//! shutdown cancels the task too and returns only after it has ended, so
//! no upload outlives the session; a session already shutting down
//! refuses the run. Its stdin is closed, so `gh` never waits for an answer
//! from the terminal Bake holds, and it sees the person's whole
//! environment ([`ExecEnv::InheritAll`]): `GH_TOKEN` and `GITHUB_TOKEN`
//! reach it, as they would from their shell.

use std::ffi::{OsStr, OsString};

use bake_ai::AbortSignal;
use bake_coding_agent::agent_session::AgentSession;
use bake_coding_agent::exec::{ExecEnv, ExecOptions, ExecResult, exec_command};
use bake_coding_agent::owned_work::OwnedTask;
use bake_tui_view::copy;

/// How one run of `gh` ended.
#[derive(Debug)]
pub enum Ran {
    /// It exited, with this status code (`None` when a signal ended it),
    /// and printed this.
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// The command's abort signal fired; the child was killed.
    Cancelled,
}

/// Why `gh` could not run.
#[derive(Debug)]
pub enum RunError {
    /// No `gh` is on `PATH`.
    Missing,
    /// It could not be started or waited for.
    Failed(String),
}

impl RunError {
    /// What Pi says of a `gh` that cannot be run.
    pub fn message(&self) -> String {
        match self {
            Self::Missing => copy::SHARE_GH_MISSING.to_owned(),
            Self::Failed(error) => error.clone(),
        }
    }
}

/// The run's task, cancelled when the run is dropped part way.
struct Cancelling(OwnedTask<ExecResult>);

impl Drop for Cancelling {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Runs `program` with `args`, its output captured, until it exits or
/// `signal` fires, as background work of `session`.
pub async fn run(
    program: &OsStr,
    args: &[OsString],
    signal: &AbortSignal,
    session: &AgentSession,
) -> Result<Ran, RunError> {
    if signal.aborted() {
        return Ok(Ran::Cancelled);
    }
    let program = program.to_os_string();
    let args = args.to_vec();
    let task = session
        .spawn_background(move |scope| async move {
            // `gh` runs where Bake runs, as it did when spawned directly.
            exec_command(
                &program,
                &args,
                std::path::Path::new("."),
                ExecOptions {
                    signal: Some(scope.signal().clone()),
                    timeout: None,
                    owner: None,
                    env: ExecEnv::InheritAll,
                },
            )
            .await
        })
        .map_err(RunError::Failed)?;
    let mut task = Cancelling(task);
    let finished = tokio::select! {
        finished = &mut task.0 => finished,
        () = signal.cancelled() => {
            task.0.cancel();
            (&mut task.0).await
        }
    };
    let result = finished.map_err(|error| RunError::Failed(error.to_string()))?;
    if let Some(kind) = result.error {
        return Err(match kind {
            std::io::ErrorKind::NotFound => RunError::Missing,
            kind => RunError::Failed(
                result
                    .error_message
                    .unwrap_or_else(|| std::io::Error::from(kind).to_string()),
            ),
        });
    }
    // Killed by a cancel; one that came after `gh` exited by itself leaves
    // its result, which is what happened.
    if result.killed {
        return Ok(Ran::Cancelled);
    }
    Ok(Ran::Exited {
        code: if result.signal.is_some() {
            None
        } else {
            Some(i32::try_from(result.code).unwrap_or(i32::MAX))
        },
        stdout: result.stdout,
        stderr: result.stderr,
    })
}

/// Pi's check before an upload: `gh auth status` must succeed. `Ok(false)`
/// when the signal fired first.
pub async fn check_auth(
    program: &OsStr,
    signal: &AbortSignal,
    session: &AgentSession,
) -> Result<bool, String> {
    match run(program, &["auth".into(), "status".into()], signal, session).await {
        Ok(Ran::Exited { code: Some(0), .. }) => Ok(true),
        Ok(Ran::Exited { .. }) => Err(copy::SHARE_GH_NOT_LOGGED_IN.to_owned()),
        Ok(Ran::Cancelled) => Ok(false),
        Err(error) => Err(error.message()),
    }
}

/// A private directory for the files an upload reads, removed with
/// everything in it when dropped, as Pi removes its `pi-share-` directory.
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    /// A new directory under the system's temporary directory, readable
    /// only by this user. Blocking.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}",
            bake_coding_agent::session::id::uuid_v7()
        ));
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        builder.create(&path)?;
        Ok(Self(path))
    }

    /// The directory.
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Fake `gh` runs: a script under `/bin/sh` that starts a long-running
// descendant, reports both pids, and waits. Linux reads `/proc`.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use bake_ai::{AbortController, ApiRegistry};
    use bake_coding_agent::agent_session::AgentSessionOptions;
    use bake_coding_agent::auth_storage::AuthStorage;
    use bake_coding_agent::model_registry::ModelRegistry;
    use bake_coding_agent::model_registry::config::ModelConfig;
    use bake_coding_agent::session::{NewSessionOptions, SessionManager};
    use bake_coding_agent::settings::Settings;
    use bake_coding_agent::system_prompt::SystemPromptOptions;

    use super::*;

    const BOUND: Duration = Duration::from_secs(10);

    async fn session() -> Arc<AgentSession> {
        let manager =
            SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
                .expect("session");
        Arc::new(
            AgentSession::create(AgentSessionOptions {
                session: manager,
                model: None,
                registry: ModelRegistry::new(
                    ModelConfig::from_value(serde_json::json!({}), "models.json"),
                    AuthStorage::empty(),
                )
                .into(),
                settings_store: None,
                selection: None,
                apis: Arc::new(ApiRegistry::new()),
                settings: Settings::default(),
                thinking_level: None,
                tools: Vec::new(),
                active_tool_names: Vec::new(),
                tool_prompts: Vec::new(),
                system_prompt: SystemPromptOptions::new("/tmp"),
                docs_command: "bake docs".into(),
                plugins: bake_coding_agent::plugins::PluginRegistry::default(),
                work: None,
                jobs: None,
            })
            .await
            .expect("agent session"),
        )
    }

    /// A fake `gh` in a private directory: run under `/bin/sh`, so no test
    /// executes a file it has just written.
    struct FakeGh {
        dir: TempDir,
        script: PathBuf,
        pids: PathBuf,
    }

    impl FakeGh {
        /// A `gh` that starts `sleep` in the background, reports its own
        /// pid and the sleep's, and waits.
        fn holding() -> Self {
            let dir = TempDir::new("bake-gh-test-").expect("temp dir");
            let pids = dir.path().join("pids");
            let script = dir.path().join("gh");
            std::fs::write(
                &script,
                "sleep 1000 </dev/null >/dev/null 2>&1 &\n\
                 echo \"$$ $!\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"\n\
                 wait\n",
            )
            .expect("script");
            Self { dir, script, pids }
        }

        fn args(&self) -> Vec<OsString> {
            vec![
                self.script.clone().into_os_string(),
                self.pids.clone().into_os_string(),
            ]
        }

        /// The pids it reported once running.
        async fn started(&self) -> Vec<i32> {
            let deadline = Instant::now() + BOUND;
            loop {
                if let Ok(text) = std::fs::read_to_string(&self.pids) {
                    return text
                        .split_whitespace()
                        .map(|pid| pid.parse().expect("pid"))
                        .collect();
                }
                assert!(Instant::now() < deadline, "the fake gh did not start");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    fn listed(pid: i32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    fn run_in_background(
        session: &Arc<AgentSession>,
        gh: &FakeGh,
        signal: AbortSignal,
    ) -> tokio::task::JoinHandle<Result<Ran, RunError>> {
        let session = Arc::clone(session);
        let args = gh.args();
        tokio::spawn(async move { run(OsStr::new("/bin/sh"), &args, &signal, &session).await })
    }

    // Esc during a run kills `gh` and the process it started, and the run
    // returns only once both are gone: neither is left running or a
    // zombie.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn esc_kills_the_gh_tree_and_waits_for_it() {
        let session = session().await;
        let gh = FakeGh::holding();
        let abort = AbortController::new();
        let call = run_in_background(&session, &gh, abort.signal());
        let pids = gh.started().await;
        assert_eq!(pids.len(), 2, "{pids:?}");
        abort.abort();
        let ran = tokio::time::timeout(BOUND, call)
            .await
            .expect("the run ends")
            .expect("joined");
        assert!(matches!(ran, Ok(Ran::Cancelled)), "{ran:?}");
        for pid in pids {
            assert!(!listed(pid), "{pid} outlived the cancelled run");
        }
        session.shutdown().await;
        drop(gh.dir);
    }

    // The session's shutdown during a run cancels it and returns only
    // once the `gh` tree is gone; the run then reads as cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_shutdown_waits_for_the_gh_tree() {
        let session = session().await;
        let gh = FakeGh::holding();
        let abort = AbortController::new();
        let call = run_in_background(&session, &gh, abort.signal());
        let pids = gh.started().await;
        tokio::time::timeout(BOUND, session.shutdown())
            .await
            .expect("the shutdown ends");
        for pid in &pids {
            assert!(!listed(*pid), "{pid} outlived the session's shutdown");
        }
        let ran = tokio::time::timeout(BOUND, call)
            .await
            .expect("the run ends")
            .expect("joined");
        assert!(matches!(ran, Ok(Ran::Cancelled)), "{ran:?}");
        // A shut-down session refuses a later run.
        let refused = run(OsStr::new("/bin/sh"), &gh.args(), &abort.signal(), &session).await;
        assert!(
            matches!(&refused, Err(RunError::Failed(message)) if message == "The session is shut down."),
            "{refused:?}"
        );
    }

    // A dropped run (its command task dropped) is cancelled: the tree is
    // killed, and the session's shutdown still waits for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_run_is_killed_and_awaited_by_shutdown() {
        let session = session().await;
        let gh = FakeGh::holding();
        let abort = AbortController::new();
        let call = run_in_background(&session, &gh, abort.signal());
        let pids = gh.started().await;
        call.abort();
        let _ = call.await;
        tokio::time::timeout(BOUND, session.shutdown())
            .await
            .expect("the shutdown ends");
        for pid in pids {
            assert!(!listed(pid), "{pid} outlived a dropped run");
        }
    }

    // A run that exits returns its code and both streams, as `gh` printed
    // them; a missing program is Pi's missing `gh`; a signal death has no
    // code; and `check_auth` reads a nonzero exit as not logged in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exits_and_failures_read_as_before() {
        let session = session().await;
        let signal = AbortController::new().signal();
        let sh = |script: &str| vec![OsString::from("-c"), OsString::from(script)];
        let ran = run(
            OsStr::new("/bin/sh"),
            &sh("echo out; echo err >&2; exit 3"),
            &signal,
            &session,
        )
        .await;
        match ran {
            Ok(Ran::Exited {
                code,
                stdout,
                stderr,
            }) => {
                assert_eq!(code, Some(3));
                assert_eq!(stdout, "out\n");
                assert_eq!(stderr, "err\n");
            }
            other => panic!("{other:?}"),
        }
        let killed = run(OsStr::new("/bin/sh"), &sh("kill -9 $$"), &signal, &session).await;
        assert!(
            matches!(killed, Ok(Ran::Exited { code: None, .. })),
            "{killed:?}"
        );
        let missing = run(OsStr::new("/nonexistent/bake-gh"), &[], &signal, &session).await;
        assert!(matches!(missing, Err(RunError::Missing)), "{missing:?}");
        assert_eq!(
            check_auth(OsStr::new("/nonexistent/bake-gh"), &signal, &session).await,
            Err(copy::SHARE_GH_MISSING.to_owned())
        );
        assert_eq!(
            check_auth(OsStr::new("/bin/false"), &signal, &session).await,
            Err(copy::SHARE_GH_NOT_LOGGED_IN.to_owned())
        );
        assert_eq!(
            check_auth(OsStr::new("/bin/true"), &signal, &session).await,
            Ok(true)
        );
        let aborted = AbortController::new();
        aborted.abort();
        assert_eq!(
            check_auth(OsStr::new("/bin/true"), &aborted.signal(), &session).await,
            Ok(false)
        );
        session.shutdown().await;
    }

    // `ExecEnv::InheritAll` is for a person's own commands: in this crate
    // only this runner names it.
    #[test]
    fn only_the_gh_runner_inherits_the_whole_environment() {
        fn visit(dir: &Path, found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && std::fs::read_to_string(&path)
                        .expect("source")
                        .contains(concat!("Inherit", "All"))
                {
                    found.push(path);
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        visit(&src, &mut found);
        assert_eq!(found, [src.join("live/commands/gh.rs")]);
    }
}
