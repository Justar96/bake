//! Context files found while working (`nestedContextFiles`) through a real
//! session over a faux model and the real file tools: off by default; a
//! successful call below the working directory adds the file before the
//! next request, outermost first and once per session; calls outside it,
//! failed calls, and `-nc` add nothing; the override name wins; the bound
//! leaves a file out with a warning for the user; a script's nested call
//! counts; and resume, a fork, `/tree`, and compaction keep the set
//! without announcing it twice.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_tool_call,
};
use bake_ai::transcript::get_current_system_message;
use bake_ai::{AssistantMessage, StopReason, TranscriptContext};
use serde_json::{Value, json};

use super::nested_context::{ENTRY_TYPE, MAX_NESTED_CONTEXT_BYTES};
use super::tests::{Harness, harness, options};
use super::*;
use crate::session::NewSessionOptions;
use crate::test_support::{TempDir, write};
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

const WAIT: Duration = Duration::from_secs(60);

type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

/// A workspace `ws` with `pkg/AGENTS.md`, `pkg/sub/AGENTS.md`, and a file
/// in each, a session directory, and a scripted model: each agent request
/// is recorded and answered from the script, and a summary request (one
/// without Bake's preamble) gets a summary.
struct Fixture {
    dir: TempDir,
    h: Harness,
    requests: Requests,
    script: Arc<Mutex<VecDeque<AssistantMessage>>>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        let ws = dir.join("ws");
        write(&ws.join("pkg/AGENTS.md"), "Package rules.");
        write(&ws.join("pkg/a.txt"), "a");
        write(&ws.join("pkg/sub/AGENTS.md"), "Sub rules.");
        write(&ws.join("pkg/sub/b.txt"), "b");
        let fixture = Self {
            dir,
            h: harness(),
            requests: Arc::default(),
            script: Arc::default(),
        };
        let steps = (0..64)
            .map(|_| {
                let requests = Arc::clone(&fixture.requests);
                let script = Arc::clone(&fixture.script);
                FauxResponseStep::from_fn(move |context, _, _, _| {
                    let system = bake_ai::transcript::get_current_system_prompt(context.messages());
                    if !system.starts_with(crate::system_prompt::PREAMBLE) {
                        return Ok(faux_assistant_message("## Goal\nThe summary."));
                    }
                    requests.lock().unwrap().push(context.clone());
                    Ok(script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| faux_assistant_message("done")))
                })
            })
            .collect();
        fixture.h.faux.set_responses(steps);
        fixture
    }

    fn ws(&self) -> PathBuf {
        self.dir.join("ws")
    }

    fn file(&self, relative: &str) -> String {
        self.ws().join(relative).to_string_lossy().into_owned()
    }

    /// The model calls `tool` with `args` as `id`, then answers "done".
    fn call(&self, tool: &str, args: Value, id: &str) {
        let mut script = self.script.lock().unwrap();
        script.push_back(faux_assistant_blocks(
            vec![faux_tool_call(tool, args, Some(id))],
            StopReason::ToolUse,
        ));
        script.push_back(faux_assistant_message("done"));
    }

    fn store(&self) -> SessionManager {
        SessionManager::create(
            &self.ws().to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("store")
    }

    async fn open(&self, store: SessionManager, settings: Settings) -> AgentSession {
        let mut options = options(&self.h, store, &self.ws().to_string_lossy());
        options.tools = create_all_tool_definitions(&self.ws(), &ToolsOptions::default())
            .into_iter()
            .map(|definition| Arc::new(definition.tool))
            .collect();
        options.active_tool_names = ["read", "ls", "codemode"].map(str::to_owned).to_vec();
        options.settings = settings;
        AgentSession::create(options).await.expect("session")
    }

    fn requests(&self) -> Vec<TranscriptContext> {
        self.requests.lock().unwrap().clone()
    }

    fn last_request(&self) -> TranscriptContext {
        self.requests().last().cloned().expect("a request")
    }
}

fn on() -> Settings {
    settings(json!({ "nestedContextFiles": true }))
}

fn settings(value: Value) -> Settings {
    Settings::from_object(value.as_object().expect("object"))
}

async fn prompt(session: &AgentSession) {
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt settles")
        .expect("prompt");
}

