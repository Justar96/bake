//! A foreground `bash` command that outlives the foreground budget
//! continues as a background job ([`crate::tools::bash`]): the call
//! returns its output so far and the job's id, and the job is an ordinary
//! one from then on: listed, read without repeats, reported once, waited
//! for by print mode, and stopped with its process tree at shutdown. A
//! command that finishes in time, one with a `timeout`, an aborted one,
//! and a tool without jobs behave as before. Real shells with short
//! budgets; each test owns its directory, registry, and session.

use std::path::PathBuf;
use std::time::Duration;

use bake_agent::{AgentToolResult, AgentToolUpdateCallback, ToolContext};
use bake_ai::providers::faux::{faux_assistant_blocks, faux_assistant_message, faux_tool_call};
use bake_ai::{AbortController, StopReason};
use serde_json::{Value, json};

use super::tests::{Harness, harness, options};
use super::*;
use crate::jobs::{JobLimits, JobStatus};
use crate::owned_work::WorkGate;
use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode};
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;
use crate::tools::bash::{BashToolOptions, create_bash_tool};

const WAIT: Duration = Duration::from_secs(30);
/// Long enough that a shell started under load still writes its first
/// output in time.
const BUDGET: Duration = Duration::from_secs(2);

fn have_bash() -> bool {
    let found = crate::tools::bash::shell::get_shell_config(None).is_ok();
    if !found {
        eprintln!("skipping: no bash");
    }
    found
}

struct Fixture {
    dir: TempDir,
    h: Harness,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.join("ws")).expect("ws");
        Self { dir, h: harness() }
    }

    fn ws(&self) -> PathBuf {
        self.dir.join("ws")
    }

    fn bash_options(
        &self,
        jobs: Option<&JobRegistry>,
        owner: Option<&WorkGate>,
    ) -> BashToolOptions {
        BashToolOptions {
            spill_directory: Some(self.dir.path().to_path_buf()),
            owner: owner.cloned(),
            jobs: jobs.cloned(),
            foreground_budget: BUDGET,
            ..BashToolOptions::default()
        }
    }

    /// A persisted session whose `bash` has the session's jobs and
    /// [`BUDGET`]; `codemode` is active too when asked.
    async fn session(&self, codemode: bool) -> AgentSession {
        self.session_with(codemode, BUDGET).await
    }

    async fn session_with(&self, codemode: bool, budget: Duration) -> AgentSession {
        let store = SessionManager::create(
            &self.ws().to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("store");
        self.open(store, codemode, budget).await
    }

    /// A session over `store`, as [`Self::session_with`] builds one.
    async fn open(&self, store: SessionManager, codemode: bool, budget: Duration) -> AgentSession {
        let work = WorkGate::default();
        let jobs = JobRegistry::new(work.clone(), JobLimits::default());
        let mut bash_options = self.bash_options(Some(&jobs), Some(&work));
        bash_options.foreground_budget = budget;
        let bash = create_bash_tool(self.ws(), bash_options);
        let mut options = options(&self.h, store, &self.ws().to_string_lossy());
        options.tools = vec![Arc::new(bash)];
        options.active_tool_names = if codemode {
            vec!["bash".into(), "codemode".into()]
        } else {
            vec!["bash".into()]
        };
        options.work = Some(work);
        options.jobs = Some(jobs);
        AgentSession::create(options).await.expect("session")
    }

    /// The model calls `tool` with `arguments` as `id`, then answers
    /// "done".
    fn call(&self, tool: &str, arguments: Value, id: &str) {
        self.h.faux.append_responses(vec![
            faux_assistant_blocks(
                vec![faux_tool_call(tool, arguments, Some(id))],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("done").into(),
        ]);
    }
}

async fn run(
    tool: &AgentTool,
    params: Value,
    signal: Option<bake_ai::AbortSignal>,
) -> Result<AgentToolResult, String> {
    (tool.execute)(
        "call".into(),
        params,
        signal,
        AgentToolUpdateCallback::ignore(),
        ToolContext::detached(),
    )
    .await
}

fn text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect()
}

