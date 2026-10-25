//! Branch summaries of tree navigation over the faux provider, adapted from
//! Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `test/agent-session-tree-navigation.test.ts` ("should create branch
//! summary when navigating with summarize=true", "should attach summary to
//! correct parent when navigating to nested user message", "should attach
//! summary to selected node when navigating to assistant message", "should
//! handle abort during summarization", "should support custom
//! summarization instructions", "should navigate between branches
//! correctly"), with Bake's checks of the summary request, the label, the
//! no-entries case, a failed summary, retry events, the caller's cancel,
//! and a resumed file rebuilding context with the summary. Pi's live-model
//! assertions on the summary's wording become checks of the request the
//! faux provider received. Each test owns its session, temporary directory,
//! and faux provider; waits are on channels.

use std::sync::{Arc, Mutex, PoisonError};

use bake_ai::providers::faux::{FauxFuture, FauxResponseStep, faux_assistant_message};
use bake_ai::{AbortController, Message};
use serde_json::{Value, json};

use super::control_tests::{file_session, harness};
use super::*;
use crate::session::messages::BRANCH_SUMMARY_PREFIX;
use crate::settings::RetrySettings;
use crate::test_support::TempDir;

const PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// The first user text of each request the faux provider received.
type Prompts = Arc<Mutex<Vec<String>>>;

fn first_user_text(messages: &[Message]) -> String {
    messages
        .iter()
        .find_map(|message| match message {
            Message::User(user) => Some(bake_ai::utils::text::user_content_text(&user.content, "")),
            _ => None,
        })
        .unwrap_or_default()
}

/// A reply of `text` that records the request's first user text.
fn recording(text: &str, prompts: &Prompts) -> FauxResponseStep {
    let (text, prompts) = (text.to_owned(), Arc::clone(prompts));
    FauxResponseStep::from_fn(move |context, _, _, _| {
        prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(first_user_text(context.messages()));
        Ok(faux_assistant_message(&text))
    })
}

fn prompts_of(prompts: &Prompts) -> Vec<String> {
    prompts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The ids of the entries holding `role` messages, in file order.
fn ids(session: &AgentSession, role: &str) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| entry.message_role() == Some(role))
        .map(|entry| entry.id().to_owned())
        .collect()
}

fn entry_count(session: &AgentSession) -> usize {
    lock(&session.session).entries().len()
}

fn summarize() -> TreeNavigationOptions {
    TreeNavigationOptions {
        summarize: true,
        ..TreeNavigationOptions::default()
    }
}

/// `u1 -> a1 -> u2 -> a2`, in a file session with `settings`.
async fn two_turns(
    dir: &TempDir,
    settings: Settings,
) -> (super::control_tests::Harness, AgentSession) {
    let h = harness();
    let session = h
        .session(file_session(dir), settings, None, Vec::new())
        .await;
    h.faux.set_responses(vec![
        faux_assistant_message("four").into(),
        faux_assistant_message("six").into(),
    ]);
    session
        .prompt("What is 2+2?", Vec::new())
        .await
        .expect("first");
    session
        .prompt("What is 3+3?", Vec::new())
        .await
        .expect("second");
    (h, session)
}

