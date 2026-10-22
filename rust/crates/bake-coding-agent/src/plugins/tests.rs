//! The dispatcher's order, composition, and failure rules, after Pi v1.3.0
//! `ExtensionRunner.emitToolCall` (handlers in order, first `block` wins,
//! a non-blocking result continues) and `AgentSession._beforeToolCall` (a
//! failing handler blocks), plus Bake's sticky failure, deadline, and
//! cancellation; and the `protected-paths` port of Pi's example.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bake_agent::BoxFuture;
use bake_ai::AbortController;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::veto::{Dispatch, FailureKind, Outcome, VetoDispatcher};
use super::*;

/// How long a test waits for a barrier before it reports it missing: half
/// the default plugin deadline, so a missing barrier is reported as itself
/// rather than as that deadline failing the plugin, and still seconds for a
/// wake on the same task.
const WAIT: Duration = Duration::from_secs(DEFAULT_CALL_TIMEOUT.as_secs() / 2);

/// What a [`Scripted`] plugin answers, from the call.
pub(crate) type Answer =
    Arc<dyn Fn(&str, &Value) -> BoxFuture<'static, Result<ToolCallVeto, String>> + Send + Sync>;

/// A plugin that records each call's tool and id and answers with a script.
pub(crate) struct Scripted {
    pub manifest: PluginManifest,
    pub calls: Arc<Mutex<Vec<String>>>,
    pub answer: Answer,
}

impl Scripted {
    pub(crate) fn new(id: &str, tools: &[&str], answer: Answer) -> Self {
        Self {
            manifest: PluginManifest::new(id, "1.0.0", tools.iter().copied()).expect("manifest"),
            calls: Arc::default(),
            answer,
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Plugin for Scripted {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    fn on_tool_call<'a>(
        &'a self,
        call: ToolCallView<'a>,
    ) -> BoxFuture<'a, Result<ToolCallVeto, String>> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("{}:{}", call.tool_name, call.tool_call_id));
        (self.answer)(call.tool_name, call.args)
    }
}

pub(crate) fn answer<F>(f: F) -> Answer
where
    F: Fn(&str, &Value) -> BoxFuture<'static, Result<ToolCallVeto, String>> + Send + Sync + 'static,
{
    Arc::new(f)
}

pub(crate) fn allow() -> Answer {
    answer(|_, _| Box::pin(async { Ok(ToolCallVeto::Continue) }))
}

pub(crate) fn block(reason: &'static str) -> Answer {
    answer(move |_, _| {
        Box::pin(async move {
            Ok(ToolCallVeto::Block {
                reason: reason.to_owned(),
                terminate: false,
            })
        })
    })
}

pub(crate) fn fail(message: &'static str) -> Answer {
    answer(move |_, _| Box::pin(async move { Err(message.to_owned()) }))
}

/// Sets its flag when dropped: proves a plugin's future was dropped.
pub(crate) struct DropFlag(pub Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signals `entered`, then never answers; `dropped` is set once the
/// dispatcher drops its future.
pub(crate) fn hang(entered: Arc<Notify>, dropped: Arc<std::sync::atomic::AtomicBool>) -> Answer {
    answer(move |_, _| {
        let entered = Arc::clone(&entered);
        let flag = DropFlag(Arc::clone(&dropped));
        Box::pin(async move {
            let _flag = flag;
            entered.notify_one();
            std::future::pending::<()>().await;
            Ok(ToolCallVeto::Continue)
        })
    })
}

fn view<'a>(tool: &'a str, id: &'a str, args: &'a Value) -> ToolCallView<'a> {
    ToolCallView {
        tool_call_id: id,
        tool_name: tool,
        args,
    }
}

fn dispatcher(plugins: Vec<(Arc<Scripted>, i32)>) -> VetoDispatcher {
    let mut registry = PluginRegistry::new();
    for (plugin, order) in plugins {
        registry.register(plugin, order).expect("register");
    }
    VetoDispatcher::new(registry).expect("non-empty")
}

