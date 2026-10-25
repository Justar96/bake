//! `/tree`'s branch summary through the driver, against a real session
//! saved in a directory this test alone owns, with the faux provider.
//!
//! Follows Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (`showTreeSelector`: the "Summarize branch?" prompt and its custom
//! instructions editor, Esc going back a step, the
//! `BranchSummaryStatusIndicator` with Esc calling `abortBranchSummary`,
//! and a cancelled summary reopening the tree at the chosen entry) and
//! `core/agent-session.ts` (`navigateTree`). Bake's checks: what the bar
//! shows, the summary request, the replayed branch row, the file, and that
//! the next request starts with the summary.

use bake_ai::providers::faux::FauxFuture;

use super::*;

/// A port over a saved session that answered `alpha` and `beta`.
fn tree_world(name: &str) -> (World, PathBuf, Harness, Requests) {
    let world = World::new(name);
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.turn(2, "beta");
    (world, file, h, requests)
}

/// Opens `/tree` as prompt `id` and chooses the entry labelled `label`;
/// returns the sheet.
fn open_and_choose(h: &Harness, id: u64, label: &str) -> DialogRequest {
    h.submit_id(id, Submission::FollowUp("/tree".into()));
    let sheet = h.next_dialog();
    h.choose_answer(sheet.id, choose(&sheet, label));
    sheet
}

fn title(dialog: &DialogRequest) -> &str {
    match &dialog.kind {
        DialogKind::List(list) => &list.title,
        DialogKind::Field(field) => &field.title,
        DialogKind::Info(info) => &info.title,
    }
}

/// The messages of request `index` as JSON.
fn request(requests: &Requests, index: usize) -> String {
    requests.lock().expect("requests")[index].1.clone()
}

// Pi `showTreeSelector` choosing "Summarize": the summary runs as the
// session's work, shown by the bar; one summary request carries the branch
// left; the transcript starts over with the branch row and the path, the
// message back in the draft; the file holds the summary where the message
// was, and the next request starts with it.
#[test]
fn summarize_writes_the_summary_and_the_next_request_starts_with_it() {
    let (_world, file, h, requests) = tree_world("tree-summarize");
    open_and_choose(&h, 3, "• user: beta");
    let prompt = h.next_dialog();
    assert_eq!(title(&prompt), "Summarize branch?");
    h.choose_answer(prompt.id, choose(&prompt, "Summarize"));
    let mut updates = h.ended(3);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    let started = updates
        .iter()
        .position(|u| matches!(u, RuntimeUpdate::TurnStarted { .. }))
        .expect("the summary ran as work");
    assert_eq!(
        updates.get(started + 1),
        Some(&RuntimeUpdate::summarizing_branch())
    );
    assert!(updates.contains(&RuntimeUpdate::TurnEnded(Outcome::Completed)));
    let rows = replaced(&updates).expect("replaced");
    assert_eq!(
        rows,
        [
            Row::Info("Navigated to selected point".into()),
            Row::User("alpha".into()),
            Row::Answer("Echo: alpha.".into()),
            Row::branch_summary(),
        ]
    );
    assert_eq!(restored(&updates).as_deref(), Some("beta"));
    assert_eq!(h_requests(&requests), 3, "one summary request");
    let summary_request = request(&requests, 2);
    assert!(
        summary_request.contains("<conversation>\\n[Assistant]: Echo: beta.\\n</conversation>"),
        "{summary_request}"
    );
    assert!(!summary_request.contains("Additional focus"));

    h.turn(4, "gamma");
    let sent = request(&requests, 3);
    assert!(
        sent.contains(
            "The following is a summary of a branch that this conversation came back from"
        ),
        "{sent}"
    );
    assert!(sent.contains("alpha") && sent.contains("gamma"), "{sent}");
    h.stop();
    let summaries = entries(&file, "branch_summary");
    assert_eq!(summaries.len(), 1);
    let messages = entries(&file, "message");
    let parent_of = |text: &str| {
        messages
            .iter()
            .find(|entry| {
                let content = &entry["message"]["content"];
                *content == text || content[0]["text"] == text
            })
            .map(|entry| entry["parentId"].clone())
    };
    assert_eq!(Some(summaries[0]["parentId"].clone()), parent_of("beta"));
    assert_eq!(
        parent_of("gamma"),
        Some(summaries[0]["id"].clone()),
        "gamma follows the summary"
    );
}