// Pi "should create branch summary when navigating with summarize=true":
// the summary of the branch left takes the first user message's place,
// as the new leaf; the message comes back for the editor. The summary request carries the
// abandoned entries and Pi's prompt, and the next request starts with the
// summary. A resumed file rebuilds the same context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_summary_of_the_branch_left_becomes_the_root_and_reaches_the_next_request() {
    let dir = TempDir::new("tree-summary-root");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let assistants = ids(&session, "assistant");
    let prompts: Prompts = Arc::default();
    h.faux
        .set_responses(vec![recording("## Goal\nArithmetic", &prompts)]);

    let navigated = session
        .navigate_tree(&users[0], summarize(), None)
        .await
        .expect("navigate");
    assert!(navigated.moved);
    assert_eq!(navigated.editor_text.as_deref(), Some("What is 2+2?"));
    let entry = navigated.summary_entry.expect("summary entry");
    assert_eq!(entry.entry_type(), "branch_summary");
    // The first user message's parent, the session's opening model and
    // thinking-level entries, Pi's root position.
    let parent = lock(&session.session)
        .entry(&users[0])
        .and_then(|entry| entry.parent_id().map(str::to_owned));
    assert_eq!(entry.parent_id().map(str::to_owned), parent);
    assert_eq!(session.leaf_id().as_deref(), Some(entry.id()));
    let json = entry.as_json();
    assert_eq!(json["fromId"], json!(assistants[1]));
    assert_eq!(
        json["summary"],
        json!(format!("{PREAMBLE}## Goal\nArithmetic"))
    );
    assert_eq!(
        json["details"],
        json!({ "readFiles": [], "modifiedFiles": [] })
    );
    assert_eq!(json["fromHook"], json!(false));
    assert!(json["usage"].is_object(), "{json:?}");
    assert!(!session.is_compacting());

    // The summary request: every abandoned entry (a1, u2, a2), then Pi's
    // prompt, without "Additional focus".
    let sent = prompts_of(&prompts);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].starts_with(
            "<conversation>\n[Assistant]: four\n\n[User]: What is 3+3?\n\n[Assistant]: six\n</conversation>\n\nCreate a structured summary of this conversation branch"
        ),
        "{}",
        sent[0]
    );
    assert!(!sent[0].contains("What is 2+2?"), "{}", sent[0]);
    assert!(sent[0].ends_with("Preserve exact file paths, function names, and error messages."));
    assert!(!sent[0].contains("Additional focus"));

    // The transcript is the summary alone, and the next request starts
    // with it.
    let roles: Vec<String> = session
        .messages()
        .iter()
        .map(|message| message.role().to_owned())
        .filter(|role| role != "system")
        .collect();
    assert_eq!(roles, ["branchSummary"]);
    let next: Prompts = Arc::default();
    h.faux.set_responses(vec![recording("ok", &next)]);
    session.prompt("again", Vec::new()).await.expect("again");
    assert_eq!(
        prompts_of(&next),
        [format!(
            "{BRANCH_SUMMARY_PREFIX}{PREAMBLE}## Goal\nArithmetic</summary>"
        )]
    );

    // Resume: the file rebuilds the summary, then the new branch.
    let file = session.session_file().expect("file");
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    let projection = reopened.build_session_projection();
    let roles: Vec<&str> = projection
        .messages
        .iter()
        .filter_map(|message| message.role())
        .filter(|role| *role != "system")
        .collect();
    assert_eq!(roles, ["branchSummary", "user", "assistant"]);
}

// Pi "should attach summary to correct parent when navigating to nested
// user message": the summary hangs from u2's parent, a1, beside u2, and
// summarizes only what lies below the common ancestor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_user_message_puts_the_summary_beside_it() {
    let dir = TempDir::new("tree-summary-nested");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let assistants = ids(&session, "assistant");
    let prompts: Prompts = Arc::default();
    h.faux.set_responses(vec![recording("nested", &prompts)]);

    let navigated = session
        .navigate_tree(&users[1], summarize(), None)
        .await
        .expect("navigate");
    assert_eq!(navigated.editor_text.as_deref(), Some("What is 3+3?"));
    let entry = navigated.summary_entry.expect("summary");
    assert_eq!(entry.parent_id(), Some(assistants[0].as_str()));
    let children: Vec<String> = lock(&session.session)
        .children(&assistants[0])
        .iter()
        .map(|entry| entry.entry_type().to_owned())
        .collect();
    assert_eq!(children.len(), 2);
    assert!(children.contains(&"branch_summary".to_owned()));
    assert!(children.contains(&"message".to_owned()));
    // u2 is the common ancestor: only a2 is summarized.
    assert!(
        prompts_of(&prompts)[0].starts_with("<conversation>\n[Assistant]: six\n</conversation>")
    );
    session.shutdown().await;
}