/// The request's `nested_context` section, as it carries it.
fn nested(context: &TranscriptContext) -> Option<String> {
    get_current_system_message(context.messages())
        .and_then(|system| system.sections)
        .and_then(|sections| {
            sections
                .0
                .into_iter()
                .find(|(name, _)| name == "nested_context")
                .and_then(|(_, text)| text)
        })
}

/// How many system messages in `messages` patch `nested_context`.
fn patches(messages: &[LiveMessage]) -> usize {
    messages
        .iter()
        .filter_map(|message| message.as_system())
        .filter(|system| {
            system
                .sections
                .as_ref()
                .is_some_and(|sections| sections.0.iter().any(|(name, _)| name == "nested_context"))
        })
        .count()
}

fn entries(session: &AgentSession) -> Vec<Value> {
    lock(&session.session)
        .branch_entries(None)
        .iter()
        .filter(|entry| {
            entry.entry_type() == "custom"
                && entry.get("customType").and_then(Value::as_str) == Some(ENTRY_TYPE)
        })
        .filter_map(|entry| entry.get("data").cloned())
        .collect()
}

fn section_for(files: &[(&str, &str)]) -> String {
    let blocks: Vec<String> = files
        .iter()
        .map(|(path, content)| {
            format!("<project_instructions path=\"{path}\">\n{content}\n</project_instructions>")
        })
        .collect();
    format!(
        "<nested_context>\nInstructions for subdirectories, found while working; each applies to the files under its directory:\n\n{}\n</nested_context>",
        blocks.join("\n\n")
    )
}

// Off, the default: the same call sends exactly what it sends with the
// setting on and nothing to find, with no section, no patch, and no entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn off_by_default_nothing_changes() {
    let mut seen = Vec::new();
    for enabled in [false, true] {
        let fx = Fixture::new("nested-off");
        let settings = if enabled {
            std::fs::remove_file(fx.ws().join("pkg/AGENTS.md")).expect("remove");
            std::fs::remove_file(fx.ws().join("pkg/sub/AGENTS.md")).expect("remove");
            on()
        } else {
            Settings::default()
        };
        let session = fx.open(fx.store(), settings).await;
        fx.call("read", json!({ "path": "pkg/sub/b.txt" }), "r1");
        prompt(&session).await;
        let requests = fx.requests();
        assert!(requests.iter().all(|request| nested(request).is_none()));
        assert_eq!(patches(&session.messages()), 0);
        assert!(entries(&session).is_empty());
        seen.push(
            requests
                .iter()
                .map(|request| request.messages().len())
                .collect::<Vec<_>>(),
        );
        session.shutdown().await;
    }
    assert_eq!(seen[0], seen[1]);
}

// A read two levels down adds both files, outermost first, as one patch
// right after the tool result and before the next request; reading or
// listing there again adds nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_below_adds_the_files_before_the_next_request() {
    let fx = Fixture::new("nested-read");
    let session = fx.open(fx.store(), on()).await;
    fx.call("read", json!({ "path": "pkg/sub/b.txt" }), "r1");
    prompt(&session).await;
    let requests = fx.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(nested(&requests[0]), None);
    let expected = section_for(&[
        (&fx.file("pkg/AGENTS.md"), "Package rules."),
        (&fx.file("pkg/sub/AGENTS.md"), "Sub rules."),
    ]);
    assert_eq!(nested(&requests[1]).as_deref(), Some(expected.as_str()));
    let messages = session.messages();
    let result = messages
        .iter()
        .position(|message| message.as_tool_result().is_some())
        .expect("result");
    assert!(
        messages[result + 1].as_system().is_some(),
        "the patch follows the result"
    );
    assert_eq!(patches(&messages), 1);
    assert_eq!(
        entries(&session),
        [json!({ "paths": [fx.file("pkg/AGENTS.md"), fx.file("pkg/sub/AGENTS.md")] })]
    );

    fx.call("read", json!({ "path": "pkg/a.txt" }), "r2");
    prompt(&session).await;
    fx.call("ls", json!({ "path": "pkg/sub" }), "r3");
    prompt(&session).await;
    assert_eq!(patches(&session.messages()), 1);
    assert_eq!(entries(&session).len(), 1);
    assert_eq!(
        nested(&fx.last_request()).as_deref(),
        Some(expected.as_str())
    );
    session.shutdown().await;
}

