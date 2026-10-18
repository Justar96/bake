//! Tree navigation and forking, adapted from Pi v1.2.0
//! (`06a466935536f8c9bcacb2d5efc0fc021be1bdff`)
//! `test/agent-session-tree-navigation.test.ts` (user, root, and assistant
//! targets, no-op at the leaf, no entries without a summary),
//! `test/agent-session-branching.test.ts` (forking from a single message,
//! in memory, and from the middle), `test/suite/regressions/
//! tree-during-streaming.test.ts` and `9178-tree-during-compaction.test.ts`
//! (busy refusals keep the leaf), and Pi v1.3.0
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `test/suite/agent-session-runtime.test.ts` (forking at the current
//! position, persisted and in memory, and an unsaved session refused), with Bake's checks of the next request's
//! context, the file's branches, an untouched source file, and the live
//! model and thinking level. Each test owns its session, temporary
//! directory, and faux provider; waits are on notifications.

use std::sync::{Arc, Mutex};

use bake_ai::Message;
use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use serde_json::Value;
use tokio::sync::Notify;

use super::control_tests::{file_session, harness, in_memory};
use super::*;
use crate::session::AgentMessage;
use crate::test_support::TempDir;

/// Each user and assistant text of a request, `role: text`.
fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(format!(
                "user: {}",
                bake_ai::utils::text::user_content_text(&user.content, "")
            )),
            Message::Assistant(assistant) => Some(format!(
                "assistant: {}",
                bake_ai::utils::text::assistant_text(assistant)
            )),
            _ => None,
        })
        .collect()
}

fn live_texts(session: &AgentSession) -> Vec<String> {
    let llm: Vec<Message> = convert_to_llm(&session.messages());
    texts(&llm)
}

/// A faux reply that records the request's user and assistant texts and
/// its model.
/// Each request's model id and texts.
type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn recording(reply: &str, seen: &Seen) -> FauxResponseStep {
    let (reply, seen) = (reply.to_owned(), Arc::clone(seen));
    FauxResponseStep::from_fn(move |context, _, _, model| {
        seen.lock()
            .unwrap()
            .push((model.id.clone(), texts(context.messages())));
        Ok(faux_assistant_message(&reply))
    })
}

fn entry_ids(session: &AgentSession, role: &str) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| {
            entry
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some(role)
        })
        .map(|entry| entry.id().to_owned())
        .collect()
}

async fn two_turns(h: &super::control_tests::Harness, session: &AgentSession) {
    h.faux.set_responses(vec![
        faux_assistant_message("one").into(),
        faux_assistant_message("two").into(),
    ]);
    session.prompt("first", Vec::new()).await.expect("first");
    session.prompt("second", Vec::new()).await.expect("second");
}

// Pi "should navigate to user message and put text in editor", nested:
// the leaf moves to the message's parent, its text comes back, nothing is
// written, and the next request carries the new path; the file then holds
// both branches under the parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_message_comes_back_and_the_next_prompt_branches_from_its_parent() {
    let dir = TempDir::new("tree-user");
    let h = harness();
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let file = session.session_file().expect("file");
    let before = std::fs::read(&file).expect("read");
    let users = entry_ids(&session, "user");
    let assistants = entry_ids(&session, "assistant");

    let navigated = session
        .navigate_tree(&users[1], None)
        .await
        .expect("navigate");
    assert_eq!(
        navigated,
        TreeNavigation {
            moved: true,
            editor_text: Some("second".into())
        }
    );
    assert_eq!(session.leaf_id().as_deref(), Some(assistants[0].as_str()));
    assert_eq!(live_texts(&session), ["user: first", "assistant: one"]);
    // Pi writes nothing without a summary.
    assert_eq!(std::fs::read(&file).expect("read"), before);
    assert!(!session.is_compacting());

    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![recording("three", &seen)]);
    session.prompt("again", Vec::new()).await.expect("again");
    assert_eq!(
        seen.lock().unwrap()[0].1,
        ["user: first", "assistant: one", "user: again"]
    );
    let children: Vec<String> = lock(&session.session)
        .children(&assistants[0])
        .iter()
        .map(|entry| entry.id().to_owned())
        .collect();
    assert_eq!(children.len(), 2, "both branches stay under the parent");
    assert!(children.contains(&users[1]));
    // A resumed copy of the file rebuilds the new branch.
    let reopened = SessionManager::open(&file, None, None);
    assert!(matches!(
        reopened,
        Err(crate::session::SessionError::Locked(_))
    ));
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("open");
    let context: Vec<AgentMessage> = reopened.build_session_context().messages;
    let live: Vec<_> = context.iter().map(AgentMessage::to_agent).collect();
    assert_eq!(
        texts(&convert_to_llm(&live)),
        [
            "user: first",
            "assistant: one",
            "user: again",
            "assistant: three"
        ]
    );
}