// Pi "should attach summary to selected node when navigating to assistant
// message", and "should navigate between branches correctly": the summary
// hangs from the chosen assistant entry and becomes the leaf; a branch
// left for another branch is summarized from its own leaf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_assistant_target_holds_the_summary_and_branches_summarize_their_own_path() {
    let dir = TempDir::new("tree-summary-assistant");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let assistants = ids(&session, "assistant");
    h.faux
        .set_responses(vec![faux_assistant_message("left a2").into()]);
    let navigated = session
        .navigate_tree(&assistants[0], summarize(), None)
        .await
        .expect("navigate");
    assert_eq!(navigated.editor_text, None);
    let first = navigated.summary_entry.expect("summary");
    assert_eq!(first.parent_id(), Some(assistants[0].as_str()));
    assert_eq!(session.leaf_id().as_deref(), Some(first.id()));

    // A branch from there, then back to u2 on the main branch.
    let prompts: Prompts = Arc::default();
    h.faux.set_responses(vec![
        faux_assistant_message("branched").into(),
        recording("left the branch", &prompts),
    ]);
    session
        .prompt("Branch path", Vec::new())
        .await
        .expect("branch");
    let navigated = session
        .navigate_tree(&users[1], summarize(), None)
        .await
        .expect("back");
    assert_eq!(navigated.editor_text.as_deref(), Some("What is 3+3?"));
    let second = navigated.summary_entry.expect("summary");
    assert_eq!(second.parent_id(), Some(assistants[0].as_str()));
    let sent = prompts_of(&prompts);
    assert!(sent[0].contains("[User]: Branch path"), "{}", sent[0]);
    assert!(sent[0].contains("left a2</summary>"), "{}", sent[0]);
    assert!(!sent[0].contains("What is 3+3?"), "{}", sent[0]);
    session.shutdown().await;
}

// Pi "should support custom summarization instructions", and
// `replaceInstructions`: custom instructions are appended as Pi's
// "Additional focus", or replace the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_instructions_add_focus_or_replace_the_prompt() {
    let dir = TempDir::new("tree-summary-custom");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let assistants = ids(&session, "assistant");
    let prompts: Prompts = Arc::default();
    h.faux.set_responses(vec![
        recording("focused", &prompts),
        recording("replaced", &prompts),
    ]);
    session
        .navigate_tree(
            &users[0],
            TreeNavigationOptions {
                summarize: true,
                custom_instructions: Some("MONKEY MONKEY MONKEY".into()),
                ..TreeNavigationOptions::default()
            },
            None,
        )
        .await
        .expect("focused");
    // Back to the old leaf, leaving the summary: it is summarized in turn.
    session
        .navigate_tree(
            &assistants[1],
            TreeNavigationOptions {
                summarize: true,
                custom_instructions: Some("Only list the numbers.".into()),
                replace_instructions: true,
                ..TreeNavigationOptions::default()
            },
            None,
        )
        .await
        .expect("replaced");
    let sent = prompts_of(&prompts);
    assert!(
        sent[0].ends_with("error messages.\n\nAdditional focus: MONKEY MONKEY MONKEY"),
        "{}",
        sent[0]
    );
    assert!(
        sent[1].ends_with("</conversation>\n\nOnly list the numbers."),
        "{}",
        sent[1]
    );
    assert!(
        !sent[1].contains("Create a structured summary"),
        "{}",
        sent[1]
    );
    session.shutdown().await;
}

// Pi's `label`: set on the summary entry when there is one, which leaves
// the label entry as the leaf; otherwise on the target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_label_marks_the_summary_or_else_the_target() {
    let dir = TempDir::new("tree-summary-label");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let assistants = ids(&session, "assistant");
    h.faux
        .set_responses(vec![faux_assistant_message("labelled").into()]);
    let navigated = session
        .navigate_tree(
            &users[1],
            TreeNavigationOptions {
                summarize: true,
                label: Some("tried twice".into()),
                ..TreeNavigationOptions::default()
            },
            None,
        )
        .await
        .expect("summary");
    let summary_id = navigated.summary_entry.expect("summary").id().to_owned();
    {
        let manager = lock(&session.session);
        assert_eq!(manager.label(&summary_id), Some("tried twice"));
        let leaf = manager.leaf_id().expect("leaf").to_owned();
        let label = manager.entry(&leaf).expect("label entry");
        assert_eq!(label.entry_type(), "label");
        assert_eq!(label.parent_id(), Some(summary_id.as_str()));
    }
    // The label entry is outside model context.
    let roles: Vec<String> = session
        .messages()
        .iter()
        .map(|message| message.role().to_owned())
        .filter(|role| role != "system")
        .collect();
    assert_eq!(roles, ["user", "assistant", "branchSummary"]);

    let navigated = session
        .navigate_tree(
            &assistants[1],
            TreeNavigationOptions {
                label: Some("main".into()),
                ..TreeNavigationOptions::default()
            },
            None,
        )
        .await
        .expect("label only");
    assert_eq!(navigated.summary_entry, None);
    {
        let manager = lock(&session.session);
        assert_eq!(manager.label(&assistants[1]), Some("main"));
        let leaf = manager.leaf_id().expect("leaf");
        assert_eq!(
            manager.entry(leaf).expect("label").parent_id(),
            Some(assistants[1].as_str())
        );
    }
    session.shutdown().await;
}

