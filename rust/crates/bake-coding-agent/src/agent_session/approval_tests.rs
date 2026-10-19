//! Approval history through a real session: the faux model asks for calls
//! that escalate the sandbox, and an answerer fixture and an effect fixture
//! read the session file on disk when they are asked and when the call
//! runs. Each case checks the approval entries
//! ([`crate::permissions::history`]) against what was asked and what ran.
//! The rules are those of Bake's TypeScript `ApprovalService.request`
//! (`packages/interaction/user-approval/src/index.ts`, lines 208-227) and
//! its `tests/approval.spec.ts`: an `asked` entry before anyone is asked, one
//! `decided` entry per request, a fresh id per request, the pair under the
//! `never` policy and with no one to answer, and a failed write that refuses
//! the request. The kill case follows `context_masking/crash_tests.rs`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use bake_agent::AgentToolResult;
use bake_ai::api::transform_messages::transform_messages;
use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_model, faux_tool_call,
};
use bake_ai::{AssistantMessage, StopReason};
use bake_sandbox::{EscalationOutcome, SandboxMode, UnavailableProvider};
use serde_json::{Value, json};

use super::tests::{Harness, harness, options};
use super::*;
use crate::permissions::approval::ApprovalFuture;
use crate::permissions::state::permission_state;
use crate::permissions::{
    APPROVAL_CUSTOM_TYPE, ApprovalEntry, ApprovalPolicy, ApprovalRequest, Approver,
    BlankJustification, KnobState, NoApprover, StandingPermissions, ToolCall, ToolPermissions,
    approval_history, sandbox_file_tools,
};
use crate::session::{
    AgentMessage, Fault, FaultKind, FileEntry, LeafSelector, NewSessionOptions, SessionEntry,
    build_session_context, build_unanswered_tool_calls, parse_session_entries,
};
use crate::test_support::TempDir;
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

/// The mode every escalating call asks for, from a `read-only` session.
const TARGET: SandboxMode = SandboxMode::WorkspaceWrite;

/// The arguments of an escalating effect call.
fn escalate() -> Value {
    json!({ "sandbox_permissions": TARGET.as_str(), "justification": "to write the file" })
}

/// The refusal an escalating call gets for `outcome`, as the tools word it.
fn refusal(outcome: EscalationOutcome) -> String {
    outcome.refusal("operation", TARGET).expect("not a grant")
}

/// What the fixtures saw, in order.
#[derive(Default)]
struct Log {
    /// Each request the answerer was asked, with the approval entries on
    /// disk at that moment.
    asked: Mutex<Vec<(ApprovalRequest, Vec<Value>)>>,
    /// Each call the effect ran, with the approval entries on disk at that
    /// moment.
    ran: Mutex<Vec<(String, Vec<Value>)>>,
}

impl Log {
    fn asked(&self) -> Vec<(ApprovalRequest, Vec<Value>)> {
        self.asked.lock().unwrap().clone()
    }

    fn ran(&self) -> Vec<(String, Vec<Value>)> {
        self.ran.lock().unwrap().clone()
    }

    fn ran_ids(&self) -> Vec<String> {
        self.ran().into_iter().map(|(id, _)| id).collect()
    }
}

/// The answer to the request about each call id.
type Answers = Arc<dyn Fn(&str) -> EscalationOutcome + Send + Sync>;

fn always(outcome: EscalationOutcome) -> Answers {
    Arc::new(move |_| outcome)
}

/// Answers by call id, reading the session file when asked.
struct Answerer {
    file: Option<PathBuf>,
    log: Arc<Log>,
    answers: Answers,
}

impl Approver for Answerer {
    fn request(&self, request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        let outcome = (self.answers)(&request.call_id);
        let on_disk = self
            .file
            .as_deref()
            .map(approvals_on_disk)
            .unwrap_or_default();
        self.log.asked.lock().unwrap().push((request, on_disk));
        Box::pin(std::future::ready(outcome))
    }
}

/// A tool whose effect is a marker file named after the call, run once the
/// engine resolves the call; it reads the session file as it runs.
fn effect_tool(
    permissions: &ToolPermissions,
    file: Option<PathBuf>,
    markers: PathBuf,
    log: Arc<Log>,
) -> AgentTool {
    let permissions = permissions.clone();
    AgentTool::new(
        "effect",
        "effect",
        "Leaves a marker file.",
        json!({
            "type": "object",
            "properties": {
                "sandbox_permissions": { "type": "string" },
                "justification": { "type": "string" },
            },
        }),
        move |id, args, signal, _, _ctx| {
            let permissions = permissions.clone();
            let file = file.clone();
            let markers = markers.clone();
            let log = Arc::clone(&log);
            Box::pin(async move {
                let call = ToolCall {
                    tool_name: "effect",
                    call_id: &id,
                    parent_tool_call_id: None,
                    args: &args,
                    subject: "operation",
                    blank_justification: BlankJustification::Checked,
                };
                permissions.engine.resolve(&call, signal.as_ref()).await?;
                let on_disk = file.as_deref().map(approvals_on_disk).unwrap_or_default();
                log.ran.lock().unwrap().push((id.clone(), on_disk));
                std::fs::write(markers.join(&id), "ran").map_err(|error| error.to_string())?;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!(
                        "ran {id}"
                    )))],
                    ..AgentToolResult::default()
                })
            })
        },
    )
}

