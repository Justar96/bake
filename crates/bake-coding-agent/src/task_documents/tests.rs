use super::*;
use crate::session::{Fault, FaultKind, NewSessionOptions};
use crate::test_support::TempDir;

fn memory() -> SessionManager {
    SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new()).unwrap()
}

fn update(body: &str) -> DocumentUpdate {
    DocumentUpdate {
        id: "goal-1".into(),
        kind: DocumentKind::Goal,
        body: body.into(),
        state: DocumentState::Active,
    }
}

#[test]
fn revisions_reject_stale_tokens_and_follow_the_selected_branch() {
    let mut session = memory();
    let first = revise(&mut session, None, update("fix bug")).unwrap();
    let first_leaf = session.leaf_id().unwrap().to_owned();
    let second = revise(&mut session, Some(&first), update("fix and test")).unwrap();
    let leaf = session.leaf_id().unwrap().to_owned();
    assert!(matches!(
        revise(&mut session, Some(&first), update("stale")),
        Err(DocumentError::Conflict)
    ));
    assert_eq!(session.leaf_id(), Some(leaf.as_str()));
    session.branch(&first_leaf).unwrap();
    let fork = revise(&mut session, Some(&first), update("different approach")).unwrap();
    assert_eq!(fork.revision, second.revision);
    assert_ne!(fork.entry_id, second.entry_id);
    assert_eq!(
        project(&session).unwrap()["goal-1"].snapshot.body,
        "different approach"
    );
    assert!(matches!(
        revise(&mut session, Some(&second), update("other branch")),
        Err(DocumentError::Conflict)
    ));
}

#[test]
fn a_claim_is_not_a_verified_state_and_cannot_change_document_kind() {
    let mut session = memory();
    let first = revise(&mut session, None, update("fix bug")).unwrap();
    let mut claim = update("done");
    claim.state = DocumentState::ClaimedComplete;
    revise(&mut session, Some(&first), claim).unwrap();
    let current = project(&session).unwrap().remove("goal-1").unwrap();
    assert_eq!(current.snapshot.state, DocumentState::ClaimedComplete);
    let mut changed_kind = update("todo");
    changed_kind.kind = DocumentKind::Todo;
    assert!(revise(&mut session, Some(&current.reference()), changed_kind).is_err());
    assert!(serde_json::from_value::<DocumentState>(Value::String("verified".into())).is_err());
}

#[test]
fn a_first_document_is_durable_and_reconstructs_identical_model_context() {
    let dir = TempDir::new("task-document-reopen");
    let mut session = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .unwrap();
    let first = revise(&mut session, None, update("test the exact result")).unwrap();
    let context = session.build_session_context();
    let path = session.session_file().unwrap().to_path_buf();
    drop(session);
    let mut opened = SessionManager::open(&path, None, None).unwrap();
    assert_eq!(project(&opened).unwrap()["goal-1"].reference(), first);
    assert_eq!(opened.build_session_context(), context);
    assert_eq!(expose_pending(&mut opened).unwrap(), 0);
}

#[test]
fn every_failed_group_prefix_reopens_without_repeating_a_snapshot() {
    for line in 0..2 {
        for kind in [FaultKind::Refuse, FaultKind::Tear(17), FaultKind::FailSync] {
            let dir = TempDir::new("task-document-prefix");
            let mut session = SessionManager::create(
                &dir.path().to_string_lossy(),
                Some(&dir.join("sessions")),
                NewSessionOptions::default(),
            )
            .unwrap();
            let path = session.session_file().unwrap().to_path_buf();
            session.arm_group_fault(Fault { line, kind });
            assert!(matches!(
                revise(&mut session, None, update("work")),
                Err(DocumentError::Persistence(_))
            ));
            drop(session);
            let mut opened = SessionManager::open(&path, None, None).unwrap();
            let before = project(&opened).unwrap();
            expose_pending(&mut opened).unwrap();
            assert_eq!(project(&opened).unwrap(), before);
            assert_eq!(expose_pending(&mut opened).unwrap(), 0);
            assert_eq!(
                opened.build_session_context().messages.len(),
                usize::from(!before.is_empty())
            );
        }
    }
}

#[test]
fn malformed_recognized_entries_fail_closed_but_unrelated_entries_are_ignored() {
    let mut session = memory();
    session
        .append_custom_entry("another.extension", Some(Value::Null))
        .unwrap();
    assert!(project(&session).unwrap().is_empty());
    session
        .append_custom_entry(DOCUMENT_TYPE, Some(serde_json::json!({"version": 2})))
        .unwrap();
    assert!(project(&session).is_err());
    assert!(revise(&mut session, None, update("work")).is_err());
}

#[test]
fn snapshot_bounds_and_revision_chains_are_checked_before_append() {
    let mut session = memory();
    for body in [" ".to_owned(), "x".repeat(MAX_BODY_BYTES + 1)] {
        assert!(revise(&mut session, None, update(&body)).is_err());
    }
    assert!(session.entries().is_empty());
    let first = revise(&mut session, None, update("work")).unwrap();
    let mut snapshot = project(&session).unwrap()["goal-1"].snapshot.clone();
    snapshot.revision = 3;
    snapshot.previous_entry = Some(first.entry_id);
    session
        .append_custom_entry(DOCUMENT_TYPE, Some(payload(&snapshot).unwrap()))
        .unwrap();
    assert!(project(&session).is_err());
}

#[test]
fn omitted_changed_and_compacted_exposures_are_repaired_from_the_current_snapshot() {
    let mut session = memory();
    revise(&mut session, None, update("work")).unwrap();
    let exposure = session.leaf_id().unwrap().to_owned();
    session.append_context_edit(&exposure, None).unwrap();
    assert_eq!(expose_pending(&mut session).unwrap(), 1);
    let exposure = session.leaf_id().unwrap().to_owned();
    session
        .append_context_edit(
            &exposure,
            Some(crate::session::EditContent::Text("changed".into())),
        )
        .unwrap();
    assert_eq!(expose_pending(&mut session).unwrap(), 1);
    session
        .append_compaction("summary", None, 100, None, None, None)
        .unwrap();
    assert_eq!(expose_pending(&mut session).unwrap(), 1);
    assert_eq!(expose_pending(&mut session).unwrap(), 0);
    assert_eq!(project(&session).unwrap()["goal-1"].snapshot.revision, 1);
}
