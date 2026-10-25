use super::*;
use crate::session::{AgentMessage, Fault, FaultKind, NewSessionOptions};
use crate::task_documents::{DocumentKind, DocumentState, DocumentUpdate, revise};
use crate::test_support::TempDir;
use serde_json::json;

const COMMAND: &str = "cargo test --locked -p example";

struct Fixture {
    session: SessionManager,
    dir: TempDir,
    document: DocumentRef,
    artifacts: ArtifactSet,
}

impl Fixture {
    fn new(persist: bool) -> Self {
        let dir = TempDir::new("verification");
        std::fs::write(dir.join("source.rs"), "old").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "test").unwrap();
        let cwd = dir.path().to_string_lossy();
        let mut session = if persist {
            SessionManager::create(
                &cwd,
                Some(&dir.join("sessions")),
                NewSessionOptions::default(),
            )
            .unwrap()
        } else {
            SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new()).unwrap()
        };
        let document = revise(&mut session, None, update("fix and test")).unwrap();
        let artifacts = capture(&dir);
        Self {
            session,
            dir,
            document,
            artifacts,
        }
    }

    fn prepare(&self) -> VerificationTicket {
        VerificationTicket::prepare(
            &self.session,
            &self.document,
            CheckSpec {
                command: COMMAND.into(),
            },
            &self.artifacts,
        )
        .unwrap()
    }

    fn record(&mut self) -> Evidence {
        let ticket = self.prepare();
        let (_, result) = check(&mut self.session, "check-1");
        record(&mut self.session, ticket, &result, &self.artifacts).unwrap()
    }
}

fn capture(dir: &TempDir) -> ArtifactSet {
    ArtifactSet::capture(dir.path(), &["source.rs".into(), "Cargo.toml".into()]).unwrap()
}

fn update(body: &str) -> DocumentUpdate {
    DocumentUpdate {
        id: "goal".into(),
        kind: DocumentKind::Goal,
        body: body.into(),
        state: DocumentState::Active,
    }
}

fn append(session: &mut SessionManager, value: Value) -> String {
    session
        .append_message(AgentMessage::from_json(value.as_object().unwrap().clone()))
        .unwrap()
}

fn call(id: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "toolCall", "id": id, "name": "bash", "arguments": {"command": COMMAND}}], "timestamp": 1})
}

fn result(id: &str) -> Value {
    json!({"role": "toolResult", "toolCallId": id, "toolName": "bash", "content": [{"type": "text", "text": "passed"}], "isError": false, "timestamp": 2})
}

fn check(session: &mut SessionManager, id: &str) -> (String, String) {
    let assistant = append(session, call(id));
    let result = append(session, result(id));
    (assistant, result)
}

#[test]
fn durable_evidence_reopens_and_tracks_only_current_declared_versions() {
    let mut fixture = Fixture::new(true);
    assert_eq!(
        status(
            &fixture.session,
            &fixture.document,
            Some(&fixture.artifacts)
        )
        .unwrap(),
        EvidenceStatus::Unavailable
    );
    let evidence = fixture.record();
    let path = fixture.session.session_file().unwrap().to_path_buf();
    let Fixture {
        session,
        dir,
        document,
        artifacts,
    } = fixture;
    drop(session);
    let reopened = SessionManager::open(&path, None, None).unwrap();
    assert_eq!(project(&reopened).unwrap()["goal"], evidence);
    assert_eq!(
        status(&reopened, &document, Some(&artifacts)).unwrap(),
        EvidenceStatus::Fresh
    );
    for (name, changed, original) in [("source.rs", "new", "old"), ("Cargo.toml", "fail", "test")] {
        std::fs::write(dir.join(name), changed).unwrap();
        assert_eq!(
            status(
                &reopened,
                &document,
                Some(&artifacts.recapture(dir.path()).unwrap())
            )
            .unwrap(),
            EvidenceStatus::Stale
        );
        std::fs::write(dir.join(name), original).unwrap();
    }
    std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
    assert!(ArtifactSet::capture(dir.path(), &["source.rs".into(), "Cargo.toml".into()]).is_err());
    assert_eq!(
        status(&reopened, &document, None).unwrap(),
        EvidenceStatus::Unavailable
    );
    // Close the owned writer before deleting the directory on Windows.
    drop(reopened);
}

