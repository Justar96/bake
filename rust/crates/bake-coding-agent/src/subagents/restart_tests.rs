//! Scope 11 restart proofs for subagents ([D38]): a parent process killed
//! with `SIGKILL` from the test while a background child runs, once the
//! child's settlement is posted and not yet delivered, once its notice is
//! synced, and while a foreground child runs. The child is a session in
//! the parent's process, so the kill ends both; the test reopens both
//! files, resumes the parent in this process, and checks the next request,
//! the notices, the child's requests, and continuing the child.
//!
//! [D38]: ../../../../../docs/roadmap/rust-0.4/scope-00/support.md

use bake_agent::{AgentEvent, AgentMessage};
use tokio::sync::Notify;

use super::*;
use crate::agent_session::SessionEvent;
use crate::jobs::crash_fixture::{halt, kill_fixture};

const CHILD_MODE: &str = "BAKE_TEST_SUBAGENT_CRASH_MODE";
const CHILD_DIR: &str = "BAKE_TEST_SUBAGENT_CRASH_DIR";

/// The lost-run notice's words.
const INTERRUPTED: &str = "was interrupted when Bake stopped and did not finish";

/// Scripts the killed parent's model: the parent delegates `Work.` and
/// waits; the child halts the process or answers once the parent's turn
/// is over (`parent_done`), so each kill point follows a finished,
/// synced parent turn. A foreground child halts at once, with the
/// parent's call open.
fn fixture_model(h: &Harness, mode: &str, dir: &std::path::Path, parent_done: &Arc<Notify>) {
    let steps = (0..16)
        .map(|_| {
            let (mode, dir, parent_done) =
                (mode.to_owned(), dir.to_owned(), Arc::clone(parent_done));
            FauxResponseStep::Factory(Arc::new(move |context, _, _, _| {
                let messages = context.messages().to_vec();
                let (mode, dir, parent_done) =
                    (mode.clone(), dir.clone(), Arc::clone(&parent_done));
                Box::pin(async move {
                    if is_child(&messages) {
                        if mode == "foreground-running" {
                            halt(&dir);
                        }
                        parent_done.notified().await;
                        if mode == "child-running" {
                            halt(&dir);
                        }
                        return Ok(faux_assistant_message("child answer"));
                    }
                    if last_user(&messages).starts_with("Background subagent") {
                        return Ok(faux_assistant_message("read the notice"));
                    }
                    if after_tool(&messages) {
                        return Ok(faux_assistant_message("waiting for the child"));
                    }
                    Ok(call(
                        "subagent",
                        json!({
                            "description": "work",
                            "prompt": "Work.",
                            "run_in_background": mode != "foreground-running",
                        }),
                    ))
                })
            }))
        })
        .collect();
    h.faux.set_responses(steps);
}

/// Runs a parent with one child to the kill point its mode names and
/// halts there: `child-running`, `notice-posted`, `notice-delivered`, or
/// `foreground-running`.
#[test]
fn subagent_crash_fixture() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(CHILD_DIR).expect("fixture directory"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        // In the test's directory, which the test removes: the kill skips
        // the harness's cleanup ([`kill_fixture`]).
        let h = harness("subagent-crash-fixture");
        std::fs::write(dir.join("place"), h.place.to_string_lossy().as_bytes()).expect("place");
        let parent_done = Arc::new(Notify::new());
        fixture_model(&h, &mode, &dir, &parent_done);
        let parent = parent(&h, None).await;
        let file = parent.session.session_file().expect("parent file");
        std::fs::write(dir.join("parent"), file.to_string_lossy().as_bytes()).expect("path");
        let delivered = dir.clone();
        let _subscription = parent.session.subscribe(move |event| {
            if let SessionEvent::Agent(AgentEvent::MessageEnd {
                message: AgentMessage::Custom(custom),
            }) = event
                && custom
                    .as_any()
                    .downcast_ref::<CustomMessage>()
                    .is_some_and(|custom| custom.custom_type == messages::NOTICE_TYPE)
            {
                halt(&delivered);
            }
        });
        let _ = parent.session.prompt("delegate", Vec::new()).await;
        parent_done.notify_one();
        if mode == "child-running" {
            std::future::pending::<()>().await;
        }
        let jobs = parent.session.jobs().clone();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let mut events = jobs.subscribe();
                if parent.host.running() == 0 && jobs.has_undelivered() {
                    return;
                }
                events.changed().await;
            }
        })
        .await
        .expect("the child's notice was posted");
        if mode == "notice-posted" {
            halt(&dir);
        }
        let _ = parent.session.deliver_job_notices().await;
        panic!("the fixture did not reach its kill point");
    });
}

