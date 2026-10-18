//! Tool definitions, ported from the parts of Pi
//! `src/core/tools/tool-definition-wrapper.ts` that shape a tool (v1.1.0;
//! see the crate's `NOTICE`), and the shared execution helpers of the file
//! tools.
//!
//! Pi's `ToolDefinition` is an `AgentTool` plus what the system prompt says
//! about it and how the terminal renders it. Bake's [`ToolDefinition`] holds
//! the [`AgentTool`] and the prompt text; renderers are the terminal's.

use std::future::Future;

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::AbortSignal;
use serde_json::Value;

use crate::agent_session::ToolPrompt;
use crate::owned_work::{Refused, WorkGate, WorkPermit};

/// Checks a mutation's target before `write` or `edit` changes it: Bake's
/// seam for the sandbox's write containment ([`crate::permissions`]). It
/// runs inside the file's mutation queue, before the first effect, with the
/// tool's absolute target path. `Ok` is the [`MutationTarget`] every later
/// step of the call goes through, so the call mutates what was checked;
/// an `Err` is the tool's error text.
pub type MutationGuard = std::sync::Arc<
    dyn Fn(&std::path::Path) -> Result<Box<dyn MutationTarget>, String> + Send + Sync,
>;

/// Where `write` and `edit` perform a call's file operations.
pub trait MutationTarget: Send + Sync {
    /// Makes the file's missing parent directories (Pi's recursive `mkdir`
    /// of the file's directory).
    fn create_parent_dirs(&self) -> std::io::Result<()>;
    /// Opens the file for writing, created if missing and truncated.
    fn create(&self) -> std::io::Result<std::fs::File>;
    /// Opens the file for reading.
    fn open_read(&self) -> std::io::Result<std::fs::File>;
    /// The tool's error text for `error` when the target refuses it rather
    /// than reporting it as Node would; `None` otherwise.
    fn refusal(&self, _error: &std::io::Error) -> Option<String> {
        None
    }
}

/// Pi's own target: the path itself, links followed.
#[derive(Debug, Clone)]
pub struct PathTarget(pub std::path::PathBuf);

impl MutationTarget for PathTarget {
    fn create_parent_dirs(&self) -> std::io::Result<()> {
        match self.0.parent() {
            Some(dir) => std::fs::create_dir_all(dir),
            None => Ok(()),
        }
    }

    fn create(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::create(&self.0)
    }

    fn open_read(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::open(&self.0)
    }
}

/// A mutation's target: `guard`'s for `path`, else [`PathTarget`].
pub(crate) fn mutation_target(
    guard: Option<&MutationGuard>,
    path: &std::path::Path,
) -> Result<Box<dyn MutationTarget>, String> {
    match guard {
        Some(guard) => guard(path),
        None => Ok(Box::new(PathTarget(path.to_path_buf()))),
    }
}

/// The text of a failed step on `target`: its refusal, else Node's message
/// for `syscall` on `path`.
pub(crate) fn target_error(
    target: &dyn MutationTarget,
    error: &std::io::Error,
    syscall: &str,
    path: &std::path::Path,
) -> String {
    target
        .refusal(error)
        .unwrap_or_else(|| crate::tools::node_error::message(error, syscall, Some(path)))
}

/// The message every tool rejects with when aborted.
pub(crate) const ABORTED: &str = "Operation aborted";

/// Pi's `ToolDefinition` without renderers.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /// The executable tool.
    pub tool: AgentTool,
    /// Pi's `promptSnippet`: the tool's line under the prompt's tools.
    pub prompt_snippet: Option<String>,
    /// Pi's `promptGuidelines`: bullets for the prompt's guidelines.
    pub prompt_guidelines: Option<Vec<String>>,
}

impl ToolDefinition {
    /// The tool's name.
    pub fn name(&self) -> &str {
        &self.tool.name
    }

    /// What the session's prompt says about the tool.
    pub fn prompt(&self) -> ToolPrompt {
        ToolPrompt {
            name: self.tool.name.clone(),
            snippet: self.prompt_snippet.clone(),
            guidelines: self.prompt_guidelines.clone().unwrap_or_default(),
        }
    }
}