/// A session over `store` whose one tool is the effect fixture, standing in
/// `read-only` under `policy` in `root/ws`, with markers in `root/markers`.
/// `answers` answers its approvals (`None`: no one can, [`NoApprover`]),
/// which it records when `record`.
async fn effect_session(
    h: &Harness,
    root: &Path,
    log: &Arc<Log>,
    store: SessionManager,
    policy: ApprovalPolicy,
    answers: Option<Answers>,
    record: bool,
) -> (AgentSession, ToolPermissions) {
    let ws = root.join("ws");
    let file = store
        .is_persisted()
        .then(|| store.session_file().map(Path::to_path_buf))
        .flatten();
    let answerer: Arc<dyn Approver> = match answers {
        Some(answers) => Arc::new(Answerer {
            file: file.clone(),
            log: Arc::clone(log),
            answers,
        }),
        None => Arc::new(NoApprover),
    };
    let standing = StandingPermissions::new(
        SandboxMode::ReadOnly,
        policy,
        "test",
        &ws,
        Some("approval-tests".into()),
    );
    let permissions = ToolPermissions::new(
        Arc::new(standing),
        answerer,
        Arc::new(UnavailableProvider::new(None)),
    );
    let tool = effect_tool(&permissions, file, root.join("markers"), Arc::clone(log));
    let mut options = options(h, store, &ws.to_string_lossy());
    options.active_tool_names = vec![tool.name.clone()];
    options.tools = vec![Arc::new(tool)];
    let session = AgentSession::create(options).await.expect("session");
    if record {
        session
            .record_approvals(permissions.engine.recorder_slot())
            .expect("bind");
    }
    (session, permissions)
}

/// One response asking for an escalating effect call per id.
fn effect_calls(ids: &[&str]) -> FauxResponseStep {
    faux_assistant_blocks(
        ids.iter()
            .map(|id| faux_tool_call("effect", escalate(), Some(id)))
            .collect(),
        StopReason::ToolUse,
    )
    .into()
}

struct Bench {
    dir: TempDir,
    h: Harness,
    log: Arc<Log>,
}

impl Bench {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.join("ws")).expect("ws");
        std::fs::create_dir_all(dir.join("markers")).expect("markers");
        Self {
            dir,
            h: harness(),
            log: Arc::default(),
        }
    }

    fn ws(&self) -> String {
        self.dir.join("ws").to_string_lossy().into_owned()
    }

    /// A new persisted session in the directory `name`.
    fn store(&self, name: &str) -> SessionManager {
        SessionManager::create(
            &self.ws(),
            Some(&self.dir.join(name)),
            NewSessionOptions::default(),
        )
        .expect("store")
    }

    async fn session(
        &self,
        store: SessionManager,
        policy: ApprovalPolicy,
        answers: Option<Answers>,
        record: bool,
    ) -> (AgentSession, ToolPermissions) {
        effect_session(
            &self.h,
            self.dir.path(),
            &self.log,
            store,
            policy,
            answers,
            record,
        )
        .await
    }

    /// The model asks for one escalating effect call per id in one
    /// response, then answers "done".
    fn script(&self, ids: &[&str]) {
        self.h.faux.append_responses(vec![
            effect_calls(ids),
            faux_assistant_message("done").into(),
        ]);
    }

    fn marker(&self, id: &str) -> bool {
        self.dir.join("markers").join(id).exists()
    }
}

/// Every line of the file at `path`, parsed; none when it is missing.
fn file_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a complete JSON line"))
        .collect()
}

fn is_approval(entry: &Value) -> bool {
    entry["type"] == "custom" && entry["customType"] == APPROVAL_CUSTOM_TYPE
}

/// The data of the approval entries among `entries`, in order.
fn approvals(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|entry| is_approval(entry))
        .map(|entry| entry["data"].clone())
        .collect()
}

fn approvals_on_disk(path: &Path) -> Vec<Value> {
    approvals(&file_lines(path))
}

/// The index of the `toolResult` entry for `id`.
fn result_index(entries: &[Value], id: &str) -> Option<usize> {
    entries.iter().position(|entry| {
        entry["type"] == "message"
            && entry["message"]["role"] == "toolResult"
            && entry["message"]["toolCallId"] == id
    })
}

