//! A process as a background job: the producer a background command
//! runs. Its output streams into the job's [`JobOutput`], and its outcome
//! follows Bake's TypeScript `packages/shell/tool-bash/src/background.ts`
//! (`processOutcome`, `processJob`): a killed process is `killed` with its
//! signal as detail, any other exit is `completed` with its exit code, a
//! nonzero one included, and a stop before the spawn is `killed`.
//!
//! The process is an owned tree under the job's gate
//! ([`JobContext::spawn_process`]) and is waited for as the coding tools
//! wait (`crate::child_process`): a stop of the job kills the tree and the
//! producer returns once it is gone; a command that exits by itself leaves
//! its own background descendants running, as Pi leaves a command's.

use std::process::ExitStatus;

use bake_process::ProcessSpec;

use super::{JobContext, JobOutcome};
use crate::child_process::wait_for_process;

/// Runs `spec` as `context`'s job and returns its outcome once the process
/// is settled. Standard output and standard error both go to the job's
/// output, if it streams. Standard input is not given: `spec.stdin` is
/// ignored and reads the null device.
pub async fn run_process(context: JobContext, spec: ProcessSpec) -> JobOutcome {
    let stopped = || JobOutcome::killed("killed before exit");
    if context.signal().aborted() {
        return stopped();
    }
    let spec = ProcessSpec {
        stdin: false,
        ..spec
    };
    let process = match context.spawn_process(&spec).await {
        Ok(process) => process,
        Err(_) if context.signal().aborted() => return stopped(),
        Err(error) => return JobOutcome::failed(format!("could not start the process: {error}")),
    };
    let output = context.output().cloned();
    let append = |bytes: &[u8]| {
        if let Some(output) = &output {
            output.append(bytes);
        }
    };
    match wait_for_process(process, None, Some(context.signal()), None, append, append).await {
        Err(error) => JobOutcome::failed(format!("the process could not be awaited: {error}")),
        Ok(exit) if exit.killed => JobOutcome::killed(killed_detail(exit.status)),
        Ok(exit) => JobOutcome::completed(format!(
            "exit code: {}",
            exit.status.and_then(|status| status.code()).unwrap_or(0)
        )),
    }
}

/// `signal: <name>` for a Unix signal exit, as the TypeScript renders
/// Node's signal name; `killed before exit` otherwise.
fn killed_detail(status: Option<ExitStatus>) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.and_then(|status| status.signal()) {
            let name = match signal {
                libc::SIGKILL => "SIGKILL".to_owned(),
                libc::SIGTERM => "SIGTERM".to_owned(),
                libc::SIGINT => "SIGINT".to_owned(),
                libc::SIGHUP => "SIGHUP".to_owned(),
                other => other.to_string(),
            };
            return format!("signal: {name}");
        }
    }
    #[cfg(not(unix))]
    let _ = status;
    "killed before exit".to_owned()
}