/// Pi's `wrapToolDefinition`: the executable tool of a definition.
pub fn wrap_tool_definition(definition: &ToolDefinition) -> AgentTool {
    definition.tool.clone()
}

/// Pi's `wrapToolDefinitions`.
pub fn wrap_tool_definitions(definitions: &[ToolDefinition]) -> Vec<AgentTool> {
    definitions.iter().map(wrap_tool_definition).collect()
}

/// Pi's `createToolDefinitionFromAgentTool`: a definition without prompt
/// text.
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    ToolDefinition {
        tool,
        prompt_snippet: None,
        prompt_guidelines: None,
    }
}

/// A result holding one text block and optional details.
pub(crate) fn text_result(text: impl Into<String>, details: Option<Value>) -> AgentToolResult {
    AgentToolResult {
        details,
        ..AgentToolResult::text(text)
    }
}

/// A permit of `owner`, if given; refused once it is closing.
pub(crate) fn admit(owner: Option<&WorkGate>) -> Result<Option<WorkPermit>, Refused> {
    owner
        .map(|owner| owner.enter().ok_or(Refused::Closed))
        .transpose()
}

/// Runs `work` on Tokio's blocking pool and waits for it; under a share of
/// `permit`, when given, so the permit's owner waits for the work even if
/// this future is dropped. The error is the failed task's text.
pub(crate) async fn blocking_under<T: Send + 'static>(
    permit: Option<WorkPermit>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    match permit {
        Some(permit) => permit
            .spawn_blocking(work)
            .map_err(|error| error.to_string())?
            .await
            .map_err(|error| error.to_string()),
        None => tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| error.to_string()),
    }
}

/// Runs blocking work on Tokio's blocking pool and waits for it, even when
/// the caller's run is aborted: the work checks `signal` itself, as Pi's
/// write and edit tools do inside their mutation queue. The owner retains
/// the worker even if hard shutdown drops this future; a closing owner
/// refuses the work before it starts.
pub(crate) async fn run_blocking<T: Send + 'static>(
    owner: Option<WorkGate>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let permit = admit(owner.as_ref()).map_err(|refused| refused.to_string())?;
    tokio::task::spawn_blocking(move || {
        let result = work();
        // The work and its captures have dropped before shutdown can finish.
        drop(permit);
        result
    })
    .await
    .unwrap_or_else(|error| Err(format!("Tool task failed: {error}")))
}

/// Runs blocking work that stops at an abort, as Pi's tools that reject
/// from their abort listener do: on abort the result is [`ABORTED`], but
/// only after the work, which polls the signal, has returned.
/// The owner retains the worker even if hard shutdown drops this future.
pub(crate) async fn run_blocking_abortable<T: Send + 'static>(
    signal: Option<AbortSignal>,
    owner: Option<WorkGate>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(ABORTED.into());
    }
    let permit = admit(owner.as_ref()).map_err(|refused| refused.to_string())?;
    let mut task = tokio::task::spawn_blocking(move || {
        let result = work();
        // The work and its captures have dropped before shutdown can finish.
        drop(permit);
        result
    });
    let joined = match &signal {
        Some(signal) => {
            tokio::select! {
                joined = &mut task => joined,
                () = signal.cancelled() => {
                    let _ = task.await;
                    return Err(ABORTED.into());
                }
            }
        }
        None => task.await,
    };
    match joined {
        Ok(_) if signal.as_ref().is_some_and(AbortSignal::aborted) => Err(ABORTED.into()),
        Ok(result) => result,
        Err(error) => Err(format!("Tool task failed: {error}")),
    }
}

/// Whether `signal` has aborted.
pub(crate) fn aborted(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::aborted)
}

/// Boxes a tool's future.
pub(crate) fn boxed<F>(future: F) -> bake_agent::BoxFuture<'static, Result<AgentToolResult, String>>
where
    F: Future<Output = Result<AgentToolResult, String>> + Send + 'static,
{
    Box::pin(future)
}

