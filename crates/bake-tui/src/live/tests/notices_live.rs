//! Notices that reach an idle live session from outside a turn: the
//! driver opens a turn only when one may open, and leaves held mail, a
//! peer's message past telecom's wake budget, in the session's registry
//! until its release rather than moving it to the notices kept for the
//! next prompt.

use std::cell::Cell;

use bake_coding_agent::session::messages::CustomMessage;

use super::*;

fn notice(text: &str) -> CustomMessage {
    CustomMessage {
        custom_type: "bake.telecom".into(),
        content: bake_ai::UserContent::Text(text.into()),
        display: true,
        details: None,
        timestamp: 0,
    }
}

fn turns_started(updates: &[RuntimeUpdate]) -> usize {
    updates
        .iter()
        .filter(|update| matches!(update, RuntimeUpdate::TurnStarted { .. }))
        .count()
}

#[test]
fn held_mail_waits_in_the_registry_and_its_release_opens_one_turn_that_carries_it() {
    let (runtime, faux, live) = setup(Vec::new(), Vec::new(), Some("test-key"));
    let jobs = live.session.jobs().clone();
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    let h = Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    };
    h.until(|update| matches!(update, RuntimeUpdate::Status(_)));
    let request = Arc::new(std::sync::Mutex::new(String::new()));
    h.faux.set_responses(vec![FauxResponseStep::from_fn({
        let request = Arc::clone(&request);
        move |context, _, _, _| {
            *request.lock().unwrap() = format!("{:?}", context.messages());
            Ok(faux_assistant_message("Read it."))
        }
    })]);

    jobs.post_held(notice("bravo says the build is green"))
        .expect("posted");
    // The driver takes one request per pass of its loop and looks at the
    // registry at the top of each, so by the second answer it has looked
    // at the held mail at least once.
    h.port().request(Request::CycleThinking);
    h.port().request(Request::CycleThinking);
    let answered = Cell::new(0);
    let before = h.until(|update| {
        if matches!(update, RuntimeUpdate::Status(_)) {
            answered.set(answered.get() + 1);
        }
        answered.get() == 2
    });
    assert_eq!(turns_started(&before), 0, "{before:#?}");
    assert!(jobs.has_held(), "the held mail was claimed");
    assert!(jobs.has_undelivered());
    assert_eq!(h.faux.call_count(), 0);

    assert!(jobs.release_held());
    let turn = h.until_turn_end();
    assert_eq!(turns_started(&turn), 1, "{turn:#?}");
    assert_eq!(
        turn.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Completed))
    );
    assert_eq!(h.faux.call_count(), 1);
    let request = request.lock().unwrap().clone();
    assert!(
        request.contains("bravo says the build is green"),
        "{request}"
    );
    assert!(!jobs.has_undelivered());
    h.stop();
}