// Pi "should navigate to non-user message without editor text" and the
// root user message case: an assistant becomes the leaf; a root user
// message empties the transcript of messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_assistant_becomes_the_leaf_and_a_root_user_message_empties_the_path() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let users = entry_ids(&session, "user");
    let assistants = entry_ids(&session, "assistant");

    let navigated = session
        .navigate_tree(&assistants[0], None)
        .await
        .expect("a1");
    assert_eq!(navigated.editor_text, None);
    assert_eq!(session.leaf_id().as_deref(), Some(assistants[0].as_str()));
    assert_eq!(live_texts(&session), ["user: first", "assistant: one"]);

    let navigated = session.navigate_tree(&users[0], None).await.expect("u1");
    assert_eq!(navigated.editor_text.as_deref(), Some("first"));
    assert!(live_texts(&session).is_empty());
    let parent = lock(&session.session)
        .entry(&users[0])
        .and_then(|entry| entry.parent_id().map(str::to_owned));
    assert_eq!(session.leaf_id(), parent);

    // Repeated navigation back to the original leaf restores its context.
    session
        .navigate_tree(&assistants[1], None)
        .await
        .expect("a2");
    assert_eq!(
        live_texts(&session),
        [
            "user: first",
            "assistant: one",
            "user: second",
            "assistant: two"
        ]
    );
}

// Pi "should handle navigation to same position (no-op)", a missing
// target, and a failed write's refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_leaf_is_a_no_op_and_a_missing_target_changes_nothing() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let leaf = session.leaf_id().expect("leaf");
    let count = lock(&session.session).entry_count();
    let navigated = session.navigate_tree(&leaf, None).await.expect("no-op");
    assert!(!navigated.moved);
    let error = session
        .navigate_tree("missing", None)
        .await
        .expect_err("missing");
    assert_eq!(error, "Entry missing not found");
    assert_eq!(session.leaf_id(), Some(leaf));
    assert_eq!(lock(&session.session).entry_count(), count);
    assert_eq!(live_texts(&session).len(), 4);
    session.shutdown().await;
    assert!(session.navigate_tree("missing", None).await.is_err());
}

// Pi "tree navigation during an active response" and #9178: a running
// response or compaction refuses navigation with Pi's messages and keeps
// the leaf; while navigation holds the session, prompts and steers are
// refused and `is_compacting` says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_sessions_refuse_navigation_and_navigation_refuses_prompts() {
    let h = harness();
    let session = Arc::new(
        h.session(in_memory(), Settings::default(), None, Vec::new())
            .await,
    );
    two_turns(&h, &session).await;
    let target = entry_ids(&session, "assistant")[0].clone();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let (started, release) = (started.clone(), release.clone());
        h.faux
            .set_responses(vec![FauxResponseStep::Factory(Arc::new(
                move |_, _, _, _| {
                    let (started, release) = (started.clone(), release.clone());
                    Box::pin(async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(faux_assistant_message("late"))
                    })
                },
            ))]);
    }
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("third", Vec::new()).await })
    };
    started.notified().await;
    let leaf = session.leaf_id();
    assert_eq!(
        session
            .navigate_tree(&target, None)
            .await
            .expect_err("streaming"),
        NAVIGATION_WHILE_STREAMING
    );
    assert_eq!(session.leaf_id(), leaf);
    release.notify_one();
    run.await.expect("join").expect("prompt");

    let leaf = session.leaf_id();
    session
        .core
        .activity
        .send_modify(|activity| activity.manual_compaction = true);
    assert_eq!(
        session
            .navigate_tree(&target, None)
            .await
            .expect_err("compacting"),
        NAVIGATION_WHILE_BUSY
    );
    session
        .core
        .activity
        .send_modify(|activity| activity.manual_compaction = false);
    assert_eq!(session.leaf_id(), leaf);

    session
        .core
        .activity
        .send_modify(|activity| activity.navigation = true);
    assert!(session.is_compacting());
    assert!(session.prompt("blocked", Vec::new()).await.is_err());
    assert_eq!(
        session
            .steer("blocked", Vec::new())
            .await
            .expect_err("steer"),
        NAVIGATION_WHILE_BUSY
    );
    assert_eq!(
        session
            .navigate_tree(&target, None)
            .await
            .expect_err("navigating"),
        NAVIGATION_WHILE_BUSY
    );
    session
        .core
        .activity
        .send_modify(|activity| activity.navigation = false);
    assert!(session.queued_messages().is_empty());
    session
        .navigate_tree(&target, None)
        .await
        .expect("idle again");
    assert!(!session.is_compacting());
}