/// The text and error flag of the `toolResult` entry for `id`.
fn result_text(entries: &[Value], id: &str) -> Option<(String, bool)> {
    let message = &entries[result_index(entries, id)?]["message"];
    let text = message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some((text, message["isError"] == true))
}

fn parsed(text: &str) -> Vec<SessionEntry> {
    parse_session_entries(text)
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            _ => None,
        })
        .collect()
}

/// The request the agent builds from the last leaf of a session file's
/// `text`, with the synthetic results `transform_messages` adds.
fn request_from(text: &str) -> Vec<Message> {
    let context = build_session_context(&parsed(text), LeafSelector::Last);
    let live: Vec<_> = context
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    transform_messages(&convert_to_llm(&live), &faux_model("faux-1"), None)
}

/// A session file's `text` without its approval entries, each other entry
/// re-parented past them, as if they had never been written.
fn strip_approvals(text: &str) -> String {
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();
    let removed: HashMap<String, Value> = lines
        .iter()
        .filter(|entry| is_approval(entry))
        .map(|entry| {
            (
                entry["id"].as_str().expect("id").to_owned(),
                entry["parentId"].clone(),
            )
        })
        .collect();
    lines
        .into_iter()
        .filter(|entry| !is_approval(entry))
        .map(|mut entry| {
            while let Some(parent) = entry["parentId"]
                .as_str()
                .and_then(|id| removed.get(id))
                .cloned()
            {
                entry["parentId"] = parent;
            }
            serde_json::to_string(&entry).expect("line")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `value` with every `timestamp` zeroed and every `durationMs` removed:
/// two runs differ only by clock (the recorded run also syncs its entries
/// while the call is timed, and a response's duration is set only when its
/// timestamp did not round below its start). No provider adapter sends
/// either field.
fn without_clock(mut value: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(members) => {
                members.retain(|key, _| key != "durationMs");
                for (key, member) in members.iter_mut() {
                    if key == "timestamp" {
                        *member = Value::from(0);
                    } else {
                        walk(member);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut value);
    value
}

// `asked` is on disk before anyone is asked, and `decided` before the call
// runs: the answerer finds the request alone in the file, and the effect
// finds the request and its `allowed-once` answer. In the file the pair
// sits between the assistant's call and its result, each entry the child of
// the one before, and a reopened file folds to the same record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_request_and_its_answer_are_on_disk_before_the_call_runs() {
    let bench = Bench::new("approval-order");
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    let file = session.session_file().expect("persisted");
    session.shutdown().await;

    let asked = bench.log.asked();
    let [(request, seen_when_asked)] = asked.as_slice() else {
        panic!("one request: {asked:?}");
    };
    assert_eq!(request.call_id, "c1");
    // A model-issued call has no calling tool call.
    assert_eq!(request.parent_tool_call_id, None);
    assert!(request.reason.contains("to write the file"), "{request:?}");
    let id = seen_when_asked
        .first()
        .and_then(|asked| asked["id"].as_str())
        .expect("the request on disk")
        .to_owned();
    let asked_data = json!({
        "version": 1,
        "kind": "asked",
        "id": id,
        "toolName": "effect",
        "callId": "c1",
        "reason": request.reason,
        "sandbox": "workspace-write",
    });
    let decided_data = json!({
        "version": 1,
        "kind": "decided",
        "id": id,
        "outcome": "allowed-once",
    });
    assert_eq!(seen_when_asked, std::slice::from_ref(&asked_data));
    assert_eq!(
        bench.log.ran(),
        [(
            "c1".to_owned(),
            vec![asked_data.clone(), decided_data.clone()]
        )]
    );
    assert!(bench.marker("c1"));

    let entries = file_lines(&file);
    let call = entries
        .iter()
        .position(|entry| {
            entry["message"]["role"] == "assistant" && entry["message"]["content"][0]["id"] == "c1"
        })
        .expect("the call");
    let [asked_at, decided_at] = [&asked_data, &decided_data].map(|data| {
        entries
            .iter()
            .position(|entry| is_approval(entry) && &entry["data"] == data)
            .expect("approval entry")
    });
    let result = result_index(&entries, "c1").expect("result");
    assert_eq!(
        [asked_at, decided_at, result],
        [call + 1, call + 2, call + 3]
    );
    assert_eq!(entries[asked_at]["parentId"], entries[call]["id"]);
    assert_eq!(entries[decided_at]["parentId"], entries[asked_at]["id"]);
    assert_eq!(entries[result]["parentId"], entries[decided_at]["id"]);
    assert_eq!(
        result_text(&entries, "c1"),
        Some(("ran c1".to_owned(), false))
    );

    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    let history = approval_history(&reopened);
    assert!(history.skipped.is_empty(), "{history:?}");
    let [record] = history.records.as_slice() else {
        panic!("one record: {history:?}");
    };
    assert_eq!(record.asked.id, id);
    assert_eq!(record.asked.call_id.as_deref(), Some("c1"));
    assert_eq!(record.asked.sandbox, Some(TARGET));
    assert_eq!(record.outcome, Some(EscalationOutcome::AllowedOnce));
    assert_eq!(
        record.asked_entry_id,
        entries[asked_at]["id"].as_str().unwrap()
    );
    assert_eq!(
        record.decided_entry_id.as_deref(),
        entries[decided_at]["id"].as_str()
    );
}

/// One call `c1` the answerer would allow, in a session whose `kind` entry
/// write fails: refused before any byte, or written whole and not synced
/// when `unsynced`. Checks what every failed write leaves (the run stops,
/// no further request, the call never runs and has no result past the gap,
/// later prompts are refused) and returns the bench, the prompt's error,
/// and the approval entries in the file.
async fn failed_write(
    name: &str,
    kind: &'static str,
    unsynced: bool,
) -> (Bench, String, Vec<Value>) {
    let bench = Bench::new(name);
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    session.core.hooks().approval_admitted = Some(Arc::new(move |entry, manager| {
        if entry.kind() == kind {
            manager.arm_group_fault(Fault {
                line: 0,
                kind: if unsynced {
                    FaultKind::FailSync
                } else {
                    FaultKind::Refuse
                },
            });
        }
    }));
    let error = session
        .prompt("go", Vec::new())
        .await
        .expect_err("the run stops");
    assert_eq!(bench.h.faux.call_count(), 1, "no request after the failure");
    assert!(!bench.marker("c1"), "the call never ran");
    assert!(bench.log.ran().is_empty());
    let file = session.session_file().expect("persisted");
    let entries = file_lines(&file);
    assert_eq!(result_text(&entries, "c1"), None, "no result past the gap");
    assert!(
        session.prompt("again", Vec::new()).await.is_err(),
        "later prompts are refused"
    );
    session.shutdown().await;
    let approvals = approvals(&entries);
    (bench, error, approvals)
}

// A request that cannot be written is refused before anyone is asked, and
// the session stops as it does for a plugin decision it cannot write. A
// line written but not synced stays in the file, as it may after a crash;
// no one was asked and nothing ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_request_refuses_the_call_unasked_and_stops_the_session() {
    for (unsynced, failure) in [
        (false, "injected: line refused"),
        (true, "injected: sync failed"),
    ] {
        let (bench, error, approvals) =
            failed_write("approval-asked-fails", "asked", unsynced).await;
        assert!(error.contains(failure), "{error}");
        assert!(bench.log.asked().is_empty(), "no one is asked");
        let kinds: Vec<&Value> = approvals.iter().map(|data| &data["kind"]).collect();
        assert_eq!(kinds, if unsynced { vec!["asked"] } else { vec![] });
    }
}

// An answer that cannot be written is refused, even `allowed-once`: the
// call never runs and the session stops. Unsynced, the `allowed-once` line
// stays in the file with no result after it, as after a kill between the
// answer and the effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_answer_refuses_the_call_and_stops_the_session() {
    for (unsynced, failure) in [
        (false, "injected: line refused"),
        (true, "injected: sync failed"),
    ] {
        let (bench, error, approvals) =
            failed_write("approval-decided-fails", "decided", unsynced).await;
        assert!(error.contains(failure), "{error}");
        assert_eq!(bench.log.asked().len(), 1);
        let kinds: Vec<&Value> = approvals.iter().map(|data| &data["kind"]).collect();
        assert_eq!(
            kinds,
            if unsynced {
                vec!["asked", "decided"]
            } else {
                vec!["asked"]
            }
        );
        if unsynced {
            assert_eq!(approvals[1]["outcome"], "allowed-once");
        }
    }
}

/// One escalating call `c1` under `policy`, answered by `answers`, in a
/// session that records its approvals when `record`: the call's result,
/// the approval entries in the file, whether it ran, and how many
/// requests the answerer was asked.
async fn one_call(
    name: &str,
    policy: ApprovalPolicy,
    answers: Option<Answers>,
    record: bool,
) -> ((String, bool), Vec<Value>, bool, usize) {
    let bench = Bench::new(name);
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(bench.store("sessions"), policy, answers, record)
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    let entries = file_lines(&session.session_file().expect("persisted"));
    session.shutdown().await;
    (
        result_text(&entries, "c1").expect("result"),
        approvals(&entries),
        bench.marker("c1"),
        bench.log.asked().len(),
    )
}

// Each answer is recorded as given and changes nothing the model sees: the
// call's result is the one an unrecorded session gives, and only
// `allowed-once` runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_answer_is_recorded_and_answered_as_without_a_record() {
    for outcome in [
        EscalationOutcome::AllowedOnce,
        EscalationOutcome::Rejected,
        EscalationOutcome::Cancelled,
        EscalationOutcome::Unavailable,
    ] {
        let answered = || Some(always(outcome));
        let (result, entries, ran, asked) =
            one_call("approval-answer", ApprovalPolicy::Ask, answered(), true).await;
        let (plain, plain_entries, plain_ran, plain_asked) = one_call(
            "approval-answer-plain",
            ApprovalPolicy::Ask,
            answered(),
            false,
        )
        .await;
        let expected = match outcome {
            EscalationOutcome::AllowedOnce => ("ran c1".to_owned(), false),
            denied => (refusal(denied), true),
        };
        assert_eq!(result, expected, "{outcome:?}");
        assert_eq!(plain, expected, "{outcome:?}, unrecorded");
        assert_eq!(ran, outcome == EscalationOutcome::AllowedOnce);
        assert_eq!(plain_ran, ran);
        assert_eq!((asked, plain_asked), (1, 1));
        assert!(plain_entries.is_empty());
        let [asked_data, decided_data] = entries.as_slice() else {
            panic!("a pair: {entries:?}");
        };
        assert_eq!(asked_data["kind"], "asked");
        assert_eq!(decided_data["kind"], "decided");
        assert_eq!(decided_data["id"], asked_data["id"]);
        assert_eq!(decided_data["outcome"], outcome.as_str());
    }
}

// Under `never` the request is rejected without asking anyone, and with no
// one to answer it is `unavailable`; both still write the pair, as the
// TypeScript service did, and give the unrecorded session's refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_and_no_answerer_still_record_the_pair() {
    for (policy, answers, outcome) in [
        (
            ApprovalPolicy::Never,
            Some(always(EscalationOutcome::AllowedOnce)),
            EscalationOutcome::Rejected,
        ),
        (ApprovalPolicy::Ask, None, EscalationOutcome::Unavailable),
    ] {
        let (result, entries, ran, asked) =
            one_call("approval-closed", policy, answers.clone(), true).await;
        let (plain, _, _, _) = one_call("approval-closed-plain", policy, answers, false).await;
        assert_eq!(result, (refusal(outcome), true), "{policy:?}");
        assert_eq!(plain, result);
        assert!(!ran);
        assert_eq!(asked, 0, "no answerer is consulted");
        let outcomes: Vec<(&Value, &Value)> = entries
            .iter()
            .map(|data| (&data["kind"], &data["outcome"]))
            .collect();
        assert_eq!(
            outcomes,
            [
                (&json!("asked"), &Value::Null),
                (&json!("decided"), &json!(outcome.as_str())),
            ]
        );
    }
}