#[test]
fn manifests_are_validated() {
    assert!(PluginManifest::new("guard-1", "0.1.0", ["write"]).is_ok());
    for id in [
        "",
        "Guard",
        "guard_1",
        "a".repeat(33).as_str(),
        "gu ard",
        "é",
    ] {
        assert!(
            matches!(
                PluginManifest::new(id, "0.1.0", ["write"]),
                Err(PluginError::InvalidId(_))
            ),
            "{id:?}"
        );
    }
    for version in ["", "1 0", "1.0\n", "v".repeat(65).as_str()] {
        assert!(matches!(
            PluginManifest::new("g", version, ["write"]),
            Err(PluginError::InvalidVersion(_))
        ));
    }
    for tool in ["", "a b", "x/y", "t".repeat(65).as_str()] {
        assert!(matches!(
            PluginManifest::new("g", "1", [tool]),
            Err(PluginError::InvalidToolName(_))
        ));
    }
    assert_eq!(
        PluginManifest::new("g", "1", Vec::<String>::new()),
        Err(PluginError::NoContributions)
    );
    let many: Vec<String> = (0..33).map(|i| format!("t{i}")).collect();
    assert_eq!(
        PluginManifest::new("g", "1", many),
        Err(PluginError::TooManyTools)
    );
    // Duplicates collapse.
    let manifest = PluginManifest::new("g", "1", ["write", "edit", "write"]).expect("ok");
    assert_eq!(manifest.veto_tools().collect::<Vec<_>>(), ["edit", "write"]);
}

#[test]
fn a_duplicate_id_is_refused_and_an_empty_registry_dispatches_nothing() {
    let mut registry = PluginRegistry::new();
    assert!(registry.is_empty());
    assert!(VetoDispatcher::new(registry.clone()).is_none());
    registry
        .register(Arc::new(Scripted::new("a", &["write"], allow())), 0)
        .expect("first");
    assert_eq!(
        registry.register(Arc::new(Scripted::new("a", &["edit"], allow())), 1),
        Err(PluginError::Duplicate(PluginId::new("a").expect("id")))
    );
}

// runner.ts `emitToolCall`: handlers run in order; a non-blocking result
// continues and the first block returns. Bake orders by configured order,
// then id, whatever the registration order.
#[tokio::test]
async fn the_first_block_in_order_wins() {
    let late = Arc::new(Scripted::new("a-late", &["write"], block("late")));
    let second = Arc::new(Scripted::new("b-second", &["write"], block("second")));
    let first = Arc::new(Scripted::new("z-first", &["write"], allow()));
    let tie = Arc::new(Scripted::new("a-tie", &["write"], allow()));
    let plugins = dispatcher(vec![
        (Arc::clone(&late), 5),
        (Arc::clone(&second), 1),
        (Arc::clone(&first), 0),
        (Arc::clone(&tie), 1),
    ]);
    let args = json!({ "path": "x" });
    let decision = plugins
        .dispatch(view("write", "call-1", &args), None)
        .await
        .decision()
        .expect("blocked");
    assert_eq!(decision.plugin.as_str(), "b-second");
    assert_eq!(decision.denial_text(), "second");
    assert_eq!(first.calls(), ["write:call-1"]);
    assert_eq!(tie.calls(), ["write:call-1"], "a-tie runs before b-second");
    assert_eq!(second.calls(), ["write:call-1"]);
    assert!(
        late.calls().is_empty(),
        "nothing runs after the first block"
    );
}

#[tokio::test]
async fn unlisted_tools_reach_no_plugin() {
    let guard = Arc::new(Scripted::new("g", &["write"], block("no")));
    let plugins = dispatcher(vec![(Arc::clone(&guard), 0)]);
    let args = json!({});
    assert!(
        plugins
            .dispatch(view("read", "c", &args), None)
            .await
            .decision()
            .is_none()
    );
    assert!(guard.calls().is_empty());
}

