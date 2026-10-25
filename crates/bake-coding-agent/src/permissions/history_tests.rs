//! The approval entry's data and the history fold, derived from Bake's
//! TypeScript `packages/interaction/user-approval/tests/{approval,invariant}.spec.ts`;
//! each test names the case it follows. The write path is tested through a
//! real session in `crate::agent_session::approval_tests`.

use std::io::Write as _;

use bake_sandbox::{EscalationOutcome, SandboxMode};
use serde_json::{Value, json};

use super::*;
use crate::session::{AgentMessage, NewSessionOptions, SessionManager};
use crate::test_support::TempDir;

fn asked(id: &str, call_id: &str) -> ApprovalEntry {
    ApprovalEntry::Asked(ApprovalAsked {
        id: id.to_owned(),
        tool_name: "bash".to_owned(),
        call_id: Some(call_id.to_owned()),
        reason: Some("escalate sandbox to danger-full-access: needs the network".to_owned()),
        sandbox: Some(SandboxMode::DangerFullAccess),
    })
}

fn decided(id: &str, outcome: EscalationOutcome) -> ApprovalEntry {
    ApprovalEntry::Decided {
        id: id.to_owned(),
        outcome,
    }
}

fn session() -> SessionManager {
    SessionManager::in_memory(Some("/history"), NewSessionOptions::default(), Vec::new())
        .expect("session")
}

fn append(session: &mut SessionManager, data: Option<Value>) -> String {
    session
        .append_custom_entry(APPROVAL_CUSTOM_TYPE, data)
        .expect("append")
}

fn record(session: &mut SessionManager, entry: &ApprovalEntry) -> String {
    append(session, Some(entry.to_data()))
}

// approval.spec: "fails closed to unavailable when nobody listens, auditing
// the asked/decided pair" and "borrows the exact readonly request for
// scoped dispatch and audit": every field of both events.
#[test]
fn every_field_round_trips_under_its_typescript_name() {
    let entry = asked("req-1", "call-1");
    let data = entry.to_data();
    assert_eq!(
        serde_json::to_string(&data).expect("json"),
        r#"{"version":1,"kind":"asked","id":"req-1","toolName":"bash","callId":"call-1","reason":"escalate sandbox to danger-full-access: needs the network","sandbox":"danger-full-access"}"#
    );
    assert_eq!(ApprovalEntry::from_data(Some(&data)), Ok(entry));

    for outcome in OUTCOMES {
        let entry = decided("req-1", outcome);
        let data = entry.to_data();
        assert_eq!(
            data,
            json!({ "version": 1, "kind": "decided", "id": "req-1", "outcome": outcome.as_str() })
        );
        assert_eq!(ApprovalEntry::from_data(Some(&data)), Ok(entry));
    }
    for mode in SandboxMode::ALL {
        let entry = ApprovalEntry::Asked(ApprovalAsked {
            sandbox: Some(mode),
            ..match asked("req-2", "call-2") {
                ApprovalEntry::Asked(asked) => asked,
                ApprovalEntry::Decided { .. } => unreachable!(),
            }
        });
        assert_eq!(ApprovalEntry::from_data(Some(&entry.to_data())), Ok(entry));
    }
}

// approval.spec: "omits absent optional fields from the asked audit event".
#[test]
fn absent_optional_fields_are_omitted_and_read_back_absent() {
    let entry = ApprovalEntry::Asked(ApprovalAsked {
        id: "req-1".to_owned(),
        tool_name: "echo".to_owned(),
        call_id: None,
        reason: None,
        sandbox: None,
    });
    let data = entry.to_data();
    assert_eq!(
        data,
        json!({ "version": 1, "kind": "asked", "id": "req-1", "toolName": "echo" })
    );
    assert_eq!(ApprovalEntry::from_data(Some(&data)), Ok(entry.clone()));
    // `null` reads as absent, and members this version does not know are
    // ignored.
    let padded = json!({
        "version": 1, "kind": "asked", "id": "req-1", "toolName": "echo",
        "callId": null, "reason": null, "sandbox": null, "agent": "main", "extra": [1, 2],
    });
    assert_eq!(ApprovalEntry::from_data(Some(&padded)), Ok(entry));
}

#[test]
fn an_unknown_or_missing_version_is_an_error() {
    for (version, error) in [
        (json!(2), "has unknown version 2"),
        (json!(0), "has unknown version 0"),
        (json!(1.5), "has unknown version 1.5"),
        (json!(-1), "has unknown version -1"),
        (json!("1"), "has unknown version \"1\""),
        (Value::Null, "has unknown version null"),
    ] {
        let data =
            json!({ "version": version, "kind": "decided", "id": "r", "outcome": "rejected" });
        assert_eq!(ApprovalEntry::from_data(Some(&data)), Err(error.to_owned()));
    }
    let data = json!({ "kind": "decided", "id": "r", "outcome": "rejected" });
    assert_eq!(
        ApprovalEntry::from_data(Some(&data)),
        Err("has no version".to_owned())
    );
}