// Pi: Esc on "Summarize branch?" reopens the tree at the chosen entry; Esc
// in the custom instructions goes back to the choices; entered
// instructions reach the summary request as "Additional focus".
#[test]
fn esc_steps_back_and_custom_instructions_reach_the_request() {
    let (_world, file, h, requests) = tree_world("tree-custom");
    let sheet = open_and_choose(&h, 3, "• user: beta");
    let prompt = h.next_dialog();
    h.choose_answer(prompt.id, DialogAnswer::Cancel);
    let again = h.next_dialog();
    let DialogKind::List(list) = &again.kind else {
        panic!("{again:?}");
    };
    assert!(list.title.starts_with("Session tree"), "{list:?}");
    let chosen = list.selected.expect("selected");
    assert_eq!(list.items[chosen].label, "• user: beta");
    assert_ne!(again.id, sheet.id);
    h.choose_answer(again.id, choose(&again, "• user: beta"));
    let prompt = h.next_dialog();
    h.choose_answer(prompt.id, choose(&prompt, "Summarize with custom prompt"));
    let field = h.next_dialog();
    assert_eq!(title(&field), "Custom summarization instructions");
    h.choose_answer(field.id, DialogAnswer::Cancel);
    let prompt = h.next_dialog();
    assert_eq!(title(&prompt), "Summarize branch?");
    h.choose_answer(prompt.id, choose(&prompt, "Summarize with custom prompt"));
    let field = h.next_dialog();
    h.choose_answer(
        field.id,
        DialogAnswer::Entered("keep the greek letters".to_owned().into()),
    );
    let mut updates = h.ended(3);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Replaced(_))));
    assert!(replaced(&updates).is_some(), "{updates:#?}");
    assert_eq!(h_requests(&requests), 3);
    assert!(
        request(&requests, 2).contains("Additional focus: keep the greek letters"),
        "{}",
        request(&requests, 2)
    );
    h.stop();
    assert_eq!(entries(&file, "branch_summary").len(), 1);
}

/// A summary request that says it started, then waits for its abort and
/// streams as aborted.
fn held_summary(started: std_mpsc::Sender<()>) -> FauxResponseStep {
    let started = Arc::new(StdMutex::new(started));
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let signal = options.base.signal.clone();
        let _ = started.lock().expect("started").send(());
        let wait: FauxFuture = Box::pin(async move {
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(faux_assistant_message("## Goal\nNever written"))
        });
        wait
    }))
}