// agent-session.ts `_beforeToolCall`: a failing handler blocks. Bake keeps
// the plugin failed, so its listed tools stay blocked without asking it,
// while its unlisted tools and other plugins' tools are unaffected.
#[tokio::test]
async fn an_error_blocks_and_the_plugin_stays_failed() {
    let failing = Arc::new(Scripted::new("broken", &["write"], fail("boom")));
    let other = Arc::new(Scripted::new("other", &["edit"], allow()));
    let plugins = dispatcher(vec![(Arc::clone(&failing), 0), (Arc::clone(&other), 0)]);
    let args = json!({});
    let first = plugins
        .dispatch(view("write", "1", &args), None)
        .await
        .decision()
        .expect("blocked");
    assert_eq!(
        first.outcome,
        Outcome::Failed {
            failure: veto::Failure {
                kind: FailureKind::Error,
                message: "boom".into()
            },
            earlier: false
        }
    );
    assert_eq!(
        first.denial_text(),
        "Plugin \"broken\" failed, blocking execution: boom"
    );
    let second = plugins
        .dispatch(view("write", "2", &args), None)
        .await
        .decision()
        .expect("still blocked");
    assert!(matches!(
        second.outcome,
        Outcome::Failed { earlier: true, .. }
    ));
    assert_eq!(
        second.denial_text(),
        "Plugin \"broken\" failed earlier, blocking execution: boom"
    );
    assert_eq!(failing.calls(), ["write:1"], "a failed plugin is not asked");
    assert!(
        plugins
            .dispatch(view("edit", "3", &args), None)
            .await
            .decision()
            .is_none()
    );
    assert!(
        plugins
            .dispatch(view("read", "4", &args), None)
            .await
            .decision()
            .is_none()
    );
}

#[tokio::test]
async fn a_panic_fails_the_plugin() {
    let in_call = Arc::new(Scripted::new(
        "panics-calling",
        &["write"],
        answer(|_, _| panic!("in the call")),
    ));
    let in_future = Arc::new(Scripted::new(
        "panics-polled",
        &["edit"],
        answer(|_, _| Box::pin(async { panic!("in the future") })),
    ));
    let plugins = dispatcher(vec![(in_call, 0), (in_future, 0)]);
    let args = json!({});
    for (tool, message) in [
        ("write", "panicked: in the call"),
        ("edit", "panicked: in the future"),
    ] {
        let decision = plugins
            .dispatch(view(tool, "1", &args), None)
            .await
            .decision()
            .expect("blocked");
        let Outcome::Failed { failure, .. } = decision.outcome else {
            panic!("failed expected");
        };
        assert_eq!(failure.kind, FailureKind::Panic);
        assert_eq!(failure.message, message);
    }
}

#[tokio::test(start_paused = true)]
async fn a_missed_deadline_fails_the_plugin_and_drops_its_future() {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slow = Arc::new(Scripted::new(
        "slow",
        &["write"],
        hang(Arc::clone(&entered), Arc::clone(&dropped)),
    ));
    let mut registry = PluginRegistry::new().with_call_timeout(Duration::from_millis(50));
    registry
        .register(Arc::clone(&slow) as _, 0)
        .expect("register");
    let plugins = VetoDispatcher::new(registry).expect("dispatcher");
    let args = json!({});
    let decision = plugins
        .dispatch(view("write", "1", &args), None)
        .await
        .decision()
        .expect("blocked");
    let Outcome::Failed { failure, earlier } = decision.outcome else {
        panic!("failed expected");
    };
    assert_eq!(failure.kind, FailureKind::Timeout);
    assert_eq!(failure.message, "no answer within 50 ms");
    assert!(!earlier);
    assert!(dropped.load(Ordering::SeqCst), "the future was dropped");
    assert!(
        plugins
            .dispatch(view("write", "2", &args), None)
            .await
            .decision()
            .is_some()
    );
    assert_eq!(slow.calls().len(), 1);
}