fn tool_result(session: &AgentSession, id: &str) -> bake_ai::ToolResultMessage {
    session
        .messages()
        .iter()
        .find_map(|message| {
            message
                .as_tool_result()
                .filter(|result| result.tool_call_id == id)
                .cloned()
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

/// The file's entries as `kind` strings: `job:<phase>:<id>`,
/// `result:<call id>`, and `notice:<job id>`.
fn file_log(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter_map(|entry| {
            let json = entry.as_json();
            match entry.entry_type() {
                "custom" if json["customType"] == "bake.job.state" => Some(format!(
                    "job:{}:{}",
                    json["data"]["phase"].as_str()?,
                    json["data"]["jobId"].as_str()?
                )),
                "custom_message" => Some(format!("notice:{}", json["details"]["jobId"].as_str()?)),
                "message" if json["message"]["role"] == "toolResult" => Some(format!(
                    "result:{}",
                    json["message"]["toolCallId"].as_str()?
                )),
                _ => None,
            }
        })
        .collect()
}

async fn terminal(jobs: &JobRegistry, id: crate::jobs::JobId) -> crate::jobs::JobSnapshot {
    tokio::time::timeout(WAIT, async {
        loop {
            let job = jobs.get(id).expect("kept");
            if job.status.is_terminal() {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the job settles")
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Reads a pid the command wrote.
async fn pid(path: &std::path::Path) -> i32 {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the command wrote its pid")
}

const PROMOTED: &str = "Command still running after 2 seconds; it continues as background job bash-1: `job_output` reads more of its output, `job_kill` stops it, and a notice arrives when it finishes.";

// A command that finishes inside the budget gives exactly the result a
// tool without jobs gives, and starts no job.
#[tokio::test]
async fn a_command_that_finishes_in_time_is_unchanged() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-in-time");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let mut options = fx.bash_options(Some(&jobs), None);
    options.foreground_budget = Duration::from_secs(60);
    let budgeted = create_bash_tool(fx.ws(), options);
    let plain = create_bash_tool(fx.ws(), fx.bash_options(None, None));
    for command in ["printf 'a\\nb\\n'", "printf oops; exit 3", "true"] {
        let normalize = |result: Result<AgentToolResult, String>| {
            let mut result = result.expect("ran");
            if let Some(Value::Object(structured)) = &mut result.structured_content {
                structured.insert("wall_time_seconds".into(), json!(0));
            }
            (
                result.content,
                result.details,
                result.structured_content,
                result.is_error,
            )
        };
        let params = json!({ "command": command });
        assert_eq!(
            normalize(run(&budgeted, params.clone(), None).await),
            normalize(run(&plain, params, None).await),
            "{command}"
        );
    }
    assert!(jobs.list().is_empty());
    jobs.close().await;

    // In a session, it leaves nothing about a job in the file.
    let session = fx.session_with(false, Duration::from_secs(60)).await;
    fx.call("bash", json!({ "command": "printf quick" }), "quick");
    session.prompt("go", Vec::new()).await.expect("prompt");
    let quick = tool_result(&session, "quick");
    assert_eq!(text(&quick.content), "quick");
    assert_eq!(quick.details, None);
    assert_eq!(file_log(&session), ["result:quick"]);
    assert!(session.jobs().list().is_empty());
    session.shutdown().await;
}

// At the budget the call returns its output so far and the job's id; the
// job's start is in the file before the result, its output continues
// without the part returned, its notice arrives once, and it reports the
// exit code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_past_the_budget_continues_as_a_job() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-promoted");
    let session = fx.session(false).await;
    let command = "printf 'first\\n'; sleep 4; printf 'second\\n'";
    fx.call("bash", json!({ "command": command }), "slow");
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("the prompt returns at the budget")
        .expect("prompt");

    let slow = tool_result(&session, "slow");
    assert!(!slow.is_error);
    assert_eq!(text(&slow.content), format!("first\n\n\n{PROMOTED}"));
    assert_eq!(
        slow.details,
        Some(json!({ "backgroundJob": { "id": "bash-1", "label": command } }))
    );
    let jobs = session.jobs().clone();
    let id = jobs.id("bash-1").expect("listed");
    assert_eq!(jobs.list().len(), 1);
    assert_eq!(jobs.get(id).expect("kept").label, command);
    assert_eq!(file_log(&session), ["job:started:bash-1", "result:slow"]);

    let job = terminal(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.detail.as_deref(), Some("exit code: 0"));
    fx.h.faux
        .append_responses(vec![faux_assistant_message("noted").into()]);
    assert!(session.deliver_job_notices().await.expect("delivered"));
    assert!(!session.deliver_job_notices().await.expect("delivered"));
    let read = jobs.read(id).expect("read");
    assert_eq!(read.text, "second\n", "the returned output is not repeated");
    let log = file_log(&session);
    assert_eq!(
        log.iter()
            .filter(|entry| entry.starts_with("notice:"))
            .collect::<Vec<_>>(),
        ["notice:bash-1"]
    );
    assert!(log.contains(&"job:terminal:bash-1".to_owned()), "{log:?}");
    session.shutdown().await;
}

// The structured content of a promoted call names the job and has no exit
// code, and the description and output schema say so.
#[tokio::test]
async fn a_promoted_call_returns_the_job_id_and_no_exit_code() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-structured");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let tool = create_bash_tool(fx.ws(), fx.bash_options(Some(&jobs), None));
    assert!(tool.description.contains(
        "Optionally provide a timeout in seconds; without one, a command still running after 2 seconds continues as a background job."
    ));
    let schema = tool.output_schema.clone().expect("schema");
    assert_eq!(
        schema["required"],
        json!(["output", "truncated", "wall_time_seconds"])
    );
    assert_eq!(schema["properties"]["job_id"]["type"], "string");
    let result = run(
        &tool,
        json!({ "command": "printf partial; sleep 30" }),
        None,
    )
    .await
    .expect("promoted");
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["output"], "partial");
    assert_eq!(structured["truncated"], false);
    assert_eq!(structured["job_id"], "bash-1");
    assert!(structured.get("exit_code").is_none(), "{structured}");
    assert_eq!(text(&result.content), format!("partial\n\n{PROMOTED}"));
    let id = jobs.id("bash-1").expect("job");
    assert_eq!(jobs.get(id).expect("kept").status, JobStatus::Running);
    jobs.kill(id, None).expect("killed");
    assert_eq!(terminal(&jobs, id).await.status, JobStatus::Killed);
    jobs.close().await;
}

// An explicit `timeout` still kills the command at that time, and an abort
// during the foreground wait kills it as before; neither starts a job.
#[tokio::test]
async fn a_timeout_or_an_abort_still_ends_the_command() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-timeout");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let tool = create_bash_tool(fx.ws(), fx.bash_options(Some(&jobs), None));
    let timed = run(&tool, json!({ "command": "sleep 5", "timeout": 1 }), None).await;
    assert_eq!(timed, Err("Command timed out after 1 seconds".to_owned()));

    let mut long = fx.bash_options(Some(&jobs), None);
    long.foreground_budget = Duration::from_secs(20);
    let tool = create_bash_tool(fx.ws(), long);
    let controller = AbortController::new();
    let abort = {
        let controller = controller.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            controller.abort();
        })
    };
    let started = std::time::Instant::now();
    let aborted = run(
        &tool,
        json!({ "command": "sleep 10" }),
        Some(controller.signal()),
    )
    .await;
    abort.await.expect("aborted");
    assert_eq!(aborted, Err("Command aborted".to_owned()));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(jobs.list().is_empty());
    jobs.close().await;
}

// A tool without a job registry has no budget: it waits for the command.
#[tokio::test]
async fn a_tool_without_jobs_waits_for_the_command() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-no-jobs");
    let mut options = fx.bash_options(None, None);
    options.foreground_budget = Duration::from_millis(200);
    let tool = create_bash_tool(fx.ws(), options);
    assert!(
        tool.description
            .ends_with("Optionally provide a timeout in seconds.")
    );
    let result = run(&tool, json!({ "command": "sleep 1.2; printf ok" }), None)
        .await
        .expect("ran");
    assert_eq!(text(&result.content), "ok");
    assert_eq!(
        result.structured_content.expect("structured")["exit_code"],
        0
    );
}