// Pi: Esc while the branch is summarized aborts the summary
// (`abortBranchSummary`), says so, and reopens the tree at the chosen
// entry; a prompt sent meanwhile is refused with its draft. Nothing is
// written; Esc on the reopened tree gives the draft back.
#[test]
fn esc_during_the_summary_cancels_it_and_reopens_the_tree() {
    let (_world, file, h, requests) = tree_world("tree-cancel");
    let (started_tx, started) = std_mpsc::channel();
    h.faux.set_responses(vec![held_summary(started_tx)]);
    let before = std::fs::read(&file).expect("file");
    open_and_choose(&h, 3, "• user: beta");
    let prompt = h.next_dialog();
    h.choose_answer(prompt.id, choose(&prompt, "Summarize"));
    started.recv_timeout(PATIENCE).expect("the summary started");
    h.submit_id(4, Submission::FollowUp("meanwhile".into()));
    let updates = h.ended(4);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("The session tree is being navigated; your draft is kept.")
    );
    h.port().request(Request::Cancel);
    let updates = h.until(|u| matches!(u, RuntimeUpdate::Dialog(Some(_))));
    assert!(
        updates.contains(&RuntimeUpdate::TurnEnded(Outcome::Interrupted)),
        "{updates:#?}"
    );
    assert!(updates.contains(&RuntimeUpdate::Commit(vec![Row::Info(
        "Branch summarization cancelled".into()
    )])));
    let Some(RuntimeUpdate::Dialog(Some(again))) = updates.last() else {
        unreachable!()
    };
    let DialogKind::List(list) = &again.kind else {
        panic!("{again:?}");
    };
    assert_eq!(
        list.items[list.selected.expect("selected")].label,
        "• user: beta"
    );
    h.choose_answer(again.id, DialogAnswer::Cancel);
    assert_eq!(refused(&h.ended(3)).as_deref(), Some(commands::CANCELLED));
    assert_eq!(std::fs::read(&file).expect("file"), before);
    assert_eq!(h_requests(&requests), 2, "the held summary is not recorded");
    h.stop();
    assert!(entries(&file, "branch_summary").is_empty());
}

// Bake: an Esc that reaches a summarizing navigation before its commit,
// when the branch left holds nothing to summarize, is refused as Pi's
// `Navigation cancelled`; the bar shows the work interrupted, not failed,
// and the leaf stays.
#[test]
fn an_esc_before_a_summarizing_commit_interrupts_the_work() {
    let world = World::new("tree-summary-boundary");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = commands::CommitGate {
        reached: reached_tx,
        // The first hit waits for the test; the second for Esc's abort, so
        // the command cannot pass its commit before Esc reaches the driver.
        release: commands::Release::Each(Arc::new(std::sync::Mutex::new(
            [
                commands::Release::Notified(Arc::clone(&release)),
                commands::Release::Aborted,
            ]
            .into(),
        ))),
    };
    let (h, requests) = session_harness(&world, manager, Vec::new(), Some(gate));
    h.turn(1, "alpha");
    h.turn(2, "beta");
    // Up to alpha's answer, so beta's answer lies below the leaf.
    open_and_choose(&h, 3, "• assistant: Echo: alpha.");
    let prompt = h.next_dialog();
    h.choose_answer(prompt.id, choose(&prompt, "No summary"));
    reached.recv_timeout(PATIENCE).expect("first choice held");
    release.notify_one();
    let mut updates = h.ended(3);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    assert!(replaced(&updates).is_some(), "{updates:#?}");

    h.submit_id(4, Submission::FollowUp("/tree".into()));
    let sheet = h.next_dialog();
    let DialogKind::List(list) = &sheet.kind else {
        panic!("{sheet:?}");
    };
    let below = list
        .items
        .iter()
        .find(|item| item.label.ends_with("assistant: Echo: beta."))
        .unwrap_or_else(|| panic!("{list:#?}"))
        .value
        .clone();
    h.choose_answer(
        sheet.id,
        DialogAnswer::Chose {
            value: below,
            save_default: false,
        },
    );
    let prompt = h.next_dialog();
    h.choose_answer(prompt.id, choose(&prompt, "Summarize"));
    reached.recv_timeout(PATIENCE).expect("second choice held");
    // The answered prompt is the command's dialog: its cancel is Esc.
    h.choose_answer(prompt.id, DialogAnswer::Cancel);
    let updates = h.ended(4);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(
        updates.contains(&RuntimeUpdate::TurnEnded(Outcome::Interrupted)),
        "{updates:#?}"
    );
    assert!(!updates.contains(&RuntimeUpdate::TurnEnded(Outcome::Failed)));
    assert!(replaced(&updates).is_none());
    assert_eq!(h_requests(&requests), 2, "nothing to summarize");
    h.stop();
    assert!(entries(&file, "branch_summary").is_empty());
}