// `allowed-once` is for its call alone: the next call asks again under a
// fresh request id, and each call runs after its own answer is on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_once_covers_only_its_call() {
    let bench = Bench::new("approval-once");
    bench.h.faux.set_responses(vec![
        effect_calls(&["c1"]),
        effect_calls(&["c2"]),
        faux_assistant_message("done").into(),
    ]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    let history = approval_history(&lock(&session.session));
    session.shutdown().await;
    let asked: Vec<String> = bench
        .log
        .asked()
        .into_iter()
        .map(|(request, _)| request.call_id)
        .collect();
    assert_eq!(asked, ["c1", "c2"]);
    let records: Vec<(Option<&str>, Option<EscalationOutcome>)> = history
        .records
        .iter()
        .map(|record| (record.asked.call_id.as_deref(), record.outcome))
        .collect();
    assert_eq!(
        records,
        [
            (Some("c1"), Some(EscalationOutcome::AllowedOnce)),
            (Some("c2"), Some(EscalationOutcome::AllowedOnce)),
        ]
    );
    assert_ne!(history.records[0].asked.id, history.records[1].asked.id);
    let ran = bench.log.ran();
    assert_eq!(bench.log.ran_ids(), ["c1", "c2"]);
    for ((_, on_disk), record) in ran.iter().zip(&history.records) {
        let last = on_disk.last().expect("entries");
        assert_eq!(last["kind"], "decided");
        assert_eq!(last["id"], record.asked.id.as_str());
    }
}