// Print mode waits for a promoted job within its job budget, then the
// session's shutdown stops it and its whole process tree.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn print_mode_waits_for_a_promoted_job_then_shutdown_stops_its_tree() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-print");
    let session = fx.session(false).await;
    let command = "echo $$ > shell.pid; sleep 30 & echo $! > child.pid; wait";
    fx.call("bash", json!({ "command": command }), "slow");
    let errors = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr: SharedWriter = Arc::clone(&errors) as SharedWriter;
    let started = std::time::Instant::now();
    let code = tokio::time::timeout(
        WAIT,
        run_print_mode(
            &session,
            PrintModeOptions {
                control: RunControl::default(),
                recovery: crate::print_mode::PrintRunRecovery::Disabled,
                mode: PrintOutput::Text,
                initial_message: Some("go".into()),
                initial_images: Vec::new(),
                messages: Vec::new(),
                job_wait: Duration::from_secs(1),
            },
            Arc::new(Mutex::new(Vec::<u8>::new())),
            stderr,
        ),
    )
    .await
    .expect("print mode returns");
    assert_eq!(code, 0);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "waited for the job"
    );
    let written = String::from_utf8(errors.lock().unwrap().clone()).expect("utf-8");
    assert!(
        written.contains(
            "1 background job(s) or subagent run(s) still running after 1 s were stopped"
        ),
        "{written}"
    );
    let shell = pid(&fx.ws().join("shell.pid")).await;
    let child = pid(&fx.ws().join("child.pid")).await;
    let jobs = session.jobs().clone();
    let id = jobs.id("bash-1").expect("job");
    session.shutdown().await;
    assert_eq!(jobs.get(id).expect("kept").status, JobStatus::Killed);
    assert!(!process_exists(shell), "the shell outlived shutdown");
    assert!(
        !process_exists(child),
        "the command's child outlived shutdown"
    );
}

