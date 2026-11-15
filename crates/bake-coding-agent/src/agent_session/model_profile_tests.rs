//! Model profiles through a session ([`crate::settings::ModelProfile`]):
//! the first request of a session whose model matches a profile carries
//! its tools, codemode mode, thinking level, and `model_notes` section; a
//! model switch reaches the model at the next prompt only as the tool
//! change and the section patch the loop already sends, and a switch back
//! undoes them; a resumed session repeats nothing; the compaction members
//! reach the compaction decision; and a session whose model matches no
//! profile sends what it sent before.

use std::time::Duration;

use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use bake_ai::transcript::{get_current_system_message, get_current_tools};
use bake_ai::{Message, TranscriptContext};
use serde_json::{Value, json};

use super::control_tests::{harness, in_memory};
use super::*;
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(30);

fn tool(name: &str) -> Arc<AgentTool> {
    Arc::new(AgentTool::new(
        name,
        name,
        format!("The {name} tool."),
        json!({ "type": "object", "properties": {} }),
        |_, _, _, _, _ctx| Box::pin(async { Ok(bake_agent::AgentToolResult::text("ok")) }),
    ))
}

/// Tools named as the built-in ones the profiles name.
fn tools() -> Vec<Arc<AgentTool>> {
    ["read", "grep", "ls"].map(tool).to_vec()
}

fn settings(value: Value) -> Settings {
    Settings::from_object(value.as_object().expect("object"))
}

/// The profiles of these tests: every `faux` model gets notes and `+grep`,
/// and `faux-1` also `-ls`, `only` codemode, a thinking level, and its own
/// notes, which override the first entry's.
fn profiles() -> Value {
    json!({
        "modelProfiles": {
            "faux/faux-*": { "tools": ["+grep"], "prompt": "Any faux model." },
            "faux-1": {
                "tools": ["-ls"],
                "codemode": { "mode": "only" },
                "thinkingLevel": "low",
                "prompt": "Batch reads through codemode.",
                "compaction": { "reserveTokens": 1234, "thresholdPercent": 55 }
            }
        }
    })
}

type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

/// A step that records the request and answers "done".
fn recording(requests: &Requests) -> FauxResponseStep {
    let requests = Arc::clone(requests);
    FauxResponseStep::from_fn(move |context, _, _, _| {
        requests.lock().unwrap().push(context.clone());
        Ok(faux_assistant_message("done"))
    })
}