// A path outside the working directory, a failed call, and `-nc` add
// nothing; the override name wins over `AGENTS.md` beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outside_failed_and_no_context_files_add_nothing() {
    let fx = Fixture::new("nested-none");
    write(&fx.dir.join("outside/AGENTS.md"), "Outside.");
    write(&fx.dir.join("outside/c.txt"), "c");
    let session = fx.open(fx.store(), on()).await;
    let outside = fx.dir.join("outside/c.txt").to_string_lossy().into_owned();
    fx.call("read", json!({ "path": outside }), "r1");
    prompt(&session).await;
    fx.call("read", json!({ "path": "pkg/missing.txt" }), "r2");
    prompt(&session).await;
    assert_eq!(patches(&session.messages()), 0);
    assert!(entries(&session).is_empty());
    session.shutdown().await;

    let mut off = on();
    off.no_context_files = true;
    let session = fx.open(fx.store(), off).await;
    fx.call("read", json!({ "path": "pkg/a.txt" }), "r3");
    prompt(&session).await;
    assert_eq!(patches(&session.messages()), 0);
    session.shutdown().await;

    write(&fx.ws().join("pkg/AGENTS.override.md"), "Override.");
    let session = fx.open(fx.store(), on()).await;
    fx.call("read", json!({ "path": "pkg/a.txt" }), "r4");
    prompt(&session).await;
    assert_eq!(
        nested(&fx.last_request()),
        Some(section_for(&[(
            &fx.file("pkg/AGENTS.override.md"),
            "Override."
        )]))
    );
    session.shutdown().await;
}

// The section holds at most 64 KiB of file text: a file that would pass it
// is left out, with a warning for the user and nothing for the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_past_the_bound_is_left_out_with_a_warning() {
    let fx = Fixture::new("nested-bound");
    let big = "x".repeat(MAX_NESTED_CONTEXT_BYTES / 2 + 1);
    write(&fx.ws().join("pkg/AGENTS.md"), &big);
    write(&fx.ws().join("pkg/sub/AGENTS.md"), &big);
    let session = fx.open(fx.store(), on()).await;
    // Each event's loaded paths and warnings.
    type Events = Vec<(Vec<String>, Vec<String>)>;
    let events: Arc<Mutex<Events>> = Arc::default();
    let seen = Arc::clone(&events);
    let _subscription = session.subscribe(move |event| {
        if let SessionEvent::NestedContext { loaded, warnings } = event {
            seen.lock()
                .unwrap()
                .push((loaded.to_vec(), warnings.to_vec()));
        }
    });
    fx.call("read", json!({ "path": "pkg/sub/b.txt" }), "r1");
    prompt(&session).await;
    let sub = fx.file("pkg/sub/AGENTS.md");
    assert_eq!(
        *events.lock().unwrap(),
        [(
            vec![fx.file("pkg/AGENTS.md")],
            vec![format!(
                "Skipped context file {sub}: the context files found while working may hold at most 64 KiB."
            )]
        )]
    );
    let section = nested(&fx.last_request()).expect("section");
    assert!(!section.contains(&sub) && !section.contains("Skipped"));
    session.shutdown().await;
}

// A script's nested `read` counts as the model's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_codemode_read_adds_the_file() {
    let fx = Fixture::new("nested-codemode");
    let session = fx.open(fx.store(), on()).await;
    fx.call(
        "codemode",
        json!({ "code": "return await tools.read({ path: \"pkg/a.txt\" });" }),
        "c1",
    );
    prompt(&session).await;
    assert_eq!(
        nested(&fx.last_request()),
        Some(section_for(&[(
            &fx.file("pkg/AGENTS.md"),
            "Package rules."
        )]))
    );
    session.shutdown().await;
}