// Shutdown stops a promoted job and its process tree.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_a_promoted_job_and_its_tree() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-shutdown");
    let session = fx.session(false).await;
    let command = "echo $$ > shell.pid; sleep 30 & echo $! > child.pid; wait";
    fx.call("bash", json!({ "command": command }), "slow");
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt returns")
        .expect("prompt");
    let shell = pid(&fx.ws().join("shell.pid")).await;
    let child = pid(&fx.ws().join("child.pid")).await;
    let jobs = session.jobs().clone();
    let id = jobs.id("bash-1").expect("job");
    assert_eq!(jobs.get(id).expect("kept").status, JobStatus::Running);
    assert!(process_exists(shell) && process_exists(child));
    tokio::time::timeout(WAIT, session.shutdown())
        .await
        .expect("shutdown");
    assert_eq!(jobs.get(id).expect("kept").status, JobStatus::Killed);
    assert!(!process_exists(shell), "the shell outlived shutdown");
    assert!(
        !process_exists(child),
        "the command's child outlived shutdown"
    );
}

// A script's `tools.bash` call has the same budget: the script receives the
// job's id and no exit code, and the job outlives the script.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_call_from_a_script_continues_as_a_job() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-codemode");
    let session = fx.session(true).await;
    fx.call(
        "codemode",
        json!({ "code": "const r = await tools.bash({ command: \"printf a; sleep 4; printf b\" });\nreturn JSON.stringify([r.job_id, r.exit_code === undefined, r.output]);" }),
        "c1",
    );
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt returns")
        .expect("prompt");
    let script = tool_result(&session, "c1");
    assert!(!script.is_error, "{:?}", script.content);
    let output = text(&script.content);
    assert!(output.contains(r#"["bash-1",true,"a"]"#), "{output}");
    let jobs = session.jobs().clone();
    let id = jobs.id("bash-1").expect("job");
    let job = terminal(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(jobs.read(id).expect("read").text, "b");
    session.shutdown().await;
}

// With the registry at its active limit, the handover is refused and the
// call keeps the command: it waits for it and returns its ordinary result,
// exit code included, and no job is started for it.
#[tokio::test]
async fn a_refused_handover_keeps_the_command_in_the_call() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-refused");
    let jobs = JobRegistry::new(
        WorkGate::default(),
        JobLimits {
            max_active: 1,
            ..JobLimits::default()
        },
    );
    let held = jobs
        .start(
            crate::jobs::JobSpec {
                kind: "bash",
                label: "holder".into(),
                output: crate::jobs::OutputMode::Final,
            },
            |context| async move {
                context.signal().cancelled().await;
                crate::jobs::JobOutcome::killed("stopped")
            },
        )
        .expect("the holder starts");
    let tool = create_bash_tool(fx.ws(), fx.bash_options(Some(&jobs), None));
    let started = std::time::Instant::now();
    let result = run(
        &tool,
        json!({ "command": "printf a; sleep 3; printf b" }),
        None,
    )
    .await
    .expect("ran");
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "waited for the command"
    );
    assert_eq!(text(&result.content), "ab");
    assert_eq!(result.details, None);
    let structured = result.structured_content.expect("structured");
    assert_eq!(structured["exit_code"], 0);
    assert!(structured.get("job_id").is_none(), "{structured}");
    assert_eq!(jobs.list().len(), 1, "only the holder");
    jobs.kill(held, None).expect("killed");
    jobs.close().await;
}

