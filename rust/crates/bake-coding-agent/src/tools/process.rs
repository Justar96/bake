//! Running `rg` and `fd` for the grep and find tools.
//!
//! Pi's `ensureTool` uses `rg` and `fd` (or `fdfind`) from `PATH` and
//! downloads them when missing. Bake only looks them up on `PATH`; a
//! missing binary fails the call with Pi's message. The child's standard
//! output is read line by line, lines longer than [`MAX_LINE_BYTES`] are
//! skipped, and at most [`MAX_STDERR_BYTES`] of standard error are kept. An
//! abort or the caller's stop kills the child, and the run returns only
//! after the child has exited and its pipes have closed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use bake_ai::AbortSignal;
use tokio::io::AsyncReadExt;

/// The longest output line passed on.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// How much standard error is kept.
pub const MAX_STDERR_BYTES: usize = 64 * 1024;

/// The first of `names` found as an executable file on `PATH`, searching
/// all of `PATH` for each name in turn, as Pi's `getToolPath` tries them.
pub(crate) fn find_executable(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    names
        .iter()
        .find_map(|name| dirs.iter().find_map(|dir| executable_in(dir, name)))
}

#[cfg(unix)]
fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidate = dir.join(name);
    let metadata = std::fs::metadata(&candidate).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
}

#[cfg(not(unix))]
fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let extensions: Vec<OsString> = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(OsString::from)
                .collect()
        })
        .unwrap_or_else(|| vec![".EXE".into(), ".CMD".into(), ".BAT".into(), ".COM".into()]);
    std::iter::once(OsString::new())
        .chain(extensions)
        .map(|ext| {
            let mut file = OsString::from(name);
            file.push(ext);
            dir.join(file)
        })
        .find(|candidate| candidate.is_file())
}

/// What the caller does after a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Keep reading.
    Continue,
    /// Kill the child; later lines are not passed on.
    Stop,
}

/// How a run ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The child exited; `code` is `None` when a signal ended it.
    Exited {
        code: Option<i32>,
        stderr: String,
        stopped: bool,
    },
    /// The signal aborted the run; the child was killed and reaped.
    Aborted,
}

/// Feeds the lines of the child's output to `on_line` as Node's `readline`
/// splits them (on `\r\n`, `\n`, and `\r`, a last unterminated line
/// included) until the output closes, then reaps the child. An abort, or
/// [`Flow::Stop`], kills the child and reaps it without reading further. `on_line` is async so it may do blocking
/// work on Tokio's pool while the child waits.
pub(crate) async fn run_lines<F, Fut>(
    program: &Path,
    args: &[OsString],
    signal: Option<&AbortSignal>,
    mut on_line: F,
) -> std::io::Result<Ended>
where
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Flow>,
{
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out_chunk = vec![0u8; 16 * 1024];
    let mut err_chunk = vec![0u8; 4 * 1024];
    let mut line: Vec<u8> = Vec::new();
    let mut overflow = false;
    let mut after_cr = false;
    let mut stderr_bytes: Vec<u8> = Vec::new();
    let mut aborted = false;
    let mut stopped = false;
    let cancelled = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(cancelled);

    // After a kill the pipes are dropped rather than drained: a process the
    // child started could hold them open.
    while (stdout.is_some() || stderr.is_some()) && !aborted && !stopped {
        tokio::select! {
            () = &mut cancelled, if !aborted => {
                aborted = true;
                let _ = child.start_kill();
            }
            read = async {
                match stdout.as_mut() {
                    Some(out) => out.read(&mut out_chunk).await,
                    None => std::future::pending().await,
                }
            }, if stdout.is_some() => {
                let count = read.unwrap_or(0);
                if count == 0 {
                    stdout = None;
                    if !line.is_empty() && !overflow && !stopped && !aborted
                        && on_line(std::mem::take(&mut line)).await == Flow::Stop
                    {
                        stopped = true;
                        let _ = child.start_kill();
                    }
                    continue;
                }
                for byte in &out_chunk[..count] {
                    if after_cr {
                        after_cr = false;
                        if *byte == b'\n' {
                            continue;
                        }
                    }
                    if *byte == b'\n' || *byte == b'\r' {
                        after_cr = *byte == b'\r';
                        let complete = std::mem::take(&mut line);
                        if !overflow && !stopped && !aborted
                            && on_line(complete).await == Flow::Stop
                        {
                            stopped = true;
                            let _ = child.start_kill();
                        }
                        overflow = false;
                    } else if !overflow {
                        if line.len() < MAX_LINE_BYTES {
                            line.push(*byte);
                        } else {
                            overflow = true;
                            line = Vec::new();
                        }
                    }
                }
            }
            read = async {
                match stderr.as_mut() {
                    Some(err) => err.read(&mut err_chunk).await,
                    None => std::future::pending().await,
                }
            }, if stderr.is_some() => {
                match read {
                    Ok(0) | Err(_) => stderr = None,
                    Ok(count) => {
                        let room = MAX_STDERR_BYTES.saturating_sub(stderr_bytes.len());
                        stderr_bytes.extend_from_slice(&err_chunk[..count.min(room)]);
                    }
                }
            }
        }
    }
    drop(stdout);
    drop(stderr);
    let status = child.wait().await?;
    if aborted || signal.is_some_and(AbortSignal::aborted) {
        return Ok(Ended::Aborted);
    }
    Ok(Ended::Exited {
        code: status.code(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        stopped,
    })
}

/// `code` as JavaScript prints a child's exit code: `null` after a signal.
pub(crate) fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "null".to_owned(), |code| code.to_string())
}