#[test]
fn changed_document_or_inputs_during_a_check_rejects_without_appending() {
    for change_document in [false, true] {
        let mut fixture = Fixture::new(false);
        let ticket = fixture.prepare();
        let (_, result) = check(&mut fixture.session, "check-1");
        if change_document {
            revise(
                &mut fixture.session,
                Some(&fixture.document),
                update("different work"),
            )
            .unwrap();
        } else {
            std::fs::write(fixture.dir.join("source.rs"), "new").unwrap();
        }
        let after = capture(&fixture.dir);
        let count = fixture.session.entries().len();
        assert!(matches!(
            record(&mut fixture.session, ticket, &result, &after),
            Err(DocumentError::Conflict)
        ));
        assert_eq!(fixture.session.entries().len(), count);
    }
}

#[test]
fn native_check_identity_order_and_explicit_success_are_required() {
    for case in [
        "error",
        "missing-status",
        "wrong-tool",
        "wrong-command",
        "duplicate-call",
        "duplicate-result",
        "result-first",
        "before-admission",
        "before-admission-label",
    ] {
        let mut fixture = Fixture::new(false);
        let earlier = case
            .starts_with("before-admission")
            .then(|| check(&mut fixture.session, "check-1"));
        if case == "before-admission-label" {
            fixture
                .session
                .append_label_change(&fixture.document.entry_id, Some("checkpoint"))
                .unwrap();
        }
        let ticket = fixture.prepare();
        let mut assistant = call("check-1");
        let mut output = result("check-1");
        match case {
            "error" => output["isError"] = json!(true),
            "missing-status" => {
                output.as_object_mut().unwrap().remove("isError");
            }
            "wrong-tool" => output["toolName"] = json!("extension"),
            "wrong-command" => assistant["content"][0]["arguments"]["command"] = json!("true"),
            "duplicate-call" => {
                assistant["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(call("check-1")["content"][0].clone());
            }
            _ => {}
        }
        let result_entry = if let Some((_, result)) = earlier {
            result
        } else if case == "result-first" {
            let result = append(&mut fixture.session, output);
            append(&mut fixture.session, assistant);
            result
        } else {
            append(&mut fixture.session, assistant);
            let result_entry = append(&mut fixture.session, output.clone());
            if case == "duplicate-result" {
                append(&mut fixture.session, output);
            }
            result_entry
        };
        let count = fixture.session.entries().len();
        assert!(
            record(
                &mut fixture.session,
                ticket,
                &result_entry,
                &fixture.artifacts
            )
            .is_err(),
            "{case}"
        );
        assert_eq!(fixture.session.entries().len(), count, "{case}");
    }
}

#[test]
fn selected_branch_and_session_identity_bound_the_check() {
    let mut fixture = Fixture::new(false);
    let ticket = fixture.prepare();
    let admission = fixture.session.leaf_id().unwrap().to_owned();
    let (_, result) = check(&mut fixture.session, "check-1");
    fixture.session.branch(&admission).unwrap();
    assert!(record(&mut fixture.session, ticket, &result, &fixture.artifacts).is_err());

    let ticket = fixture.prepare();
    let mut fork = fixture.session.branched_copy(Some(&admission)).unwrap();
    let (_, result) = check(&mut fork, "fork-check");
    assert!(matches!(
        record(&mut fork, ticket, &result, &fixture.artifacts),
        Err(DocumentError::Conflict)
    ));
    assert!(project(&fork).unwrap().is_empty());
}

#[test]
fn historical_evidence_survives_compaction_but_never_verifies_a_new_revision() {
    let mut fixture = Fixture::new(false);
    let admission = fixture.session.leaf_id().unwrap().to_owned();
    let evidence = fixture.record();
    fixture
        .session
        .append_context_edit(&evidence.snapshot.result_entry, None)
        .unwrap();
    fixture
        .session
        .append_compaction("summary", None, 100, None, None, None)
        .unwrap();
    assert_eq!(
        status(
            &fixture.session,
            &fixture.document,
            Some(&fixture.artifacts)
        )
        .unwrap(),
        EvidenceStatus::Fresh
    );
    let fork = fixture
        .session
        .branched_copy(fixture.session.leaf_id())
        .unwrap();
    assert_eq!(
        status(&fork, &fixture.document, Some(&fixture.artifacts)).unwrap(),
        EvidenceStatus::Fresh
    );

    let newer = revise(
        &mut fixture.session,
        Some(&fixture.document),
        update("additional requirement"),
    )
    .unwrap();
    assert_eq!(
        status(&fixture.session, &newer, Some(&fixture.artifacts)).unwrap(),
        EvidenceStatus::Stale
    );
    fixture.session.branch(&admission).unwrap();
    assert_eq!(
        status(
            &fixture.session,
            &fixture.document,
            Some(&fixture.artifacts)
        )
        .unwrap(),
        EvidenceStatus::Unavailable
    );
}

#[test]
fn admission_after_a_label_survives_pi_fork_label_recreation() {
    let mut fixture = Fixture::new(false);
    fixture
        .session
        .append_label_change(&fixture.document.entry_id, Some("checkpoint"))
        .unwrap();
    fixture.record();
    let fork = fixture
        .session
        .branched_copy(fixture.session.leaf_id())
        .unwrap();
    assert_eq!(
        status(&fork, &fixture.document, Some(&fixture.artifacts)).unwrap(),
        EvidenceStatus::Fresh
    );
}

#[test]
fn invalid_or_ambiguous_replayed_evidence_fails_closed() {
    for case in [
        "version",
        "command",
        "result",
        "document",
        "artifacts",
        "duplicate-call",
        "duplicate-result",
    ] {
        let mut fixture = Fixture::new(false);
        fixture
            .session
            .append_custom_entry("unrelated", Some(Value::Null))
            .unwrap();
        let evidence = fixture.record();
        let mut payload = serde_json::to_value(&evidence.snapshot).unwrap();
        match case {
            "version" => payload["version"] = json!(2),
            "command" => payload["check"]["command"] = json!("true"),
            "result" => payload["resultEntry"] = json!(evidence.snapshot.assistant_entry),
            "document" => payload["document"]["revision"] = json!(2),
            "artifacts" => payload["artifacts"] = json!({}),
            "duplicate-call" => {
                append(&mut fixture.session, call("check-1"));
            }
            "duplicate-result" => {
                append(&mut fixture.session, result("check-1"));
            }
            _ => unreachable!(),
        }
        fixture
            .session
            .append_custom_entry(EVIDENCE_TYPE, Some(payload))
            .unwrap();
        assert!(project(&fixture.session).is_err(), "{case}");
        assert!(
            status(
                &fixture.session,
                &fixture.document,
                Some(&fixture.artifacts)
            )
            .is_err(),
            "{case}"
        );
    }
}

#[test]
fn command_and_artifact_bounds_are_validated_at_admission() {
    let fixture = Fixture::new(false);
    for command in [
        " ".into(),
        "true\0false".into(),
        "x".repeat(MAX_COMMAND_BYTES + 1),
    ] {
        assert!(
            VerificationTicket::prepare(
                &fixture.session,
                &fixture.document,
                CheckSpec { command },
                &fixture.artifacts
            )
            .is_err()
        );
    }
    let empty: ArtifactSet = serde_json::from_value(json!({})).unwrap();
    assert!(
        VerificationTicket::prepare(
            &fixture.session,
            &fixture.document,
            CheckSpec {
                command: COMMAND.into()
            },
            &empty
        )
        .is_err()
    );
    assert!(project(&fixture.session).unwrap().is_empty());
}

#[test]
fn failed_evidence_append_reopens_only_complete_valid_provenance() {
    for kind in [FaultKind::Refuse, FaultKind::Tear(17), FaultKind::FailSync] {
        let mut fixture = Fixture::new(true);
        let ticket = fixture.prepare();
        let (_, result) = check(&mut fixture.session, "check-1");
        let path = fixture.session.session_file().unwrap().to_path_buf();
        let expected = if matches!(kind, FaultKind::FailSync) {
            EvidenceStatus::Fresh
        } else {
            EvidenceStatus::Unavailable
        };
        fixture.session.arm_group_fault(Fault { line: 0, kind });
        assert!(matches!(
            record(&mut fixture.session, ticket, &result, &fixture.artifacts),
            Err(DocumentError::Persistence(_))
        ));
        let Fixture {
            session,
            dir: _dir,
            document,
            artifacts,
        } = fixture;
        drop(session);
        let opened = SessionManager::open(&path, None, None).unwrap();
        assert_eq!(
            status(&opened, &document, Some(&artifacts)).unwrap(),
            expected
        );
    }
}