// Pi keeps the live model and thinking level across tree navigation: a
// change recorded on the branch left behind is not undone, and the next
// request uses the live selection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigation_keeps_the_live_model_and_thinking_level() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let target = entry_ids(&session, "assistant")[0].clone();
    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect("model");
    session
        .set_thinking_level(ModelThinkingLevel::Low, false)
        .await
        .expect("level");
    session
        .navigate_tree(&target, None)
        .await
        .expect("navigate");
    assert_eq!(session.model().expect("model").id, "faux-2");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux.set_responses(vec![recording("ok", &seen)]);
    session.prompt("next", Vec::new()).await.expect("next");
    assert_eq!(seen.lock().unwrap()[0].0, "faux-2");
}

// Pi "should allow forking from single message" and "should fork from
// middle of conversation", persisted: the fork holds the path before the
// chosen message in its own file, naming the source as its parent; the
// source file is untouched and its writer keeps writing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_copies_the_path_and_leaves_the_source_file_alone() {
    let dir = TempDir::new("fork-file");
    let h = harness();
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let file = session.session_file().expect("file");
    let before = std::fs::read(&file).expect("read");
    let points = session.user_messages_for_forking();
    assert_eq!(
        points.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );

    let forked = session
        .fork_session_manager(&points[1].entry_id, ForkPosition::Before)
        .await
        .expect("fork");
    assert_eq!(forked.selected_text.as_deref(), Some("second"));
    let fork_file = forked
        .session
        .session_file()
        .expect("fork file")
        .to_path_buf();
    assert_ne!(fork_file, file);
    assert_eq!(fork_file.parent(), file.parent(), "same session directory");
    assert!(fork_file.exists());
    assert_ne!(forked.session.session_id(), session.session_id());
    assert_eq!(
        forked
            .session
            .header()
            .and_then(|h| h.parent_session().map(str::to_owned)),
        Some(file.to_string_lossy().into_owned())
    );
    let live: Vec<_> = forked
        .session
        .build_session_context()
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    assert_eq!(
        texts(&convert_to_llm(&live)),
        ["user: first", "assistant: one"]
    );
    assert_eq!(std::fs::read(&file).expect("read"), before);
    // The fork holds its own file's lock, the source keeps its own.
    assert!(matches!(
        SessionManager::open(&fork_file, None, None),
        Err(crate::session::SessionError::Locked(_))
    ));
    h.faux
        .set_responses(vec![faux_assistant_message("still here").into()]);
    session
        .prompt("third", Vec::new())
        .await
        .expect("source writes");
    assert!(std::fs::read(&file).expect("read").len() > before.len());

    // Forking before the first message keeps only what precedes it, the
    // system prompt and settings entries (Pi #8724: roles `["system"]`),
    // and is written only once it has a conversation.
    let empty = session
        .fork_session_manager(&points[0].entry_id, ForkPosition::Before)
        .await
        .expect("root fork");
    assert_eq!(empty.selected_text.as_deref(), Some("first"));
    let roles: Vec<String> = empty
        .session
        .build_session_context()
        .messages
        .iter()
        .filter_map(|message| message.role().map(str::to_owned))
        .collect();
    assert_eq!(roles, ["system"]);
    assert!(!empty.session.session_file().expect("path").exists());
    assert!(
        empty
            .session
            .header()
            .and_then(|h| h.parent_session())
            .is_some()
    );

    let assistant = entry_ids(&session, "assistant")[0].clone();
    assert_eq!(
        session
            .fork_session_manager(&assistant, ForkPosition::Before)
            .await
            .expect_err("not a user message"),
        "Invalid entry ID for forking"
    );
}

