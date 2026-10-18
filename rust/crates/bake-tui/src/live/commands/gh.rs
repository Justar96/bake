//! The GitHub CLI, which `/share` and `/bug` upload through, as Pi's
//! `session-share.ts` runs it: `gh auth status` first, then the upload.
//!
//! Each run is owned by the command's task: the child is killed when the
//! command's abort signal fires (Esc on its sheet) or the task is dropped
//! (shutdown), so no upload outlives the session. Its stdin is closed, so
//! `gh` never waits for an answer from the terminal Bake holds.

use std::ffi::{OsStr, OsString};
use std::process::Stdio;

use bake_ai::AbortSignal;
use bake_tui_view::copy;

/// How one run of `gh` ended.
#[derive(Debug)]
pub enum Ran {
    /// It exited, with this status code (`None` when a signal ended it),
    /// and printed this.
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// The command's abort signal fired; the child was killed.
    Cancelled,
}

/// Why `gh` could not run.
#[derive(Debug)]
pub enum RunError {
    /// No `gh` is on `PATH`.
    Missing,
    /// It could not be started or waited for.
    Failed(String),
}

impl RunError {
    /// What Pi says of a `gh` that cannot be run.
    pub fn message(&self) -> String {
        match self {
            Self::Missing => copy::SHARE_GH_MISSING.to_owned(),
            Self::Failed(error) => error.clone(),
        }
    }
}

/// Runs `program` with `args`, its output captured, until it exits or
/// `signal` fires.
pub async fn run(
    program: &OsStr,
    args: &[OsString],
    signal: &AbortSignal,
) -> Result<Ran, RunError> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RunError::Missing
            } else {
                RunError::Failed(error.to_string())
            }
        })?;
    if signal.aborted() {
        return Ok(Ran::Cancelled);
    }
    tokio::select! {
        () = signal.cancelled() => Ok(Ran::Cancelled),
        output = child.wait_with_output() => {
            let output = output.map_err(|error| RunError::Failed(error.to_string()))?;
            Ok(Ran::Exited {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }
}

/// Pi's check before an upload: `gh auth status` must succeed. `Ok(false)`
/// when the signal fired first.
pub async fn check_auth(program: &OsStr, signal: &AbortSignal) -> Result<bool, String> {
    match run(program, &["auth".into(), "status".into()], signal).await {
        Ok(Ran::Exited { code: Some(0), .. }) => Ok(true),
        Ok(Ran::Exited { .. }) => Err(copy::SHARE_GH_NOT_LOGGED_IN.to_owned()),
        Ok(Ran::Cancelled) => Ok(false),
        Err(error) => Err(error.message()),
    }
}

/// A private directory for the files an upload reads, removed with
/// everything in it when dropped, as Pi removes its `pi-share-` directory.
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    /// A new directory under the system's temporary directory, readable
    /// only by this user. Blocking.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}",
            bake_coding_agent::session::id::uuid_v7()
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&path)?;
        Ok(Self(path))
    }

    /// The directory.
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