// A reopened session inherits no answer: the same escalation asks again,
// the new answer decides it, and the recorded `allowed-once` widens no
// standing knob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopened_session_asks_again() {
    let bench = Bench::new("approval-reopen");
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    let file = session.session_file().expect("persisted");
    session.shutdown().await;
    drop(session);

    bench.script(&["c2"]);
    let store = SessionManager::open(&file, None, None).expect("reopen");
    let knobs = permission_state(&store).expect("permission state").knobs;
    assert_eq!(knobs, KnobState::default());
    let (session, _) = bench
        .session(
            store,
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::Rejected)),
            true,
        )
        .await;
    session.prompt("again", Vec::new()).await.expect("prompt");
    {
        let manager = lock(&session.session);
        let history = approval_history(&manager);
        let records: Vec<(Option<&str>, Option<EscalationOutcome>)> = history
            .records
            .iter()
            .map(|record| (record.asked.call_id.as_deref(), record.outcome))
            .collect();
        assert_eq!(
            records,
            [
                (Some("c1"), Some(EscalationOutcome::AllowedOnce)),
                (Some("c2"), Some(EscalationOutcome::Rejected)),
            ]
        );
        assert_eq!(
            permission_state(&manager).expect("permission state").knobs,
            knobs
        );
    }
    session.shutdown().await;
    assert_eq!(
        bench.log.asked().len(),
        2,
        "the reopened session asks again"
    );
    assert_eq!(bench.log.ran_ids(), ["c1"]);
    assert_eq!(
        result_text(&file_lines(&file), "c2"),
        Some((refusal(EscalationOutcome::Rejected), true))
    );
}