// Cancellation itself fails no plugin: a check that is running when the
// call is cancelled is dropped, nothing is decided, and the next call asks
// the plugin again.
#[tokio::test]
async fn cancelling_a_running_check_drops_it_and_fails_nothing() {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hanging = hang(Arc::clone(&entered), Arc::clone(&dropped));
    // Hangs on its first call; blocks every later one.
    let slow = Arc::new(Scripted::new(
        "slow",
        &["write"],
        answer(move |tool, args| {
            if args["path"] == "first" {
                hanging(tool, args)
            } else {
                Box::pin(async {
                    Ok(ToolCallVeto::Block {
                        reason: "asked again".into(),
                        terminate: false,
                    })
                })
            }
        }),
    ));
    let plugins = dispatcher(vec![(Arc::clone(&slow), 0)]);
    let controller = AbortController::new();
    let signal = controller.signal();
    let first = json!({ "path": "first" });
    let dispatching = plugins.dispatch(view("write", "1", &first), Some(&signal));
    let cancel = async {
        entered.notified().await;
        controller.abort();
    };
    // Bounded: both futures are local, so a timeout drops all the work.
    let (dispatched, ()) = tokio::time::timeout(WAIT, async { tokio::join!(dispatching, cancel) })
        .await
        .expect("timed out waiting for the check to start");
    assert_eq!(dispatched, Dispatch::Cancelled);
    assert!(dropped.load(Ordering::SeqCst), "the check was dropped");
    let second = json!({ "path": "second" });
    let decision = plugins
        .dispatch(view("write", "2", &second), None)
        .await
        .decision()
        .expect("asked again");
    assert_eq!(
        decision.outcome,
        Outcome::Blocked {
            reason: "asked again".into()
        }
    );
    assert_eq!(slow.calls(), ["write:1", "write:2"]);
}

// An already-aborted call asks nobody, and is cancelled before an earlier
// failure is reported, so no decision is recorded for it. The cancellation
// does not clear that failure: the next call is still blocked by it.
#[tokio::test]
async fn an_aborted_call_is_cancelled_before_any_plugin_or_failure() {
    let broken = Arc::new(Scripted::new("broken", &["write"], fail("boom")));
    let fresh = Arc::new(Scripted::new("fresh", &["edit"], allow()));
    let plugins = dispatcher(vec![(Arc::clone(&broken), 0), (Arc::clone(&fresh), 0)]);
    let args = json!({});
    assert!(
        plugins
            .dispatch(view("write", "1", &args), None)
            .await
            .decision()
            .is_some()
    );
    let controller = AbortController::new();
    controller.abort();
    let signal = controller.signal();
    for (tool, id) in [("write", "2"), ("edit", "3")] {
        assert_eq!(
            plugins.dispatch(view(tool, id, &args), Some(&signal)).await,
            Dispatch::Cancelled,
            "{tool}"
        );
    }
    assert!(fresh.calls().is_empty());
    let after = plugins
        .dispatch(view("write", "4", &args), None)
        .await
        .decision()
        .expect("still blocked");
    assert!(matches!(
        after.outcome,
        Outcome::Failed { earlier: true, .. }
    ));
    assert_eq!(broken.calls(), ["write:1"]);
}

// Two calls checked at once by the same plugin, as owned futures on one
// task: the second fails the plugin while the first is still running, so
// the first's later `Continue` no longer counts.
#[tokio::test]
async fn a_failure_during_a_concurrent_check_overrides_its_continue() {
    let release = Arc::new(Notify::new());
    let racing = Arc::new(Scripted::new(
        "racing",
        &["write"],
        answer({
            let release = Arc::clone(&release);
            move |_, args| {
                let release = Arc::clone(&release);
                let waits = args["path"] == "waits";
                Box::pin(async move {
                    if waits {
                        release.notified().await;
                        Ok(ToolCallVeto::Continue)
                    } else {
                        // Fails the plugin, then lets the first check answer.
                        release.notify_one();
                        Err("boom".to_owned())
                    }
                })
            }
        }),
    ));
    let plugins = dispatcher(vec![(Arc::clone(&racing), 0)]);
    let waits = json!({ "path": "waits" });
    let fails = json!({ "path": "fails" });
    // Bounded: both checks are local futures, so a timeout drops all the
    // work.
    let (first, second) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            plugins.dispatch(view("write", "1", &waits), None),
            plugins.dispatch(view("write", "2", &fails), None),
        )
    })
    .await
    .expect("timed out waiting for the concurrent checks to answer");
    assert!(matches!(
        second.decision().expect("blocked").outcome,
        Outcome::Failed { earlier: false, .. }
    ));
    let first = first.decision().expect("the continue no longer counts");
    assert!(matches!(
        &first.outcome,
        Outcome::Failed { failure, earlier: true } if failure.kind == FailureKind::Error
    ));
    assert_eq!(racing.calls(), ["write:1", "write:2"]);
}