/// A string argument, `""` when absent; validation has checked types.
pub(crate) fn string_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// An optional string argument.
pub(crate) fn optional_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// An optional number argument.
pub(crate) fn number_arg(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

/// An optional boolean argument, JavaScript truthiness for other values.
pub(crate) fn bool_arg(args: &Value, key: &str) -> bool {
    match args.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::io::Write;
    use std::task::Poll;
    use std::time::Duration;

    type Work = Box<dyn FnOnce() + Send>;

    /// Starts `start`'s waiter on blocking work that holds a file open
    /// until released, aborts the waiter, and checks that the gate's close
    /// waits for the work, whose write is complete once the close returns.
    async fn dropped_waiter_keeps_work_owned<Fut>(
        name: &str,
        start: impl FnOnce(WorkGate, Work) -> Fut,
    ) where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let dir = crate::test_support::TempDir::new(name);
        let path = dir.join("worker");
        let output = path.clone();
        let gate = WorkGate::default();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let waiter = tokio::spawn(start(
            gate.clone(),
            Box::new(move || {
                let mut file = std::fs::File::create(output).unwrap();
                entered.send(()).unwrap();
                let _ = held.recv();
                file.write_all(b"settled").unwrap();
                file.sync_all().unwrap();
            }),
        ));
        tokio::time::timeout(Duration::from_secs(10), ready)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        let _ = waiter.await;
        let mut drained = Box::pin(gate.close());
        let waiting =
            std::future::poll_fn(|cx| Poll::Ready(drained.as_mut().poll(cx).is_pending())).await;
        drop(release);
        if waiting {
            tokio::time::timeout(Duration::from_secs(10), drained)
                .await
                .unwrap();
        }
        assert!(
            waiting,
            "{name}: dropping a waiter released a still-running blocking worker"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"settled", "{name}");
    }

    #[tokio::test]
    async fn dropped_blocking_waiter_keeps_work_owned_through_file_cleanup() {
        dropped_waiter_keeps_work_owned("read-worker-cleanup", |gate, work| async move {
            let _ = run_blocking_abortable(None, Some(gate), move || {
                work();
                Ok(())
            })
            .await;
        })
        .await;
    }

    // The write and edit tools' worker.
    #[tokio::test]
    async fn dropped_mutation_waiter_keeps_work_owned_through_file_cleanup() {
        dropped_waiter_keeps_work_owned("mutation-worker-cleanup", |gate, work| async move {
            let _ = run_blocking(Some(gate), move || {
                work();
                Ok(())
            })
            .await;
        })
        .await;
    }

    // The grep and find tools' worker, under a share of the call's permit.
    #[tokio::test]
    async fn dropped_shared_permit_waiter_keeps_work_owned_through_file_cleanup() {
        dropped_waiter_keeps_work_owned("shared-worker-cleanup", |gate, work| {
            let call = admit(Some(&gate)).unwrap().unwrap();
            let line = blocking_under(Some(call.share()), work);
            async move {
                let _call = call;
                let _ = line.await;
            }
        })
        .await;
    }

    #[test]
    fn admit_refuses_a_closing_owner() {
        assert!(matches!(admit(None), Ok(None)));
        let owner = WorkGate::default();
        let permit = admit(Some(&owner)).unwrap();
        assert!(permit.is_some());
        drop(owner.close());
        assert_eq!(admit(Some(&owner)).err(), Some(Refused::Closed));
        drop(permit);
        assert!(owner.is_closed());
    }

    /// `write` and `edit` from [`crate::tools::index::create_tool`] and in
    /// their sandboxed forms, all under `owner`.
    fn owned_mutation_tools(
        cwd: &std::path::Path,
        owner: &WorkGate,
    ) -> Vec<(String, bake_agent::AgentTool)> {
        use crate::permissions::{
            ApprovalPolicy, NoApprover, StandingPermissions, ToolPermissions,
            owned_sandbox_file_tools,
        };
        use crate::tools::index::{ToolsOptions, create_tool_definition};
        let options = ToolsOptions {
            owner: Some(owner.clone()),
            ..Default::default()
        };
        let permissions = ToolPermissions::new(
            std::sync::Arc::new(StandingPermissions::new(
                bake_sandbox::SandboxMode::WorkspaceWrite,
                ApprovalPolicy::Never,
                "workspace-write",
                cwd,
                None,
            )),
            std::sync::Arc::new(NoApprover),
            std::sync::Arc::new(bake_sandbox::UnavailableProvider::new(None)),
        );
        let native: Vec<ToolDefinition> = ["write", "edit"]
            .iter()
            .map(|name| create_tool_definition(name, cwd, &options).unwrap())
            .collect();
        let sandboxed =
            owned_sandbox_file_tools(native.clone(), cwd, &permissions, Some(owner.clone()));
        native
            .into_iter()
            .map(|definition| (definition.name().to_owned(), definition.tool))
            .chain(
                sandboxed.into_iter().map(|definition| {
                    (format!("sandboxed {}", definition.name()), definition.tool)
                }),
            )
            .collect()
    }

    #[tokio::test]
    async fn owned_write_and_edit_refuse_a_closed_owner_without_an_effect() {
        let dir = crate::test_support::TempDir::new("mutation-closed-owner");
        std::fs::write(dir.join("present"), "alpha\n").unwrap();
        let owner = WorkGate::default();
        let tools = owned_mutation_tools(dir.path(), &owner);
        owner.close().await;
        for (label, tool) in tools {
            let args = if tool.name == "write" {
                serde_json::json!({"path": "present", "content": "replaced\n"})
            } else {
                serde_json::json!({"path": "present", "edits": [{"oldText": "alpha", "newText": "beta"}]})
            };
            let result = (tool.execute)(
                "call".into(),
                args,
                None,
                bake_agent::AgentToolUpdateCallback::ignore(),
            )
            .await;
            assert_eq!(result.unwrap_err(), "the session is shut down", "{label}");
            if tool.name == "write" {
                let created = (tool.execute)(
                    "call".into(),
                    serde_json::json!({"path": "sub/new", "content": "new\n"}),
                    None,
                    bake_agent::AgentToolUpdateCallback::ignore(),
                )
                .await;
                assert_eq!(created.unwrap_err(), "the session is shut down", "{label}");
            }
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("present")).unwrap(),
            "alpha\n"
        );
        assert!(!dir.join("sub").exists());
    }

    // The text is the one the refused `rg` or `fd` spawn gives
    // ([`crate::child_process`]); the blocking work's admission now comes
    // first.
    #[cfg(unix)]
    #[tokio::test]
    async fn grep_and_find_refuse_a_closed_owner() {
        use crate::tools::index::{ToolsOptions, create_tool};
        use crate::tools::process::find_executable;
        let dir = crate::test_support::TempDir::new("search-closed-owner");
        std::fs::write(dir.join("present"), "needle\n").unwrap();
        let owner = WorkGate::default();
        owner.close().await;
        for (name, program, refused) in [
            (
                "grep",
                &["rg"][..],
                "Failed to run ripgrep: the session is shut down",
            ),
            (
                "find",
                &["fd", "fdfind"][..],
                "Failed to run fd: the session is shut down",
            ),
        ] {
            if find_executable(program).is_none() {
                println!("skipping {name}: {program:?} is not on PATH");
                continue;
            }
            let tool = create_tool(
                name,
                dir.path(),
                &ToolsOptions {
                    owner: Some(owner.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let result = (tool.execute)(
                "call".into(),
                serde_json::json!({"pattern": "needle"}),
                None,
                bake_agent::AgentToolUpdateCallback::ignore(),
            )
            .await;
            assert_eq!(result.unwrap_err(), refused, "{name}");
        }
    }

    #[tokio::test]
    async fn native_read_and_ls_refuse_a_closed_owner() {
        use crate::tools::index::{ToolsOptions, create_tool};
        let dir = crate::test_support::TempDir::new("read-closed-owner");
        std::fs::write(dir.join("present"), "hello").unwrap();
        let owner = crate::owned_work::WorkGate::default();
        owner.close().await;
        for name in ["read", "ls"] {
            let tool = create_tool(
                name,
                dir.path(),
                &ToolsOptions {
                    owner: Some(owner.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let args = if name == "read" {
                serde_json::json!({"path":"present"})
            } else {
                serde_json::json!({})
            };
            let result = (tool.execute)(
                "call".into(),
                args,
                None,
                bake_agent::AgentToolUpdateCallback::ignore(),
            )
            .await;
            assert_eq!(result.unwrap_err(), "the session is shut down", "{name}");
        }
    }
}