// An abort that lands before the request's write is admitted asks no one
// and writes nothing; the call gets the `cancelled` refusal it gets when an
// abort withdraws the question in an unrecorded session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_before_the_request_is_admitted_asks_no_one_and_records_nothing() {
    let bench = Bench::new("approval-abort-asked");
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    let agent = session.core.agent().expect("agent");
    session.core.hooks().before_approval_admission = Some(Arc::new(move |entry| {
        if entry.kind() == "asked" {
            agent.abort();
        }
    }));
    let _ = session.prompt("go", Vec::new()).await;
    let entries = file_lines(&session.session_file().expect("persisted"));
    session.shutdown().await;
    assert!(bench.log.asked().is_empty());
    assert!(!bench.marker("c1"));
    assert!(approvals(&entries).is_empty());
    assert_eq!(
        result_text(&entries, "c1"),
        Some((refusal(EscalationOutcome::Cancelled), true))
    );
}

// An abort that lands after the answer but before its write is admitted
// records `cancelled`, whatever the answer was, and the call does not run:
// the outcome written is the one that takes effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_before_the_answer_is_admitted_records_cancelled() {
    let bench = Bench::new("approval-abort-decided");
    bench.script(&["c1"]);
    let (session, _) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    let agent = session.core.agent().expect("agent");
    session.core.hooks().before_approval_admission = Some(Arc::new(move |entry| {
        if entry.kind() == "decided" {
            agent.abort();
        }
    }));
    let _ = session.prompt("go", Vec::new()).await;
    let entries = file_lines(&session.session_file().expect("persisted"));
    session.shutdown().await;
    assert_eq!(bench.log.asked().len(), 1);
    assert!(!bench.marker("c1"));
    let written = approvals(&entries);
    let [asked, decided] = written.as_slice() else {
        panic!("a pair: {written:?}");
    };
    assert_eq!(asked["kind"], "asked");
    assert_eq!(decided["kind"], "decided");
    assert_eq!(decided["outcome"], "cancelled");
    assert_eq!(
        result_text(&entries, "c1"),
        Some((refusal(EscalationOutcome::Cancelled), true))
    );
}

// Once the session is shut down, an escalating call is refused without
// asking anyone, and nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shut_down_session_refuses_without_asking() {
    let bench = Bench::new("approval-shut-down");
    let (session, permissions) = bench
        .session(
            bench.store("sessions"),
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    session.shutdown().await;
    let args = escalate();
    let call = ToolCall {
        tool_name: "effect",
        call_id: "c1",
        parent_tool_call_id: None,
        args: &args,
        subject: "operation",
        blank_justification: BlankJustification::Checked,
    };
    let refused = permissions.engine.resolve(&call, None).await;
    assert_eq!(refused.expect_err("refused"), "The session is shut down.");
    drop(session);
    let refused = permissions.engine.resolve(&call, None).await;
    assert_eq!(refused.expect_err("refused"), "The session is shut down.");
    assert!(bench.log.asked().is_empty());
    assert!(
        !bench
            .dir
            .join("sessions")
            .read_dir()
            .expect("dir")
            .any(|_| true)
    );
}

// An engine records into one session: a second binding is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_records_into_one_session() {
    let bench = Bench::new("approval-bind-once");
    let (session, permissions) = bench
        .session(bench.store("sessions"), ApprovalPolicy::Ask, None, true)
        .await;
    assert!(permissions.engine.recorder_slot().is_bound());
    assert!(
        session
            .record_approvals(permissions.engine.recorder_slot())
            .is_err()
    );
    session.shutdown().await;
}