// invariant.spec: "rejects malformed and unpaired audit events" (the empty
// toolName and the unknown outcome): a damaged entry is an error naming the
// problem, never a panic.
#[test]
fn malformed_data_is_an_error_naming_the_problem() {
    let cases = [
        (None, "has no data object"),
        (Some(json!("asked")), "has no data object"),
        (Some(json!([1])), "has no data object"),
        (Some(Value::Null), "has no data object"),
        (Some(json!({ "version": 1, "id": "r" })), "has no kind"),
        (
            Some(json!({ "version": 1, "kind": "asked", "toolName": "bash" })),
            "has no id",
        ),
        (
            Some(json!({ "version": 1, "kind": "asked", "id": "", "toolName": "bash" })),
            "has an empty id",
        ),
        (
            Some(json!({ "version": 1, "kind": "asked", "id": 7, "toolName": "bash" })),
            "has a non-string id",
        ),
        (
            Some(json!({ "version": 1, "kind": "asked", "id": "r" })),
            "has no toolName",
        ),
        (
            Some(json!({ "version": 1, "kind": "asked", "id": "r", "toolName": "" })),
            "has an empty toolName",
        ),
        (
            Some(
                json!({ "version": 1, "kind": "asked", "id": "r", "toolName": "bash", "callId": 1 }),
            ),
            "has a non-string callId",
        ),
        (
            Some(
                json!({ "version": 1, "kind": "asked", "id": "r", "toolName": "bash", "reason": {} }),
            ),
            "has a non-string reason",
        ),
        (
            Some(
                json!({ "version": 1, "kind": "asked", "id": "r", "toolName": "bash", "sandbox": "root" }),
            ),
            "names unknown sandbox mode \"root\"",
        ),
        (
            Some(
                json!({ "version": 1, "kind": "asked", "id": "r", "toolName": "bash", "sandbox": true }),
            ),
            "has a non-string sandbox",
        ),
        (
            Some(json!({ "version": 1, "kind": "decided", "id": "r" })),
            "has no outcome",
        ),
        (
            Some(json!({ "version": 1, "kind": "decided", "id": "r", "outcome": "maybe" })),
            "names unknown outcome \"maybe\"",
        ),
        (
            Some(json!({ "version": 1, "kind": "decided", "id": "r", "outcome": "Allowed-Once" })),
            "names unknown outcome \"Allowed-Once\"",
        ),
        (
            Some(json!({ "version": 1, "kind": "policy", "id": "r" })),
            "has unknown kind \"policy\"",
        ),
        (
            Some(json!({ "version": 1, "kind": 1, "id": "r" })),
            "has a non-string kind",
        ),
    ];
    for (data, error) in cases {
        assert_eq!(
            ApprovalEntry::from_data(data.as_ref()),
            Err(error.to_owned()),
            "{data:?}"
        );
    }
}

// approval.spec: "settles cancelled immediately on an already-aborted
// signal" and "resolves cancelled when the signal aborts mid-question":
// cancellation is linearized at the write's admission.
#[test]
fn admission_turns_an_aborted_write() {
    let ask = asked("req-1", "call-1");
    assert_eq!(ask.clone().admitted(false), Some(ask.clone()));
    assert_eq!(ask.admitted(true), None);
    for outcome in OUTCOMES {
        let answer = decided("req-1", outcome);
        assert_eq!(answer.clone().admitted(false), Some(answer.clone()));
        assert_eq!(
            answer.admitted(true),
            Some(decided("req-1", EscalationOutcome::Cancelled))
        );
    }
}

// approval.spec: "issues a fresh id per request"; the fold pairs each
// request with its answer by id, in the order asked.
#[test]
fn the_fold_pairs_requests_with_their_answers() {
    let mut session = session();
    let first = record(&mut session, &asked("req-1", "call-1"));
    let second = record(&mut session, &asked("req-2", "call-2"));
    let answer_second = record(
        &mut session,
        &decided("req-2", EscalationOutcome::AllowedOnce),
    );
    let answer_first = record(&mut session, &decided("req-1", EscalationOutcome::Rejected));
    let unanswered = record(&mut session, &asked("req-3", "call-2"));

    let history = approval_history(&session);
    assert_eq!(history.skipped, []);
    let summary: Vec<_> = history
        .records
        .iter()
        .map(|record| {
            (
                record.asked.id.as_str(),
                record.asked_entry_id.as_str(),
                record.outcome,
                record.decided_entry_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "req-1",
                first.as_str(),
                Some(EscalationOutcome::Rejected),
                Some(answer_first.as_str())
            ),
            (
                "req-2",
                second.as_str(),
                Some(EscalationOutcome::AllowedOnce),
                Some(answer_second.as_str())
            ),
            ("req-3", unanswered.as_str(), None, None),
        ]
    );
    let for_call: Vec<_> = history
        .for_call("call-2")
        .map(|record| record.asked.id.as_str())
        .collect();
    assert_eq!(for_call, ["req-2", "req-3"]);
    assert_eq!(history.for_call("call-9").count(), 0);
}