fn declared(context: &TranscriptContext) -> Vec<String> {
    get_current_tools(context.messages())
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

/// The request's current sections, by name.
fn sections(context: &TranscriptContext) -> Vec<(String, String)> {
    get_current_system_message(context.messages())
        .and_then(|system| system.sections)
        .map(|sections| {
            sections
                .0
                .into_iter()
                .map(|(name, value)| (name, value.unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

/// One system message's patch: the sections it sets (`true`) or removes
/// (`false`), the tools it adds, and the tools it removes.
type Patch = (Vec<(String, bool)>, Vec<String>, Vec<String>);

/// The system messages a prompt appended, as [`Patch`]es.
fn system_patches(messages: &[LiveMessage]) -> Vec<Patch> {
    messages
        .iter()
        .filter_map(|message| message.as_system())
        .map(|system| {
            (
                system
                    .sections
                    .as_ref()
                    .map(|sections| {
                        sections
                            .0
                            .iter()
                            .map(|(name, value)| (name.clone(), value.is_some()))
                            .collect()
                    })
                    .unwrap_or_default(),
                system
                    .tools_added
                    .iter()
                    .flatten()
                    .map(|tool| tool.name.clone())
                    .collect(),
                system
                    .tools_removed
                    .iter()
                    .flatten()
                    .map(|tool| tool.name.clone())
                    .collect(),
            )
        })
        .collect()
}

async fn prompt(session: &AgentSession) {
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt returns")
        .expect("prompt");
}

// The first request of a session whose model matches: `read` and `codemode`
// declared (`grep` added and `ls` removed, then hidden by `only` codemode),
// the profile's thinking level, and `model_notes` from the later entry,
// right after `environment`.
#[tokio::test]
async fn the_first_request_carries_the_matching_profile() {
    let h = super::tests::harness();
    let dir = TempDir::new("profile-first");
    let requests: Requests = Arc::default();
    h.faux.set_responses(vec![recording(&requests)]);
    let mut options = super::tests::options(
        &h,
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("session"),
        &dir.path().to_string_lossy(),
    );
    options.settings = settings(profiles());
    options.tools = tools();
    options.active_tool_names = ["read", "ls", "codemode"].map(str::to_owned).to_vec();
    let session = AgentSession::create(options).await.expect("session");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    prompt(&session).await;
    let requests = requests.lock().unwrap().clone();
    // `only` leaves the tools scripts call out of the request.
    assert_eq!(declared(&requests[0]), ["codemode"]);
    let names: Vec<String> = sections(&requests[0])
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let environment = names
        .iter()
        .position(|name| name == "environment")
        .expect("environment");
    assert_eq!(names[environment + 1], "model_notes", "{names:?}");
    let notes = sections(&requests[0])
        .into_iter()
        .find(|(name, _)| name == "model_notes")
        .expect("notes")
        .1;
    assert_eq!(
        notes,
        "<model_notes>\nBatch reads through codemode.\n</model_notes>"
    );
    // The scripts' tools: `read` and `grep`, not `ls`.
    let codemode = get_current_tools(requests[0].messages())
        .into_iter()
        .find(|tool| tool.name == "codemode")
        .expect("codemode")
        .description;
    assert!(
        codemode.contains("grep") && !codemode.contains("tools.ls"),
        "{codemode}"
    );
    session.shutdown().await;
}

// A switch to a model with another profile sends, at the next prompt, the
// tool change and the section patch the loop already sends; a switch back
// undoes both; and the profile's thinking level comes with the switch.
#[tokio::test]
async fn a_model_switch_sends_the_tool_change_and_the_section_patch() {
    let h = harness();
    let requests: Requests = Arc::default();
    h.faux.set_responses(vec![
        recording(&requests),
        recording(&requests),
        recording(&requests),
    ]);
    let mut value = profiles();
    // `codemode` is not active here, so `faux-1` keeps its tools declared.
    value["modelProfiles"]["faux-2"] = json!({ "thinkingLevel": "minimal" });
    let session = h.session(in_memory(), settings(value), None, tools()).await;
    // The session started with an explicit level; the profile's applies
    // from a switch, as `modelThinkingLevels` does.
    prompt(&session).await;
    let first = requests.lock().unwrap()[0].clone();
    assert_eq!(declared(&first), ["read", "grep"]);

    let before = session.messages().len();
    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect("switched");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Minimal);
    prompt(&session).await;
    let appended = session.messages()[before..].to_vec();
    assert_eq!(
        system_patches(&appended),
        [(
            vec![("model_notes".to_owned(), true)],
            vec!["ls".to_owned()],
            Vec::new()
        )],
        "faux-2 keeps `+grep`, gets `ls` back, and the first entry's notes"
    );
    let second = requests.lock().unwrap()[1].clone();
    assert_eq!(declared(&second), ["read", "grep", "ls"]);
    let notes = sections(&second)
        .into_iter()
        .find(|(name, _)| name == "model_notes")
        .expect("notes")
        .1;
    assert_eq!(notes, "<model_notes>\nAny faux model.\n</model_notes>");

    let before = session.messages().len();
    session
        .set_model(h.model("faux-1"), false)
        .await
        .expect("switched back");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    prompt(&session).await;
    let appended = session.messages()[before..].to_vec();
    assert_eq!(
        system_patches(&appended),
        [(
            vec![("model_notes".to_owned(), true)],
            Vec::new(),
            vec!["ls".to_owned()]
        )]
    );

    // A model no profile matches removes the notes and the profile's tools.
    let before = session.messages().len();
    session
        .set_model(h.model("plain"), false)
        .await
        .expect("switched");
    h.faux.append_responses(vec![recording(&requests)]);
    prompt(&session).await;
    let appended = session.messages()[before..].to_vec();
    let patches = system_patches(&appended);
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].0, [("model_notes".to_owned(), false)]);
    let last = requests.lock().unwrap().last().cloned().expect("request");
    assert_eq!(declared(&last), ["read", "grep", "ls"]);
    session.shutdown().await;
}

// A resumed session rebuilds the same request: its first prompt after the
// reopen patches nothing.
#[tokio::test]
async fn a_resumed_session_repeats_its_profile_without_a_patch() {
    let h = harness();
    let dir = TempDir::new("profile-resume");
    let store = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .expect("store");
    let path = store.session_file().expect("file").to_owned();
    h.faux.set_responses(vec![
        faux_assistant_message("one").into(),
        faux_assistant_message("two").into(),
    ]);
    let session = h.session(store, settings(profiles()), None, tools()).await;
    prompt(&session).await;
    session.shutdown().await;

    let reopened = SessionManager::open(&path, None, None).expect("reopened");
    let session = h
        .session(reopened, settings(profiles()), None, tools())
        .await;
    let before = session.messages().len();
    prompt(&session).await;
    assert_eq!(system_patches(&session.messages()[before..]), Vec::new());
    session.shutdown().await;
}

// The profile's compaction members win over `compaction.modelOverrides`
// in the settings the session's compaction decides with.
#[tokio::test]
async fn the_compaction_members_reach_the_compaction_decision() {
    let h = harness();
    let mut value = profiles();
    value["compaction"] = json!({
        "keepRecentTokens": 777,
        "modelOverrides": { "faux/faux-1": { "reserveTokens": 999, "thresholdPercent": 90 } }
    });
    let session = h.session(in_memory(), settings(value), None, tools()).await;
    let model = h.model("faux-1");
    let decided = session
        .core
        .compaction_settings(Some(&model))
        .expect("settings");
    assert_eq!(decided.reserve_tokens, 1234);
    assert_eq!(decided.threshold_percent, Some(55));
    assert_eq!(decided.keep_recent_tokens, 777);
    let other = session
        .core
        .compaction_settings(Some(&h.model("plain")))
        .expect("settings");
    assert_eq!(
        other.reserve_tokens,
        crate::compaction::DEFAULT_COMPACTION_SETTINGS.reserve_tokens
    );
    assert_eq!(other.threshold_percent, None);
    session.shutdown().await;
}

// A malformed profile stops the session's creation with its reason.
#[tokio::test]
async fn a_malformed_profile_stops_the_session() {
    let h = super::tests::harness();
    let dir = TempDir::new("profile-malformed");
    let mut options = super::tests::options(
        &h,
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("session"),
        &dir.path().to_string_lossy(),
    );
    options.settings = settings(json!({
        "modelProfiles": { "faux-1": { "thinkingLevel": "huge" } }
    }));
    let Err(error) = AgentSession::create(options).await else {
        panic!("created");
    };
    assert_eq!(
        error,
        "Invalid modelProfiles[\"faux-1\"].thinkingLevel setting: huge. Expected one of off, minimal, low, medium, high, xhigh, or max."
    );
}

// Without a matching profile, nothing is added: no section, no tool, and
// the first request is the one a session without profiles sends.
#[tokio::test]
async fn a_session_no_profile_matches_sends_what_it_sent_before() {
    let mut first = Vec::new();
    for value in [
        json!({}),
        json!({ "modelProfiles": { "other/*": { "prompt": "Not this one.", "tools": ["+ls"] } } }),
    ] {
        let h = harness();
        let requests: Requests = Arc::default();
        h.faux.set_responses(vec![recording(&requests)]);
        let session = h
            .session(
                in_memory(),
                settings(value),
                None,
                ["read", "grep"].map(tool).to_vec(),
            )
            .await;
        prompt(&session).await;
        let request = requests.lock().unwrap()[0].clone();
        session.shutdown().await;
        let messages: Vec<Message> = request.messages().to_vec();
        first.push((declared(&request), sections(&request), messages.len()));
    }
    assert_eq!(first[0], first[1]);
    assert!(first[0].1.iter().all(|(name, _)| name != "model_notes"));
}