/// A summary response that says it started on `started`, then waits for
/// the request's signal and streams as aborted.
fn held_until_aborted(started: tokio::sync::mpsc::UnboundedSender<()>) -> FauxResponseStep {
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let signal = options.base.signal.clone();
        let started = started.clone();
        let wait: FauxFuture = Box::pin(async move {
            let _ = started.send(());
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            Ok(faux_assistant_message("## Goal\nNever written"))
        });
        wait
    }))
}

// Pi "should handle abort during summarization": while the summary runs
// the session counts as compacting; `abort_branch_summary`, or the
// caller's signal, aborts it, and nothing is written: the entries, the
// file, the leaf, and the transcript stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_summary_writes_nothing() {
    let dir = TempDir::new("tree-summary-abort");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let session = Arc::new(session);
    let users = ids(&session, "user");
    let file = session.session_file().expect("file");
    let bytes = std::fs::read(&file).expect("read");
    let entries = entry_count(&session);
    let leaf = session.leaf_id();
    let transcript = session.messages().len();

    for by_signal in [false, true] {
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        h.faux.set_responses(vec![held_until_aborted(started_tx)]);
        let abort = AbortController::new();
        let navigation = tokio::spawn({
            let (session, target, signal) =
                (Arc::clone(&session), users[0].clone(), abort.signal());
            async move {
                session
                    .navigate_tree(&target, summarize(), Some(signal))
                    .await
            }
        });
        started.recv().await.expect("the summary request started");
        assert!(session.is_compacting());
        assert!(session.prompt("refused", Vec::new()).await.is_err());
        if by_signal {
            abort.abort();
        } else {
            session.abort_branch_summary();
        }
        let result = navigation.await.expect("joined");
        assert_eq!(result, Err(BRANCH_SUMMARY_CANCELLED.to_owned()));
        assert_eq!(entry_count(&session), entries);
        assert_eq!(session.leaf_id(), leaf);
        assert_eq!(session.messages().len(), transcript);
        assert!(!session.is_compacting());
    }
    session.shutdown().await;
    assert_eq!(std::fs::read(&file).expect("read"), bytes);
}

// Pi `navigateTree`: a summary already made is written however late the
// cancel comes; only `result.aborted` stops the navigation. A cancel that
// reaches the commit boundary after the summary still moves the leaf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_after_the_summary_is_made_still_writes_it() {
    let dir = TempDir::new("tree-summary-late-cancel");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let users = ids(&session, "user");
    let prompts: Prompts = Arc::default();
    h.faux
        .set_responses(vec![recording("## Goal\nArithmetic", &prompts)]);
    let abort = AbortController::new();
    let late = abort.clone();
    session.core.hooks().before_navigation = Some(Box::new(move || late.abort()));
    let navigated = session
        .navigate_tree(&users[0], summarize(), Some(abort.signal()))
        .await
        .expect("navigated despite the late cancel");
    assert!(abort.signal().aborted());
    assert!(navigated.moved);
    let entry = navigated.summary_entry.expect("summary entry");
    assert_eq!(session.leaf_id().as_deref(), Some(entry.id()));
    assert_eq!(prompts_of(&prompts).len(), 1);
    session.shutdown().await;
}

// Pi: without entries to summarize, as when the target lies below the
// leaf, no request is sent and no summary is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_to_summarize_sends_no_request() {
    let dir = TempDir::new("tree-summary-empty");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let assistants = ids(&session, "assistant");
    session
        .navigate_tree(&assistants[0], TreeNavigationOptions::default(), None)
        .await
        .expect("up");
    let entries = entry_count(&session);
    let prompts: Prompts = Arc::default();
    h.faux.set_responses(vec![recording("unused", &prompts)]);
    let navigated = session
        .navigate_tree(&assistants[1], summarize(), None)
        .await
        .expect("down");
    assert!(navigated.moved);
    assert_eq!(navigated.summary_entry, None);
    assert_eq!(session.leaf_id().as_deref(), Some(assistants[1].as_str()));
    assert_eq!(entry_count(&session), entries);
    assert!(prompts_of(&prompts).is_empty());
    // The leaf itself is a no-op, summarized or not.
    let navigated = session
        .navigate_tree(&assistants[1], summarize(), None)
        .await
        .expect("no-op");
    assert_eq!(navigated, TreeNavigation::default());
    assert!(prompts_of(&prompts).is_empty());
    session.shutdown().await;
}