// A session that does not persist (`--no-session`) binds no recorder: it
// asks and runs as before, keeps no approval entry, and writes no file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_does_not_persist_records_nothing() {
    let bench = Bench::new("approval-in-memory");
    bench.script(&["c1"]);
    let store =
        SessionManager::in_memory(Some(&bench.ws()), NewSessionOptions::default(), Vec::new())
            .expect("store");
    let (session, permissions) = bench
        .session(
            store,
            ApprovalPolicy::Ask,
            Some(always(EscalationOutcome::AllowedOnce)),
            true,
        )
        .await;
    assert!(!permissions.engine.recorder_slot().is_bound());
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(session.session_file(), None);
    {
        let manager = lock(&session.session);
        assert!(approval_history(&manager).records.is_empty());
        assert!(
            manager
                .entries()
                .iter()
                .all(|entry| entry.get("customType").and_then(Value::as_str)
                    != Some(APPROVAL_CUSTOM_TYPE))
        );
    }
    session.shutdown().await;
    assert_eq!(bench.log.asked().len(), 1);
    assert_eq!(bench.log.ran_ids(), ["c1"]);
    let mut names: Vec<String> = std::fs::read_dir(bench.dir.path())
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, ["markers", "ws"], "no session file");
}

/// A session over Pi's `write` under D24, standing in `read-only` with the
/// `ask` policy in `ws`; the answerer allows `w-allow` and rejects the rest.
async fn write_session(
    h: &Harness,
    ws: &Path,
    store: SessionManager,
    log: &Arc<Log>,
    record: bool,
) -> AgentSession {
    let answers: Answers = Arc::new(|id| {
        if id == "w-allow" {
            EscalationOutcome::AllowedOnce
        } else {
            EscalationOutcome::Rejected
        }
    });
    let standing = StandingPermissions::new(
        SandboxMode::ReadOnly,
        ApprovalPolicy::Ask,
        "test",
        ws,
        Some("approval-tests".into()),
    );
    let permissions = ToolPermissions::new(
        Arc::new(standing),
        Arc::new(Answerer {
            file: store.session_file().map(Path::to_path_buf),
            log: Arc::clone(log),
            answers,
        }),
        Arc::new(UnavailableProvider::new(None)),
    );
    let tools: Vec<Arc<AgentTool>> = sandbox_file_tools(
        create_all_tool_definitions(ws, &ToolsOptions::default()),
        ws,
        &permissions,
    )
    .into_iter()
    .filter(|definition| definition.name() == "write")
    .map(|definition| Arc::new(definition.tool))
    .collect();
    let mut options = options(h, store, &ws.to_string_lossy());
    options.active_tool_names = tools.iter().map(|tool| tool.name.clone()).collect();
    options.tools = tools;
    let session = AgentSession::create(options).await.expect("session");
    if record {
        session
            .record_approvals(permissions.engine.recorder_slot())
            .expect("bind");
    }
    session
}

/// A step that records the request it answers.
fn capture(seen: &Arc<Mutex<Vec<Value>>>, reply: AssistantMessage) -> FauxResponseStep {
    let seen = Arc::clone(seen);
    FauxResponseStep::from_fn(move |context, _, _, _| {
        seen.lock()
            .unwrap()
            .push(serde_json::to_value(context).expect("request"));
        Ok(reply.clone())
    })
}

// The entries reach no request: through Pi's real `write`, an approved and
// a rejected escalation give the provider the same requests, tools and
// results included, whether the session records them or not (up to the
// clock), and the request built from the recorded file is byte-identical
// to the one built from that file with the entries removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_requests_are_the_same_with_and_without_the_entries() {
    let bench = Bench::new("approval-requests");
    let ws = bench.dir.join("ws");
    let write = |path: &str, id: &str| {
        faux_tool_call(
            "write",
            json!({
                "path": path,
                "content": "x",
                "sandbox_permissions": TARGET.as_str(),
                "justification": "to write the file",
            }),
            Some(id),
        )
    };
    let mut runs = Vec::new();
    for (record, sessions) in [(true, "recorded"), (false, "unrecorded")] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        bench.h.faux.set_responses(vec![
            capture(
                &seen,
                faux_assistant_blocks(
                    vec![write("a.txt", "w-allow"), write("b.txt", "w-reject")],
                    StopReason::ToolUse,
                ),
            ),
            capture(&seen, faux_assistant_message("done")),
        ]);
        let store = bench.store(sessions);
        let session = write_session(&bench.h, &ws, store, &bench.log, record).await;
        session.prompt("go", Vec::new()).await.expect("prompt");
        let file = session.session_file().expect("persisted");
        session.shutdown().await;
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).ok().as_deref(),
            Some("x")
        );
        assert!(!ws.join("b.txt").exists());
        std::fs::remove_file(ws.join("a.txt")).expect("reset");
        let text = std::fs::read_to_string(&file).expect("session file");
        runs.push((text, seen.lock().unwrap().clone()));
    }
    let [(recorded_file, recorded), (unrecorded_file, unrecorded)] = runs.as_slice() else {
        unreachable!();
    };
    assert_eq!(bench.log.asked().len(), 4);
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        recorded
            .iter()
            .cloned()
            .map(without_clock)
            .collect::<Vec<_>>(),
        unrecorded
            .iter()
            .cloned()
            .map(without_clock)
            .collect::<Vec<_>>()
    );
    let kinds: Vec<Value> = parsed(recorded_file)
        .iter()
        .filter(|entry| {
            entry.get("customType").and_then(Value::as_str) == Some(APPROVAL_CUSTOM_TYPE)
        })
        .map(|entry| entry.get("data").expect("data")["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        [
            json!("asked"),
            json!("decided"),
            json!("asked"),
            json!("decided")
        ]
    );
    let with = serde_json::to_string(&request_from(recorded_file)).expect("request");
    let without =
        serde_json::to_string(&request_from(&strip_approvals(recorded_file))).expect("request");
    assert_eq!(with, without);
    assert_eq!(
        without_clock(serde_json::from_str(&with).expect("request")),
        without_clock(serde_json::to_value(request_from(unrecorded_file)).expect("request"))
    );
}