// Resume, a fork, and compaction keep the set and send no second patch;
// `/tree` back to before the find leaves it on the other branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_fork_compaction_and_tree_keep_the_set() {
    let fx = Fixture::new("nested-resume");
    let session = fx.open(fx.store(), on()).await;
    prompt(&session).await;
    let before_find = session.leaf_id().expect("leaf");
    fx.call("read", json!({ "path": "pkg/a.txt" }), "r1");
    prompt(&session).await;
    let expected = section_for(&[(&fx.file("pkg/AGENTS.md"), "Package rules.")]);
    let file = session.session_file().expect("file");
    let leaf = session.leaf_id().expect("leaf");

    // A fork at the leaf carries the entries.
    let forked = session
        .fork_session_manager(&leaf, navigation::ForkPosition::At)
        .await
        .expect("fork");
    session.shutdown().await;
    let forked = fx.open(forked.session, on()).await;
    assert_eq!(forked.core.nested.paths(), [fx.file("pkg/AGENTS.md")]);
    let count = forked.messages().len();
    prompt(&forked).await;
    assert_eq!(patches(&forked.messages()[count..]), 0);
    forked.shutdown().await;

    // A resumed session: the same, and the request still carries it.
    let resumed = fx
        .open(SessionManager::open(&file, None, None).expect("open"), on())
        .await;
    let count = resumed.messages().len();
    prompt(&resumed).await;
    assert_eq!(patches(&resumed.messages()[count..]), 0);
    assert_eq!(
        nested(&fx.last_request()).as_deref(),
        Some(expected.as_str())
    );

    // Compaction keeps it in the recorded system message.
    let mut compacting = on();
    compacting.compaction = settings(json!({
        "compaction": { "keepRecentTokens": 1, "cachedSummary": false }
    }))
    .compaction;
    *write_settings(&resumed.settings) = compacting;
    resumed.compact(None).await.expect("compacted");
    let count = resumed.messages().len();
    prompt(&resumed).await;
    assert_eq!(patches(&resumed.messages()[count..]), 0);
    assert_eq!(
        nested(&fx.last_request()).as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(resumed.core.nested.paths(), [fx.file("pkg/AGENTS.md")]);

    // `/tree` back to before the find: the file is not on that branch, and
    // the session finds it again there.
    resumed
        .navigate_tree(&before_find, TreeNavigationOptions::default(), None)
        .await
        .expect("navigate");
    assert!(resumed.core.nested.paths().is_empty());
    fx.call("read", json!({ "path": "pkg/a.txt" }), "r2");
    prompt(&resumed).await;
    assert_eq!(
        nested(&fx.last_request()).as_deref(),
        Some(expected.as_str())
    );
    resumed.shutdown().await;
}

// An invalid value stops the session's creation with its reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_setting_stops_the_session() {
    let fx = Fixture::new("nested-invalid");
    let mut options = options(&fx.h, fx.store(), &fx.ws().to_string_lossy());
    options.settings = settings(json!({ "nestedContextFiles": "yes" }));
    let Err(error) = AgentSession::create(options).await else {
        panic!("created");
    };
    assert_eq!(
        error,
        "Invalid nestedContextFiles setting: expected true or false."
    );
}

// In `-p` text mode, a context file left out at the bound is a warning on
// standard error, and standard output is only the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn print_mode_prints_the_bound_warning_on_stderr() {
    use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode};
    let fx = Fixture::new("nested-print");
    let big = "x".repeat(MAX_NESTED_CONTEXT_BYTES / 2 + 1);
    write(&fx.ws().join("pkg/AGENTS.md"), &big);
    write(&fx.ws().join("pkg/sub/AGENTS.md"), &big);
    let session = fx.open(fx.store(), on()).await;
    fx.call("read", json!({ "path": "pkg/sub/b.txt" }), "r1");
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let errors = Arc::new(Mutex::new(Vec::<u8>::new()));
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
            Arc::clone(&out) as SharedWriter,
            Arc::clone(&errors) as SharedWriter,
        ),
    )
    .await
    .expect("print mode returns");
    assert_eq!(code, 0);
    let sub = fx.file("pkg/sub/AGENTS.md");
    assert_eq!(
        String::from_utf8(errors.lock().unwrap().clone()).expect("utf-8"),
        format!(
            "Warning: Skipped context file {sub}: the context files found while working may hold at most 64 KiB.\n"
        )
    );
    assert_eq!(
        String::from_utf8(out.lock().unwrap().clone()).expect("utf-8"),
        "done\n"
    );
    session.shutdown().await;
}