// An abort that arrives while the command is being handed over, after the
// budget and before the job holds it, is not lost: the call returns the
// job's id, and the command is stopped, so the job ends as killed. The
// abort is made deterministic by aborting from the job journal's write of
// the job's start, which the handover waits for.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_during_the_handover_stops_the_job() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-handover-abort");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let controller = AbortController::new();
    let recorded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let controller = controller.clone();
        let recorded = Arc::clone(&recorded);
        jobs.attach_recorder(Arc::new(move |_snapshot| {
            if recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                controller.abort();
            }
            Box::pin(async { Ok(()) })
        }))
        .expect("recorder");
    }
    let tool = create_bash_tool(fx.ws(), fx.bash_options(Some(&jobs), None));
    let command = "echo $$ > shell.pid; printf a; sleep 30";
    let result = run(
        &tool,
        json!({ "command": command }),
        Some(controller.signal()),
    )
    .await
    .expect("handed over");
    assert_eq!(
        result.structured_content.expect("structured")["job_id"],
        "bash-1"
    );
    let id = jobs.id("bash-1").expect("job");
    let job = terminal(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Killed);
    assert_eq!(job.detail.as_deref(), Some("killed before exit"));
    let shell = pid(&fx.ws().join("shell.pid")).await;
    assert!(!process_exists(shell), "the command outlived the abort");
    jobs.close().await;
}

// Under the `workspace-write` sandbox, a command past the budget is handed
// over the same way, and it runs confined: the home directory is not
// writable to it. Skipped where the host has no working sandbox backend.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandboxed_command_continues_as_a_job() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-sandboxed");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let standing = crate::permissions::StandingPermissions::new(
        bake_sandbox::SandboxMode::WorkspaceWrite,
        crate::permissions::ApprovalPolicy::Never,
        "workspace-write",
        fx.ws(),
        None,
    );
    let permissions = crate::permissions::ToolPermissions::new(
        Arc::new(standing),
        Arc::new(crate::permissions::NeverApprover),
        bake_sandbox::default_provider(),
    );
    let tool = crate::permissions::sandboxed_bash_tool(
        &fx.ws(),
        fx.bash_options(Some(&jobs), None),
        &permissions,
    )
    .expect("bash")
    .tool;
    let probe = run(&tool, json!({ "command": "true" }), None).await;
    if let Err(error) = &probe {
        eprintln!("skipping: no sandbox backend works here ({error})");
        jobs.close().await;
        return;
    }
    let command =
        "if [ -w \"$HOME\" ]; then printf open; else printf confined; fi; sleep 4; printf ' done'";
    let result = run(&tool, json!({ "command": command }), None)
        .await
        .expect("handed over");
    assert_eq!(text(&result.content), format!("confined\n\n{PROMOTED}"));
    let id = jobs.id("bash-1").expect("job");
    let job = terminal(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.detail.as_deref(), Some("exit code: 0"));
    assert_eq!(jobs.read(id).expect("read").text, " done");
    jobs.close().await;
}

/// The text of each job notice in `session`'s transcript.
fn notices(session: &AgentSession) -> Vec<String> {
    session
        .messages()
        .iter()
        .filter_map(|message| match message {
            bake_agent::AgentMessage::Custom(custom) => custom
                .as_any()
                .downcast_ref::<crate::session::messages::CustomMessage>()
                .filter(|custom| custom.custom_type == job_notices::NOTICE_TYPE)
                .map(|custom| match &custom.content {
                    UserContent::Text(text) => text.clone(),
                    UserContent::Blocks(blocks) => text(&blocks[..]),
                }),
            _ => None,
        })
        .collect()
}