/// What a killed fixture left: its directory, which holds everything the
/// fixture made, its sessions directory, and the parent's file.
struct Killed {
    _dir: TempDir,
    place: PathBuf,
    parent: PathBuf,
}

async fn kill_at(mode: &str) -> Killed {
    let dir = TempDir::new(&format!("subagent-kill-{mode}"));
    kill_fixture(
        "subagents::tests::restart::subagent_crash_fixture",
        &[
            (CHILD_MODE, mode.as_ref()),
            (CHILD_DIR, dir.path().as_os_str()),
        ],
        dir.path(),
    )
    .await;
    let read = |name: &str| PathBuf::from(std::fs::read_to_string(dir.join(name)).expect(name));
    let place = read("place");
    assert!(place.starts_with(dir.path()), "{mode}: {place:?}");
    Killed {
        place,
        parent: read("parent"),
        _dir: dir,
    }
}

/// Whether every tool call the provider would be sent is answered: Pi's
/// transform, as every adapter applies it, then call and result ids.
fn provider_valid(messages: &[Message], model: &Model) -> bool {
    let sent = bake_ai::api::transform_messages::transform_messages(messages, model, None);
    let calls: Vec<&str> = sent
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(&assistant.content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            bake_ai::AssistantContentBlock::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = sent
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    calls == results
}

/// The notices of the parent's file, as their run and stop reason.
fn file_notices(path: &std::path::Path) -> Vec<(u64, String)> {
    std::fs::read_to_string(path)
        .expect("parent file")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["customType"] == messages::NOTICE_TYPE)
        .map(|entry| {
            (
                entry["details"]["runId"].as_u64().unwrap_or_default(),
                entry["details"]["stopReason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

// A parent killed with each child's run at a different point. Both files
// load; the resumed parent's next request is provider-valid and carries
// one notice for a background run: the interrupted notice for a run in
// flight at the kill (`child-running`); for a run that settled before it,
// whose settlement the file records before its notice is posted
// (`notice-posted`), the same finished notice the live parent would have
// delivered; and none again for a notice already synced
// (`notice-delivered`). A foreground child's open call gets Pi's synthetic
// result and no notice. No child is re-run, a second resume reports
// nothing, and a background child continues from its file; each run's
// notice is in the file once, in run order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_parent_reports_each_child_run_once_and_reruns_none() {
    for (mode, first, recorded, notices_before) in [
        ("child-running", Some("interrupted"), false, 0),
        ("notice-posted", Some("completed"), true, 0),
        ("notice-delivered", Some("completed"), true, 1),
        ("foreground-running", None, false, 0),
    ] {
        let killed = kill_at(mode).await;
        let background = first.is_some();

        // Both files load, and the child's names its parent.
        let parent_store =
            SessionManager::open(&killed.parent, Some(&killed.place), None).expect("parent loads");
        let children: Vec<PathBuf> = std::fs::read_dir(children_dir(&killed.place))
            .expect("children directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect();
        assert_eq!(children.len(), 1, "{mode}: {children:?}");
        let child_store =
            SessionManager::open(&children[0], Some(&children_dir(&killed.place)), None)
                .expect("child loads");
        let header: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&children[0])
                .expect("child file")
                .lines()
                .next()
                .expect("a header"),
        )
        .expect("header json");
        assert_eq!(
            header["parentSession"],
            json!(killed.parent.to_string_lossy()),
            "{mode}"
        );
        let child_texts: Vec<String> = child_store
            .build_session_context()
            .messages
            .iter()
            .filter_map(|message| user_text(message.to_agent().as_llm()?))
            .collect();
        assert!(
            child_texts
                .first()
                .is_some_and(|text| text.starts_with("Work.\n")),
            "{mode}: the child's task is in its file: {child_texts:?}"
        );
        drop(child_store);
        assert_eq!(file_notices(&killed.parent).len(), notices_before, "{mode}");
        assert_eq!(
            file_settled(&killed.parent),
            if recorded {
                vec![(1, "completed".to_owned())]
            } else {
                Vec::new()
            },
            "{mode}: the settlement the killed parent recorded"
        );

        let h = harness_in(
            &format!("subagent-resume-{mode}"),
            Some(killed.place.clone()),
        );
        let seen: Arc<Mutex<Vec<Vec<Message>>>> = Arc::default();
        let child_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let seen = Arc::clone(&seen);
            let child_requests = Arc::clone(&child_requests);
            h.think(Arc::new(move |_, messages| {
                if is_child(messages) {
                    child_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Some(faux_assistant_message("resumed answer"));
                }
                seen.lock().expect("seen").push(messages.to_vec());
                Some(faux_assistant_message("ok"))
            }));
        }
        let resumed = parent(&h, Some(parent_store)).await;
        let listed = resumed.host.list();
        if background {
            assert_eq!(listed.len(), 1, "{mode}");
            assert_eq!(listed[0].status, Status::Ready, "{mode}");
        } else {
            assert!(
                listed.is_empty(),
                "{mode}: an unanswered foreground start records no child"
            );
        }
        bounded("the prompt", resumed.session.prompt("next", Vec::new()))
            .await
            .expect("prompt");
        let request = seen.lock().expect("seen")[0].clone();
        assert!(
            provider_valid(&request, &h.model("faux-1")),
            "{mode}: {request:#?}"
        );
        let texts: Vec<String> = request.iter().filter_map(user_text).collect();
        let notices: Vec<&String> = texts
            .iter()
            .filter(|text| text.starts_with("Background subagent"))
            .collect();
        match first {
            Some("interrupted") => {
                assert_eq!(notices.len(), 1, "{mode}: {texts:#?}");
                assert!(notices[0].contains(INTERRUPTED), "{mode}: {texts:#?}");
            }
            Some(_) => {
                // The live parent's notice, word for word.
                let id = &listed[0].id;
                assert_eq!(
                    notices,
                    [&format!(
                        "Background subagent {id} finished and will do no further work unless you send it more.\nIts closing message:\nchild answer"
                    )],
                    "{mode}"
                );
            }
            None => assert!(notices.is_empty(), "{mode}: {texts:#?}"),
        }
        if !background {
            let synthetic = bake_ai::api::transform_messages::transform_messages(
                &request,
                &h.model("faux-1"),
                None,
            );
            assert!(
                synthetic
                    .iter()
                    .any(|message| matches!(message, Message::ToolResult(result)
                    if result.tool_name == "subagent" && result.is_error)),
                "{mode}: the open call is answered by Pi's synthetic result"
            );
        }
        let child_requests = || child_requests.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(child_requests(), 0, "{mode}: a child was re-run");
        let mut expected: Vec<(u64, String)> =
            first.map(|stop| (1, stop.to_owned())).into_iter().collect();
        assert_eq!(file_notices(&killed.parent), expected, "{mode}");

        // A background child continues from its file as run 2.
        if background {
            let id = listed[0].id.clone();
            let (_, run) = bounded("the message", resumed.host.message_child(&id, "resume"))
                .await
                .expect("continue the child");
            assert_eq!(run, Some(2), "{mode}");
            settled(&resumed).await;
            assert_eq!(child_requests(), 1, "{mode}");
            expected.push((2, "completed".to_owned()));
        }
        bounded("the resumed parent's shutdown", resumed.session.shutdown()).await;
        assert!(
            resumed.work.is_closed(),
            "{mode}: the parent's work outlived it"
        );

        // A second resume reports nothing more, and each run's notice is
        // in the file exactly once, in run order.
        let store =
            SessionManager::open(&killed.parent, Some(&killed.place), None).expect("reopen");
        assert!(lost_runs(&store).is_empty(), "{mode}");
        assert_eq!(
            file_notices(&killed.parent),
            expected,
            "{mode}: each run settles once"
        );
    }
}