// invariant.spec: "rejects malformed and unpaired audit events" (the
// repeated open id and the answer with no matching request), and a second
// answer to one request. The fold skips each and reports it, and keeps the
// rest.
#[test]
fn the_fold_skips_and_reports_entries_out_of_order() {
    let mut session = session();
    let orphan = record(&mut session, &decided("req-0", EscalationOutcome::Rejected));
    record(&mut session, &asked("req-1", "call-1"));
    let again = record(&mut session, &asked("req-1", "call-9"));
    let answer = record(
        &mut session,
        &decided("req-1", EscalationOutcome::Cancelled),
    );
    let second = record(
        &mut session,
        &decided("req-1", EscalationOutcome::AllowedOnce),
    );

    let history = approval_history(&session);
    assert_eq!(history.records.len(), 1);
    let only = &history.records[0];
    assert_eq!(only.asked.call_id.as_deref(), Some("call-1"));
    assert_eq!(only.outcome, Some(EscalationOutcome::Cancelled));
    assert_eq!(only.decided_entry_id.as_deref(), Some(answer.as_str()));
    assert_eq!(
        history.skipped,
        [
            SkippedApprovalEntry {
                entry_id: orphan,
                reason: "answers request req-0, which was not asked before it".to_owned(),
            },
            SkippedApprovalEntry {
                entry_id: again,
                reason: "asks request req-1 again".to_owned(),
            },
            SkippedApprovalEntry {
                entry_id: second.clone(),
                reason: "answers request req-1 again".to_owned(),
            },
        ]
    );
    assert_eq!(
        history.skipped[2].to_string(),
        format!("the bake.approval session entry {second} answers request req-1 again")
    );
}

// approval.spec "folds to the last event" steps over unrelated events; so
// does this fold, and a damaged approval entry hides nothing else.
#[test]
fn the_fold_steps_over_other_entries_and_damaged_ones() {
    let mut session = session();
    session
        .append_custom_entry(
            crate::permissions::PERMISSION_CUSTOM_TYPE,
            Some(json!({ "sandbox": "danger-full-access" })),
        )
        .expect("permission");
    session
        .append_custom_entry("other.extension", Some(asked("req-x", "call-x").to_data()))
        .expect("other");
    session.append_session_info("named").expect("info");
    let missing = append(&mut session, None);
    let future = append(
        &mut session,
        Some(json!({ "version": 2, "kind": "asked", "id": "req-1", "toolName": "bash" })),
    );
    record(&mut session, &asked("req-1", "call-1"));
    record(
        &mut session,
        &decided("req-1", EscalationOutcome::Unavailable),
    );

    let history = approval_history(&session);
    assert_eq!(history.records.len(), 1);
    assert_eq!(history.records[0].asked.id, "req-1");
    assert_eq!(
        history.records[0].outcome,
        Some(EscalationOutcome::Unavailable)
    );
    let skipped: Vec<_> = history
        .skipped
        .iter()
        .map(|skipped| (skipped.entry_id.as_str(), skipped.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (missing.as_str(), "has no data object"),
            (future.as_str(), "has unknown version 2"),
        ]
    );
    // The permission fold ignores approval entries: no recorded answer
    // changes the session's standing permission.
    let knobs = crate::permissions::state::permission_state(&session)
        .expect("state")
        .knobs;
    assert_eq!(knobs.sandbox, Some(SandboxMode::DangerFullAccess));
}

/// A session file with approval entries, unparseable lines between them,
/// and a torn last line opens and folds without a panic; the readable
/// entries survive. The loader drops a line that is not JSON before the
/// fold sees it, as Pi v1.3.0's `parseSessionEntries` does, so nothing is
/// reported as skipped: the fold reports only approval entries it reads
/// and cannot use.
#[test]
fn a_damaged_session_file_opens_and_folds() {
    let dir = TempDir::new("approval-history-damaged");
    let mut session = SessionManager::create(
        "/history",
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session");
    session
        .append_message(AgentMessage::from_json(
            json!({ "role": "user", "content": "go", "timestamp": 0 })
                .as_object()
                .expect("object")
                .clone(),
        ))
        .expect("user");
    record(&mut session, &asked("req-1", "call-1"));
    record(
        &mut session,
        &decided("req-1", EscalationOutcome::AllowedOnce),
    );
    let path = session.session_file().expect("persisted").to_path_buf();
    drop(session);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open");
    file.write_all(b"not json\n{\"type\":\"custom\",\"customType\":\"bake.approval\",\"data\":{\"version\":1,\"kind\":\"asked\",\"id\":")
        .expect("damage");
    drop(file);

    let reopened = SessionManager::open(&path, None, None).expect("reopen");
    let history = approval_history(&reopened);
    assert_eq!(history.skipped, []);
    assert_eq!(history.records.len(), 1);
    assert_eq!(
        history.records[0].outcome,
        Some(EscalationOutcome::AllowedOnce)
    );
}