// A handed-over job whose process ended before its notice was delivered,
// as in a crash: a session resumed from the file as it was then reports
// the job with its next prompt, once. With only its start recorded the
// outcome is unknown (`lost`); with its end recorded, the outcome is
// given, without its output, which was not kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resumed_session_reports_a_handed_over_job_whose_notice_was_lost() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-crash");
    let session = fx.session(false).await;
    let command = "printf a; sleep 4";
    fx.call("bash", json!({ "command": command }), "slow");
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt returns")
        .expect("prompt");
    let file = session.session_file().expect("file");
    // The file as a crash right after the handover leaves it.
    let at_handover = fx.dir.join("at-handover.jsonl");
    std::fs::copy(&file, &at_handover).expect("copy");
    // And as one after the job ended but before its notice went out.
    let jobs = session.jobs().clone();
    let id = jobs.id("bash-1").expect("job");
    terminal(&jobs, id).await;
    let at_end = fx.dir.join("at-end.jsonl");
    std::fs::copy(&file, &at_end).expect("copy");
    assert!(file_log(&session).contains(&"job:terminal:bash-1".to_owned()));
    session.shutdown().await;

    for (copy, expected) in [
        (
            at_handover,
            format!(
                "background job bash-1 (bash: {command}) belongs to an earlier process and has no recorded final outcome [status: lost]. Its output is unavailable. Check whether its effects completed before starting it again."
            ),
        ),
        (
            at_end,
            format!(
                "background job bash-1 (bash: {command}) finished in its earlier process [status: completed, exit code: 0]. Its output was not retained across processes and is unavailable."
            ),
        ),
    ] {
        let resumed = fx
            .open(
                SessionManager::open(&copy, None, None).expect("open"),
                false,
                BUDGET,
            )
            .await;
        assert!(notices(&resumed).is_empty());
        fx.h.faux.append_responses(vec![
            faux_assistant_message("seen").into(),
            faux_assistant_message("again").into(),
        ]);
        resumed.prompt("next", Vec::new()).await.expect("prompt");
        assert_eq!(notices(&resumed), std::slice::from_ref(&expected));
        // The earlier process's job is not one of this process's.
        assert!(resumed.jobs().list().is_empty());
        resumed.prompt("after", Vec::new()).await.expect("prompt");
        assert_eq!(notices(&resumed), [expected], "reported once");
        resumed.shutdown().await;
    }
}

// A handed-over job is as old as its command: its start is the command's,
// so its age, in the Tasks row and the journal, agrees with the call's
// `wall_time_seconds` rather than starting at the handover.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handed_over_job_counts_from_the_commands_start() {
    if !have_bash() {
        return;
    }
    let fx = Fixture::new("foreground-age");
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let tool = create_bash_tool(fx.ws(), fx.bash_options(Some(&jobs), None));
    let before = bake_ai::now_ms();
    let result = run(&tool, json!({ "command": "printf a; sleep 30" }), None)
        .await
        .expect("handed over");
    let returned = bake_ai::now_ms();
    let wall = (result.structured_content.expect("structured")["wall_time_seconds"]
        .as_f64()
        .expect("wall time")
        * 1000.0) as i64;
    let id = jobs.id("bash-1").expect("job");
    let job = jobs.get(id).expect("kept");
    assert!(wall >= BUDGET.as_millis() as i64, "{wall} ms");
    // Started with the command, not at the handover: as old as the call's
    // wall time when the call returned, to within the clocks' rounding.
    assert!(job.started_at >= before, "{} < {before}", job.started_at);
    assert!(
        (returned - job.started_at - wall).abs() < 100,
        "the job's age {} ms differs from the call's {wall} ms",
        returned - job.started_at
    );
    jobs.kill(id, None).expect("killed");
    terminal(&jobs, id).await;
    jobs.close().await;

    // In a session, the journal records that start.
    let session = fx.session(false).await;
    fx.call("bash", json!({ "command": "sleep 30" }), "slow");
    let before = bake_ai::now_ms();
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt returns")
        .expect("prompt");
    let returned = tool_result(&session, "slow").timestamp;
    let recorded = lock(&session.session)
        .entries()
        .iter()
        .find_map(|entry| {
            let data = entry.get("data")?;
            (entry.get("customType")? == "bake.job.state" && data["phase"] == "started")
                .then(|| data["startedAt"].as_i64())
                .flatten()
        })
        .expect("the start is journaled");
    assert!(recorded >= before, "{recorded} < {before}");
    assert!(
        returned - recorded >= BUDGET.as_millis() as i64,
        "the journaled start is the command's, a budget before the result"
    );
    let jobs = session.jobs().clone();
    assert_eq!(
        jobs.get(jobs.id("bash-1").expect("job"))
            .expect("kept")
            .started_at,
        recorded
    );
    session.shutdown().await;
}