// Pi "should support in-memory forking in --no-session mode".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_memory_session_forks_in_memory() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let points = session.user_messages_for_forking();
    let forked = session
        .fork_session_manager(&points[1].entry_id, ForkPosition::Before)
        .await
        .expect("fork");
    assert!(!forked.session.is_persisted());
    assert_eq!(forked.session.session_file(), None);
    assert_eq!(forked.selected_text.as_deref(), Some("second"));
    assert_eq!(live_texts(&session).len(), 4, "the source is unchanged");
}

// Pi "duplicates the current active branch when forking at the current
// position": the fork at the leaf is a new file in the same directory,
// naming the source as its parent, with every message of the path; the
// source file is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_at_the_leaf_duplicates_the_active_branch() {
    let dir = TempDir::new("fork-at");
    let h = harness();
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let file = session.session_file().expect("file");
    let before = std::fs::read(&file).expect("read");
    let leaf = session.leaf_id().expect("leaf");

    let forked = session
        .fork_session_manager(&leaf, ForkPosition::At)
        .await
        .expect("fork");
    assert_eq!(forked.selected_text, None);
    let fork_file = forked
        .session
        .session_file()
        .expect("fork file")
        .to_path_buf();
    assert_ne!(fork_file, file);
    assert_eq!(fork_file.parent(), file.parent(), "same session directory");
    assert!(fork_file.exists());
    assert_ne!(forked.session.session_id(), session.session_id());
    assert_eq!(
        forked
            .session
            .header()
            .and_then(|h| h.parent_session().map(str::to_owned)),
        Some(file.to_string_lossy().into_owned())
    );
    assert_eq!(forked.session.leaf_id(), Some(leaf.as_str()));
    let live: Vec<_> = forked
        .session
        .build_session_context()
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    assert_eq!(texts(&convert_to_llm(&live)), live_texts(&session));
    assert_eq!(std::fs::read(&file).expect("read"), before);

    // At an assistant entry, the fork ends with that entry.
    let assistant = entry_ids(&session, "assistant")[0].clone();
    let at_assistant = session
        .fork_session_manager(&assistant, ForkPosition::At)
        .await
        .expect("fork at assistant");
    let live: Vec<_> = at_assistant
        .session
        .build_session_context()
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    assert_eq!(
        texts(&convert_to_llm(&live)),
        ["user: first", "assistant: one"]
    );
    assert_eq!(
        session
            .fork_session_manager("missing-entry", ForkPosition::At)
            .await
            .expect_err("missing"),
        "Invalid entry ID for forking"
    );
}

// Pi "duplicates the current active branch in-memory when forking at the
// current position".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_memory_fork_at_the_leaf_stays_in_memory() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    two_turns(&h, &session).await;
    let leaf = session.leaf_id().expect("leaf");
    let forked = session
        .fork_session_manager(&leaf, ForkPosition::At)
        .await
        .expect("fork");
    assert!(!forked.session.is_persisted());
    assert_eq!(forked.session.session_file(), None);
    assert_eq!(forked.selected_text, None);
    let live: Vec<_> = forked
        .session
        .build_session_context()
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    assert_eq!(texts(&convert_to_llm(&live)), live_texts(&session));
    assert_eq!(live_texts(&session).len(), 4, "the source is unchanged");
}

// Pi "reports why an unflushed session cannot be forked": a persisted
// session whose file is not written yet refuses a fork at its leaf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsaved_session_refuses_a_fork_at_its_leaf() {
    let dir = TempDir::new("fork-at-unsaved");
    let h = harness();
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    let file = session.session_file().expect("file");
    assert!(!file.exists());
    let leaf = session.leaf_id().expect("leaf");
    assert_eq!(
        session
            .fork_session_manager(&leaf, ForkPosition::At)
            .await
            .expect_err("unsaved"),
        "This session has not been saved yet. Send a message before cloning or forking it."
    );
}