#[tokio::test]
async fn an_oversized_reason_fails_the_plugin_and_an_empty_one_is_pis_default() {
    let huge = Arc::new(Scripted::new(
        "huge",
        &["write"],
        answer(|_, _| {
            Box::pin(async {
                Ok(ToolCallVeto::Block {
                    reason: "x".repeat(MAX_REASON_BYTES + 1),
                    terminate: true,
                })
            })
        }),
    ));
    let quiet = Arc::new(Scripted::new(
        "quiet",
        &["edit"],
        answer(|_, _| {
            Box::pin(async {
                Ok(ToolCallVeto::Block {
                    reason: String::new(),
                    terminate: true,
                })
            })
        }),
    ));
    let plugins = dispatcher(vec![(huge, 0), (quiet, 0)]);
    let args = json!({});
    let oversized = plugins
        .dispatch(view("write", "1", &args), None)
        .await
        .decision()
        .expect("blocked");
    assert!(matches!(
        &oversized.outcome,
        Outcome::Failed { failure, .. } if failure.kind == FailureKind::InvalidReturn
    ));
    assert!(
        !oversized.terminate,
        "an invalid return's terminate is ignored"
    );
    let empty = plugins
        .dispatch(view("edit", "2", &args), None)
        .await
        .decision()
        .expect("blocked");
    assert_eq!(empty.denial_text(), "Tool execution was blocked");
    assert!(empty.terminate);
}

#[tokio::test]
async fn decision_entries_name_the_plugin_call_and_failure() {
    let plugins = dispatcher(vec![(
        Arc::new(Scripted::new("broken", &["write"], fail("boom"))),
        0,
    )]);
    let args = json!({});
    let decision = plugins
        .dispatch(view("write", "call-7", &args), None)
        .await
        .decision()
        .expect("blocked");
    assert_eq!(
        decision.entry_data(),
        json!({
            "plugin": "broken",
            "version": "1.0.0",
            "toolCallId": "call-7",
            "toolName": "write",
            "decision": "failed",
            "reason": "Plugin \"broken\" failed, blocking execution: boom",
            "terminate": false,
            "failure": { "kind": "error", "message": "boom", "earlier": false },
        })
    );
}

// Pi `examples/extensions/protected-paths.ts`: `write` and `edit` to a path
// containing `.env`, `.git/`, or `node_modules/` are blocked with
// `Path "<path>" is protected`; anything else continues.
#[tokio::test]
async fn protected_paths_follows_pis_example() {
    let guard = ProtectedPaths::pi_default();
    let manifest = guard.manifest();
    assert_eq!(manifest.id().as_str(), "protected-paths");
    assert_eq!(manifest.veto_tools().collect::<Vec<_>>(), ["edit", "write"]);
    for (tool, path, blocked) in [
        ("write", ".env", true),
        ("edit", "app/.env.local", true),
        ("write", "repo/.git/config", true),
        ("edit", "web/node_modules/x/index.js", true),
        ("write", "src/env.rs", false),
        ("edit", ".gitignore", false),
        ("read", ".env", false),
    ] {
        let args = json!({ "path": path });
        let answer = guard
            .on_tool_call(view(tool, "c", &args))
            .await
            .expect("answer");
        let expected = if blocked {
            ToolCallVeto::Block {
                reason: format!("Path \"{path}\" is protected"),
                terminate: false,
            }
        } else {
            ToolCallVeto::Continue
        };
        assert_eq!(answer, expected, "{tool} {path}");
    }
    let long = format!(".env{}", "é".repeat(2000));
    let args = json!({ "path": long });
    let ToolCallVeto::Block { reason, .. } = guard
        .on_tool_call(view("write", "c", &args))
        .await
        .expect("answer")
    else {
        panic!("blocked expected");
    };
    assert!(reason.len() <= MAX_REASON_BYTES, "a quoted path is bounded");
    assert!(
        guard
            .on_tool_call(view("write", "c", &json!({})))
            .await
            .is_err(),
        "a call without a path is refused"
    );
}