// Pi `throw new Error(result.error)`: a failed summary is refused with its
// error and changes nothing; the session stays usable. A missing target is
// refused before any request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_summary_changes_nothing() {
    let dir = TempDir::new("tree-summary-failed");
    let retry_off = Settings {
        retry: RetrySettings {
            enabled: false,
            ..RetrySettings::default()
        },
        ..Settings::default()
    };
    let (h, session) = two_turns(&dir, retry_off).await;
    let users = ids(&session, "user");
    let entries = entry_count(&session);
    let leaf = session.leaf_id();
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(|_, _, _, _| {
            Err("summary refused".to_owned())
        })]);
    let error = session
        .navigate_tree(&users[0], summarize(), None)
        .await
        .expect_err("failed");
    assert!(
        error.starts_with("Branch summarization failed: "),
        "{error}"
    );
    assert!(error.contains("summary refused"), "{error}");
    assert_eq!(entry_count(&session), entries);
    assert_eq!(session.leaf_id(), leaf);
    assert!(!session.is_compacting());
    assert_eq!(
        session
            .navigate_tree("missing", summarize(), None)
            .await
            .expect_err("missing"),
        "Entry missing not found"
    );
    h.faux
        .set_responses(vec![faux_assistant_message("still here").into()]);
    session.prompt("later", Vec::new()).await.expect("usable");
    session.shutdown().await;
}

// Pi `_summarizationRetryCallbacks({ source: "branchSummary" })`: a
// transient summary error is retried under the retry settings, announced
// with the branch summary as its source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_summary_reports_the_branch_summary_source() {
    let dir = TempDir::new("tree-summary-retry");
    let quick = Settings {
        retry: RetrySettings {
            enabled: true,
            max_retries: 2,
            base_delay_ms: 1,
            ..RetrySettings::default()
        },
        ..Settings::default()
    };
    let (h, session) = two_turns(&dir, quick).await;
    let users = ids(&session, "user");
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let _subscription = session.subscribe({
        let events = Arc::clone(&events);
        move |event| {
            let json = crate::json_event::session_event_json(event);
            if json["type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("summarization_retry"))
            {
                events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(json);
            }
        }
    });
    h.faux.set_responses(vec![
        FauxResponseStep::from_fn(|_, _, _, _| Err("overloaded_error".to_owned())),
        faux_assistant_message("after a retry").into(),
    ]);
    let navigated = session
        .navigate_tree(&users[0], summarize(), None)
        .await
        .expect("retried");
    assert!(navigated.summary_entry.is_some());
    let events = events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "summarization_retry_scheduled",
            "summarization_retry_attempt_start",
            "summarization_retry_finished"
        ]
    );
    assert_eq!(
        events[1],
        json!({ "type": "summarization_retry_attempt_start", "source": "branchSummary" })
    );
    session.shutdown().await;
}

// Bake: shutdown aborts a running branch summary, as Pi's `dispose` calls
// `abortBranchSummary`; nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_aborts_a_running_summary() {
    let dir = TempDir::new("tree-summary-shutdown");
    let (h, session) = two_turns(&dir, Settings::default()).await;
    let session = Arc::new(session);
    let users = ids(&session, "user");
    let file = session.session_file().expect("file");
    let bytes = std::fs::read(&file).expect("read");
    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    h.faux.set_responses(vec![held_until_aborted(started_tx)]);
    let navigation = tokio::spawn({
        let (session, target) = (Arc::clone(&session), users[0].clone());
        async move { session.navigate_tree(&target, summarize(), None).await }
    });
    started.recv().await.expect("started");
    session.shutdown().await;
    assert_eq!(
        navigation.await.expect("joined"),
        Err(BRANCH_SUMMARY_CANCELLED.to_owned())
    );
    assert_eq!(std::fs::read(&file).expect("read"), bytes);
}