// `SessionManager::branched_copy`, Pi's runtime `fork` at a root user
// message (`newSession({ parentSession })`) and below one
// (`createBranchedSession`), on the store alone: the source keeps its
// entries, leaf, file bytes, and writer lock.
#[test]
fn branched_copies_never_change_their_source() {
    let dir = TempDir::new("branched-copy");
    let mut source = file_session(&dir);
    let user = source
        .append_message(bake_ai::UserMessage {
            content: bake_ai::UserContent::Text("hi".into()),
            timestamp: 1,
        })
        .expect("user");
    let reply = source
        .append_message(faux_assistant_message("hello"))
        .expect("assistant");
    let file = source.session_file().expect("file").to_path_buf();
    let before = std::fs::read(&file).expect("read");

    let empty = source.branched_copy(None).expect("root");
    assert_eq!(empty.entry_count(), 0);
    assert_eq!(
        empty
            .header()
            .and_then(|h| h.parent_session().map(str::to_owned)),
        Some(file.to_string_lossy().into_owned())
    );
    assert!(!empty.session_file().expect("path").exists());

    let copy = source.branched_copy(Some(&user)).expect("copy");
    assert_eq!(copy.entry_count(), 1);
    // A user message is a conversation, so the fork's file is written now.
    assert!(copy.session_file().expect("path").exists());
    drop(copy);
    let copy = source.branched_copy(Some(&reply)).expect("copy");
    assert_eq!(copy.entry_count(), 2);

    assert_eq!(source.leaf_id(), Some(reply.as_str()));
    assert_eq!(source.entry_count(), 2);
    assert_eq!(std::fs::read(&file).expect("read"), before);
    source
        .append_message(faux_assistant_message("more"))
        .expect("the source still writes");
}

// Bake: a navigation held after it claimed the session, before it moves
// the leaf, keeps prompts and steers out (a second navigation waits its
// turn behind it, as every change does, where Pi #9178 rejects it); a cancel that
// arrives while it is held moves nothing (the commit boundary), and an
// uncancelled one moves the leaf when released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_navigation_excludes_input_and_a_cancel_before_its_commit_moves_nothing() {
    let h = harness();
    let session = Arc::new(
        h.session(in_memory(), Settings::default(), None, Vec::new())
            .await,
    );
    two_turns(&h, &session).await;
    let users = entry_ids(&session, "user");
    let leaf = session.leaf_id();
    let before = live_texts(&session);

    for cancel in [true, false] {
        let (reached_tx, reached) = std::sync::mpsc::channel::<()>();
        let (release_tx, release) = std::sync::mpsc::channel::<()>();
        session.core.hooks().before_navigation = Some(Box::new(move || {
            let _ = reached_tx.send(());
            let _ = release.recv();
        }));
        let abort = bake_ai::AbortController::new();
        let held = {
            let (session, target, signal) =
                (Arc::clone(&session), users[1].clone(), abort.signal());
            tokio::spawn(async move { session.navigate_tree(&target, Some(signal)).await })
        };
        tokio::task::spawn_blocking(move || reached.recv().expect("held"))
            .await
            .expect("join");
        assert!(session.is_compacting());
        assert!(session.prompt("blocked", Vec::new()).await.is_err());
        assert_eq!(
            session
                .steer("blocked", Vec::new())
                .await
                .expect_err("steer"),
            NAVIGATION_WHILE_BUSY
        );
        assert_eq!(session.leaf_id(), leaf, "nothing moved while held");
        if cancel {
            abort.abort();
        }
        release_tx.send(()).expect("release");
        let result = held.await.expect("join");
        assert!(!session.is_compacting());
        if cancel {
            assert_eq!(result.expect_err("cancelled"), NAVIGATION_CANCELLED);
            assert_eq!(session.leaf_id(), leaf);
            assert_eq!(live_texts(&session), before);
        } else {
            assert_eq!(
                result.expect("moved").editor_text.as_deref(),
                Some("second")
            );
            assert_eq!(live_texts(&session), ["user: first", "assistant: one"]);
        }
    }
    // A navigation already cancelled is refused before it claims anything.
    let abort = bake_ai::AbortController::new();
    abort.abort();
    assert_eq!(
        session
            .navigate_tree(&users[0], Some(abort.signal()))
            .await
            .expect_err("cancelled"),
        NAVIGATION_CANCELLED
    );
    assert!(session.queued_messages().is_empty());
}
