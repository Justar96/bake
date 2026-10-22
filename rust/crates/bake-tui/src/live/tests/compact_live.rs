//! `/compact [instructions]` through the driver, against a real session
//! saved in a directory this test alone owns, with the faux provider.
//!
//! Follows Pi v1.2.0 (`06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (`/compact` parsing, `handleCompactCommand`, the `compaction_end`
//! handler) and its `test/interactive-mode-compaction.test.ts` case
//! "renders retained entries and appends the latest summary cost at the
//! bottom" (the kept history, then the compaction note; the cost notice is
//! not ported). Pi aborts a running response before compacting; bake
//! refuses `/compact` while a turn runs (see the busy case in
//! `sessions_live.rs`). Bake's own cases: Esc cancels the summary request
//! and the draft comes back; prompts and commands typed while it runs are
//! refused with their drafts kept; a failure keeps the session usable; and
//! stopping the port during a compaction writes nothing.

use bake_ai::providers::faux::FauxFuture;

use super::*;

/// Keeps one token; automatic compaction is off, so only `/compact`
/// compacts the 1000-token faux window, as Pi's `/compact` ignores the
/// toggle.
fn keep_one(retry: bool) -> Settings {
    Settings::from_object(
        json!({ "compaction": { "enabled": false, "keepRecentTokens": 1 }, "retry": { "enabled": retry } })
            .as_object()
            .expect("settings"),
    )
}

/// A port over a saved session that already answered `one`, `two`, and
/// `three`; the provider echoes from then on.
fn compact_world(name: &str) -> (World, PathBuf, Harness, Requests) {
    let world = World::new(name);
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests, _) =
        settings_harness(&world, manager, Vec::new(), None, None, keep_one(false));
    for (id, text) in [(1, "one"), (2, "two"), (3, "three")] {
        h.turn(id, text);
    }
    (world, file, h, requests)
}

/// A summary request that waits for its abort, then streams as aborted.
fn held_summary() -> FauxResponseStep {
    FauxResponseStep::Factory(Arc::new(|_, options, _, _| {
        let signal = options.base.signal.clone();
        let wait: FauxFuture = Box::pin(async move {
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(faux_assistant_message("## Goal\nNever written"))
        });
        wait
    }))
}

fn turn_ended(updates: &[RuntimeUpdate]) -> Option<Outcome> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::TurnEnded(outcome) => Some(*outcome),
        _ => None,
    })
}

// Pi `/compact <instructions>`: the instructions reach the summary request,
// one compaction is written, the transcript starts over with the kept
// history and the note below it, the slash text is never a prompt, and the
// next prompt is sent with the summary instead of the summarized history.
#[test]
fn compact_rebuilds_the_transcript_and_the_next_prompt_sees_the_summary() {
    let (_world, file, h, requests) = compact_world("compact-ok");
    let before = h_requests(&requests);
    h.submit_id(
        4,
        Submission::FollowUp("/compact   keep the numbers  ".into()),
    );
    let mut updates = h.ended(4);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Replaced(_))));
    let started = updates
        .iter()
        .position(|u| matches!(u, RuntimeUpdate::TurnStarted { .. }))
        .expect("the bar shows the compaction");
    assert!(
        updates[..started]
            .iter()
            .all(|u| matches!(u, RuntimeUpdate::Status(_))),
        "{updates:#?}"
    );
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, RuntimeUpdate::Phase(phase) if phase == "compacting"))
    );
    assert_eq!(turn_ended(&updates), Some(Outcome::Completed));
    // No dialog opened, so none is closed: closing one would start a
    // keyboard handoff that drops the keys typed meanwhile.
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, RuntimeUpdate::Dialog(_)))
    );
    assert!(refused(&updates).is_none());
    assert!(committed(&updates).is_empty(), "no row before the rebuild");
    let rows = replaced(&updates).expect("rebuilt");
    let note = match rows.last() {
        Some(Row::Info(note)) => note.clone(),
        other => panic!("{other:?}"),
    };
    assert!(
        note.starts_with("Compacted from ") && note.ends_with(" tokens"),
        "{note}"
    );
    assert!(
        rows.contains(&Row::Answer("Echo: three.".into())),
        "{rows:#?}"
    );
    assert!(!rows.contains(&Row::User("one".into())), "{rows:#?}");
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row, Row::Info(text) if text.contains("Earlier conversation")))
    );

    let sent = requests.lock().expect("requests")[before..].to_vec();
    assert_eq!(sent.len(), 2, "the history and the split turn's prefix");
    assert!(
        sent[0].1.contains("Additional focus: keep the numbers"),
        "{}",
        sent[0].1
    );
    assert_eq!(entries(&file, "compaction").len(), 1);

    h.turn(5, "four");
    let next = requests
        .lock()
        .expect("requests")
        .last()
        .cloned()
        .expect("request");
    assert!(
        next.1.contains("compacted into the following summary"),
        "{}",
        next.1
    );
    assert!(!next.1.contains("\"one\""), "{}", next.1);
    h.stop();
    assert_eq!(texts(&file, "user"), ["one", "two", "three", "four"]);
    assert!(
        !requests
            .lock()
            .expect("requests")
            .iter()
            .any(|(_, messages)| messages.contains("/compact"))
    );
}

