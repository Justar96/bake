//! Snapshot validation is independent of summary timing and provider output.

use bake_ai::providers::faux::{faux_assistant_message, faux_model};
use bake_ai::{UserContent, UserMessage};

use super::*;
use crate::session::{EditContent, NewSessionOptions};

fn settings() -> CompactionSettings {
    CompactionSettings {
        enabled: true,
        reserve_tokens: 1024,
        keep_recent_tokens: 1,
        threshold_percent: None,
    }
}

fn append_turn(session: &mut SessionManager, text: &str) {
    session
        .append_message(UserMessage {
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })
        .unwrap();
    session
        .append_message(faux_assistant_message(&format!("answer: {text}")))
        .unwrap();
}

fn history() -> SessionManager {
    let mut session =
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new()).unwrap();
    append_turn(&mut session, "first request");
    append_turn(&mut session, "second request");
    append_turn(&mut session, "third request");
    session
}

fn snapshot(session: &SessionManager) -> Snapshot {
    Snapshot::capture(
        session,
        faux_model("faux-1"),
        ModelThinkingLevel::Off,
        settings(),
    )
    .unwrap()
}

#[test]
fn snapshot_accepts_descendant_growth_and_keeps_the_original_boundary() {
    let mut session = history();
    let snapshot = snapshot(&session);
    let retained = snapshot.preparation.first_kept_entry_id.clone();
    append_turn(&mut session, "appended while summarizing");
    assert!(snapshot.matches(&session, &snapshot.model, snapshot.thinking, settings()));
    session
        .append_compaction(
            "summary of the old prefix",
            Some(&retained),
            snapshot.preparation.tokens_before,
            None,
            Some(false),
            None,
        )
        .unwrap();
    let messages = session.build_session_projection().messages;
    assert!(messages.iter().any(|message| {
        serde_json::to_string(message.as_json())
            .unwrap()
            .contains("appended while summarizing")
    }));
}

#[test]
fn an_edit_of_summarized_input_rejects_the_candidate() {
    let mut session = history();
    let snapshot = snapshot(&session);
    let target = snapshot
        .branch
        .iter()
        .find(|entry| entry.entry_type() == "message")
        .unwrap()
        .id()
        .to_owned();
    session
        .append_context_edit(&target, Some(EditContent::Text("replacement".into())))
        .unwrap();
    assert!(!snapshot.matches(&session, &snapshot.model, snapshot.thinking, settings()));
}

#[test]
fn another_compaction_rejects_the_candidate() {
    let mut session = history();
    let snapshot = snapshot(&session);
    session
        .append_compaction(
            "newer summary",
            Some(&snapshot.preparation.first_kept_entry_id),
            100,
            None,
            Some(false),
            None,
        )
        .unwrap();
    assert!(!snapshot.matches(&session, &snapshot.model, snapshot.thinking, settings()));
}

#[test]
fn navigating_to_a_sibling_rejects_the_old_branch() {
    let mut session = history();
    let snapshot = snapshot(&session);
    let target = snapshot.branch[1].id().to_owned();
    session.branch(&target).unwrap();
    append_turn(&mut session, "new sibling");
    assert!(!snapshot.matches(&session, &snapshot.model, snapshot.thinking, settings()));
}

#[test]
fn changed_model_thinking_or_compaction_settings_reject_the_candidate() {
    let session = history();
    let snapshot = snapshot(&session);
    let mut changed = snapshot.model.clone();
    changed.id = "another-model".into();
    assert!(!snapshot.matches(&session, &changed, snapshot.thinking, settings()));
    assert!(!snapshot.matches(
        &session,
        &snapshot.model,
        ModelThinkingLevel::Low,
        settings()
    ));
    let changed = CompactionSettings {
        keep_recent_tokens: 200,
        ..settings()
    };
    assert!(!snapshot.matches(&session, &snapshot.model, snapshot.thinking, changed));
}

#[test]
fn summary_call_reserve_includes_both_parts_of_a_split_turn() {
    let session = history();
    let mut snapshot = snapshot(&session);
    snapshot.preparation.is_split_turn = true;
    assert!(!snapshot.preparation.messages_to_summarize.is_empty());
    assert_eq!(snapshot.calls(), 2);
    snapshot.preparation.messages_to_summarize.clear();
    assert_eq!(snapshot.calls(), 1);
}

#[path = "lifecycle_tests.rs"]
mod lifecycle;