const CHILD_ROOT: &str = "BAKE_TEST_APPROVAL_ROOT";
const READY: &str = "bake-approval-writer-held";
const CHILD_TEST: &str = "agent_session::approval_tests::approval_writer_child";

fn hold() {
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    // A vanished parent closes stdin; no helper survives its owner.
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::process::exit(91);
}

/// The writer the kill case stops: one call `c1` the answerer allows, held
/// once its `allowed-once` is written and synced, before the call runs.
#[test]
fn approval_writer_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let h = harness();
            h.faux.set_responses(vec![
                effect_calls(&["c1"]),
                faux_assistant_message("done").into(),
            ]);
            let ws = root.join("ws").to_string_lossy().into_owned();
            let store = SessionManager::open(&root.join("session.jsonl"), None, Some(&ws)).unwrap();
            let (session, _) = effect_session(
                &h,
                &root,
                &Arc::default(),
                store,
                ApprovalPolicy::Ask,
                Some(always(EscalationOutcome::AllowedOnce)),
                true,
            )
            .await;
            session.core.hooks().approval_written = Some(Arc::new(|entry| {
                if matches!(entry, ApprovalEntry::Decided { .. }) {
                    hold();
                }
            }));
            let _ = session.prompt("go", Vec::new()).await;
            panic!("the held child must be killed by its parent");
        });
}

struct Writer {
    child: Child,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    fn start(root: &Path) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
            .env(CHILD_ROOT, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut writer = Self {
            child,
            reader: None,
        };
        let output = writer.child.stdout.take().unwrap();
        let (ready_tx, ready) = mpsc::channel();
        writer.reader = Some(
            std::thread::Builder::new()
                .name("approval-child-output".into())
                .spawn(move || {
                    for line in BufReader::new(output).lines() {
                        let Ok(line) = line else {
                            return;
                        };
                        if line.contains(READY) {
                            let _ = ready_tx.send(());
                        }
                    }
                })
                .unwrap(),
        );
        ready
            .recv_timeout(Duration::from_secs(60))
            .expect("the child wrote its answer");
        writer
    }

    fn kill(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "the child is held before the parent's kill"
        );
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        #[cfg(windows)]
        assert_eq!(status.code(), Some(1));
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// A writer killed after its `allowed-once` is on disk and before the call
// runs reopens with the answer recorded and the call unanswered: the next
// request closes it with Pi's synthetic `No result provided`, and the
// effect never happened. The reopened session grants nothing from it.
#[test]
fn a_writer_killed_after_the_answer_reopens_with_the_call_unanswered() {
    let dir = TempDir::new("approval-crash");
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    std::fs::create_dir_all(dir.join("markers")).unwrap();
    let mut child = Writer::start(dir.path());
    child.kill();
    drop(child);
    assert!(
        !dir.join("markers").join("c1").exists(),
        "the effect never ran"
    );

    let file = dir.join("session.jsonl");
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let history = approval_history(&reopened);
    assert!(history.skipped.is_empty(), "{history:?}");
    let [record] = history.records.as_slice() else {
        panic!("one record: {history:?}");
    };
    assert_eq!(record.asked.call_id.as_deref(), Some("c1"));
    assert_eq!(record.outcome, Some(EscalationOutcome::AllowedOnce));
    assert_eq!(
        permission_state(&reopened).unwrap().knobs,
        KnobState::default()
    );
    drop(reopened);

    let text = std::fs::read_to_string(&file).unwrap();
    let unanswered: Vec<String> = build_unanswered_tool_calls(&parsed(&text), LeafSelector::Last)
        .into_iter()
        .map(|call| call.tool_call_id)
        .collect();
    assert_eq!(unanswered, ["c1"]);
    match request_from(&text).last() {
        Some(Message::ToolResult(result)) => {
            assert_eq!(result.tool_call_id, "c1");
            assert!(result.is_error);
            assert!(
                matches!(result.content.as_slice(),
                    [UserContentBlock::Text(text)] if text.text == "No result provided"),
                "{result:?}"
            );
        }
        other => panic!("the request ends with the synthetic result: {other:?}"),
    }
}