// Pi's `abortCompaction` on Esc: the summary request is aborted, nothing
// is written, and the draft comes back. Prompts and commands typed while it
// runs are refused with their drafts kept, and the session goes on with its
// whole history.
#[test]
fn esc_cancels_a_running_compaction_and_keeps_every_draft() {
    let (_world, file, h, requests) = compact_world("compact-esc");
    let calls = h.faux.call_count();
    h.faux.set_responses(vec![held_summary()]);
    h.submit_id(4, Submission::FollowUp("/compact".into()));
    h.until(|u| matches!(u, RuntimeUpdate::Phase(phase) if phase == "compacting"));
    h.wait_for_requests(calls + 1);
    h.submit_id(5, Submission::Steer("typed meanwhile".into()));
    let reason = refused(&h.ended(5)).expect("refused");
    assert!(reason.contains("being compacted"), "{reason}");
    h.submit_id(6, Submission::Steer("/model".into()));
    let reason = refused(&h.ended(6)).expect("refused");
    assert!(reason.contains("Another command"), "{reason}");
    h.port().request(Request::Cancel);
    let updates = h.ended(4);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert_eq!(turn_ended(&updates), Some(Outcome::Interrupted));
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, RuntimeUpdate::Dialog(_)))
    );
    assert!(replaced(&updates).is_none());
    assert!(entries(&file, "compaction").is_empty());

    echo(&h.faux, &requests);
    h.turn(7, "four");
    let next = requests
        .lock()
        .expect("requests")
        .last()
        .cloned()
        .expect("request");
    assert!(
        next.1.contains("\"one\"") && !next.1.contains("summary"),
        "{}",
        next.1
    );
    h.stop();
    assert_eq!(texts(&file, "user"), ["one", "two", "three", "four"]);
}

// Pi `compaction_end` with an error: the failure is reported, nothing is
// written, the draft comes back, and the session takes the next prompt.
// An empty session has nothing to compact, which is reported the same way.
#[test]
fn a_failed_compaction_keeps_the_draft_and_the_session() {
    let (_world, file, h, requests) = compact_world("compact-fail");
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(|_, _, _, _| {
            Err("400 bad request".into())
        })]);
    let updates = h.command(4, "/compact");
    let reason = refused(&updates).expect("refused");
    assert!(
        reason.starts_with("Compaction failed:") && reason.contains("400 bad request"),
        "{reason}"
    );
    assert_eq!(turn_ended(&updates), Some(Outcome::Failed));
    assert!(entries(&file, "compaction").is_empty());
    echo(&h.faux, &requests);
    h.turn(5, "four");
    h.stop();
    assert_eq!(texts(&file, "user"), ["one", "two", "three", "four"]);

    let world = World::new("compact-empty");
    let (h, _requests, _) = settings_harness(
        &world,
        persisted(&world),
        Vec::new(),
        None,
        None,
        keep_one(false),
    );
    let reason = refused(&h.command(1, "/compact")).expect("refused");
    assert_eq!(
        reason,
        "Compaction failed: Nothing to compact (session too small) Your draft is kept."
    );
    assert_eq!(h.faux.call_count(), 0);
    h.stop();
}

// Bake: stopping the port during a compaction cancels it and waits for it;
// the file gets no compaction and no update arrives after the stop.
#[test]
fn stopping_during_a_compaction_writes_nothing() {
    let (_world, file, h, _requests) = compact_world("compact-stop");
    let calls = h.faux.call_count();
    h.faux.set_responses(vec![held_summary()]);
    h.submit_id(4, Submission::FollowUp("/compact".into()));
    h.until(|u| matches!(u, RuntimeUpdate::Phase(phase) if phase == "compacting"));
    h.wait_for_requests(calls + 1);
    h.stop();
    assert!(entries(&file, "compaction").is_empty());
    SessionManager::open(&file, None, None).expect("file free");
}