/// Node's `path.relative(from, to)` for absolute paths: `..` steps up to
/// their common ancestor, then down to `to`; on Windows compared without
/// case and joined with `\`, and `to` itself across drives.
pub(crate) fn node_relative(from: &Path, to: &Path) -> String {
    use std::path::Component;
    let absolute = |path: &Path| {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
        };
        crate::session::paths::lexical_normalize(&path)
    };
    let (from, to) = (absolute(from), absolute(to));
    let parts = |path: &Path| -> Vec<String> {
        path.components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                Component::Prefix(prefix) => {
                    Some(prefix.as_os_str().to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect()
    };
    let (from_parts, to_parts) = (parts(&from), parts(&to));
    let same = |a: &String, b: &String| {
        if cfg!(windows) {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    if cfg!(windows)
        && from_parts
            .first()
            .zip(to_parts.first())
            .is_some_and(|(a, b)| !same(a, b))
    {
        return to.to_string_lossy().into_owned();
    }
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| same(a, b))
        .count();
    let mut out: Vec<String> = vec!["..".into(); from_parts.len() - common];
    out.extend(to_parts[common..].iter().cloned());
    out.join(std::path::MAIN_SEPARATOR_STR)
}

/// Node's `path.basename`: the last component, ignoring trailing
/// separators (both kinds on Windows).
pub(crate) fn node_basename(path: &str) -> String {
    let is_separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let trimmed = path.trim_end_matches(is_separator);
    trimmed
        .rsplit(is_separator)
        .next()
        .unwrap_or_default()
        .to_owned()
}

// Every test here is Unix-only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // Pi's abort handling of grep and find: the child is killed and the run
    // ends, even when a process the child started keeps the pipes open.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_kills_and_reaps_the_child() {
        let controller = bake_ai::AbortController::new();
        let signal = controller.signal();
        let started = std::time::Instant::now();
        let abort = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            controller.abort();
        });
        // The background `sleep` holds the pipes; its pid is the first line.
        let args: Vec<OsString> = ["-c", "sleep 30 & echo $!; exec sleep 30"]
            .map(OsString::from)
            .to_vec();
        let mut seen = Vec::new();
        let ended = run_lines(Path::new("/bin/sh"), &args, Some(&signal), |line| {
            seen.push(line);
            std::future::ready(Flow::Continue)
        })
        .await
        .expect("spawn");
        abort.await.expect("abort task");
        let pid: i32 = String::from_utf8_lossy(&seen[0])
            .trim()
            .parse()
            .expect("pid");
        // SAFETY: `kill` takes plain integers; the pid is the test's own
        // background process.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        assert!(matches!(ended, Ended::Aborted), "{ended:?}");
        assert_eq!(seen.len(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_the_child_and_lines_split_like_readline() {
        let args: Vec<OsString> = ["-c", "printf 'a\\r\\nb\\rc\\nd'; exec sleep 30"]
            .map(OsString::from)
            .to_vec();
        let mut seen = Vec::new();
        let ended = run_lines(Path::new("/bin/sh"), &args, None, |line| {
            seen.push(String::from_utf8_lossy(&line).into_owned());
            std::future::ready(if seen.len() == 3 {
                Flow::Stop
            } else {
                Flow::Continue
            })
        })
        .await
        .expect("spawn");
        assert_eq!(seen, ["a", "b", "c"]);
        assert!(
            matches!(ended, Ended::Exited { stopped: true, .. }),
            "{ended:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_paths_follow_node() {
        assert_eq!(
            node_relative(Path::new("/a/b"), Path::new("/a/b/c/d.txt")),
            "c/d.txt"
        );
        assert_eq!(node_relative(Path::new("/a/b"), Path::new("/a/b")), "");
        assert_eq!(node_relative(Path::new("/a/b"), Path::new("/a/x")), "../x");
        assert_eq!(node_basename("/a/b/"), "b");
        assert_eq!(node_basename("file.txt"), "file.txt");
    }
}
