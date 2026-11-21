//! Bounded native clipboard helper execution (`clipboard::feed`).
//!
//! Clipboard text may be sensitive, so it is streamed straight over the
//! helper's stdin pipe in memory without touching the filesystem. One shared
//! deadline covers writing stdin, closing EOF, and waiting for the helper to
//! exit.
//!
//! Helpers run through [`bake_process::OwnedProcess`], which uses non-blocking
//! stdin on Unix and overlapped named pipes inside a Job object on Windows:
//! - On timeout or write failure, Bake kills the owned tree and gives it a
//!   bounded grace period to settle. No writer thread is needed.
//! - On normal exit, the tree is detached (`TreeState::LeftRunning`) so
//!   clipboard-owner daemons forked by `xclip` or `wl-copy` continue running.

use std::borrow::Cow;
use std::io;
use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::clipboard::Tool;

/// Upper bound on settling a killed or exited helper after the main deadline.
const TEARDOWN_GRACE: Duration = Duration::from_millis(500);

/// Encodes `text` as UTF-8, or UTF-16LE with a byte order mark for Windows
/// `clip` / `clip.exe`.
fn encode_input(text: &str, utf16: bool) -> Cow<'_, [u8]> {
    if utf16 {
        let mut bytes = Vec::with_capacity(2 + text.len() * 2);
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        Cow::Owned(bytes)
    } else {
        Cow::Borrowed(text.as_bytes())
    }
}

/// Runs `tool` with `text` on its standard input under one shared `timeout`
/// covering write, EOF, and exit. Returns `true` only when every byte was
/// written without error and the helper exited cleanly before `timeout`.
pub(crate) fn feed(tool: Tool, text: &str, timeout: Duration) -> bool {
    feed_command(tool.program, tool.args, tool.utf16, text, timeout)
}

fn feed_command(program: &str, args: &[&str], utf16: bool, text: &str, timeout: Duration) -> bool {
    let input = encode_input(text, utf16);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    else {
        return false;
    };
    rt.block_on(feed_async(program, args, &input, timeout))
}

async fn feed_async(program: &str, args: &[&str], input: &[u8], timeout: Duration) -> bool {
    let spec = ProcessSpec {
        program: program.into(),
        args: args.iter().map(Into::into).collect(),
        cwd: None,
        env: None,
        stdin: true,
    };
    let Ok(mut process) = OwnedProcess::spawn(&spec, None).await else {
        return false;
    };
    let Some(stdin) = process.take_stdin() else {
        kill_and_settle(process).await;
        return false;
    };
    let stdout = process.take_stdout();
    let stderr = process.take_stderr();

    // Discard stdout/stderr while the leader runs so noisy helpers never block
    // on a full pipe, without waiting for forked daemons to close them.
    let drain = async {
        tokio::join!(drain_pipe(stdout), drain_pipe(stderr));
        std::future::pending::<()>().await;
    };

    let outcome = tokio::time::timeout(timeout, async {
        tokio::select! {
            res = async {
                write_and_close(stdin, input).await?;
                process.wait_exit().await
            } => res,
            () = drain => unreachable!(),
        }
    })
    .await;

    match outcome {
        Ok(Ok(())) => match tokio::time::timeout(TEARDOWN_GRACE, process.settle()).await {
            Ok(Ok(settled)) => settled.status.success(),
            _ => false,
        },
        Ok(Err(_)) | Err(_) => {
            kill_and_settle(process).await;
            false
        }
    }
}

async fn write_and_close(mut stdin: bake_process::ProcessStdin, input: &[u8]) -> io::Result<()> {
    stdin.write_all(input).await?;
    stdin.flush().await?;
    stdin.shutdown().await?;
    drop(stdin);
    Ok(())
}

async fn drain_pipe<R: AsyncRead + Unpin>(pipe: Option<R>) {
    if let Some(mut pipe) = pipe {
        let _ = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await;
    }
}

async fn kill_and_settle(mut process: OwnedProcess) {
    process.kill();
    let _ = tokio::time::timeout(TEARDOWN_GRACE, process.settle()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_input_preserves_utf8_and_utf16le_with_bom() {
        let sample = "hello é 漢 🦀\nline2";
        assert_eq!(encode_input(sample, false), sample.as_bytes());

        let utf16 = encode_input(sample, true);
        assert_eq!(&utf16[..2], &[0xff, 0xfe]);
        assert_eq!(utf16.len() % 2, 0);
        let (pairs, rem) = utf16[2..].as_chunks::<2>();
        assert!(rem.is_empty());
        let units: Vec<u16> = pairs.iter().copied().map(u16::from_le_bytes).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), sample);

        assert_eq!(encode_input("", false).as_ref(), b"");
        assert_eq!(encode_input("", true).as_ref(), &[0xff, 0xfe]);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        /// Larger than the default 64 KiB Linux/macOS pipe buffer so a stalled
        /// reader deterministically blocks a naive synchronous `write_all`.
        const OVER_PIPE_BYTES: usize = 256 * 1024;

        struct TempDir(PathBuf);

        impl TempDir {
            fn new(tag: &str) -> Self {
                static COUNTER: AtomicUsize = AtomicUsize::new(0);
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos());
                let dir = std::env::temp_dir().join(format!(
                    "bake-clipboard-feed-{tag}-{}-{}-{nanos}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::SeqCst),
                ));
                std::fs::create_dir(&dir).unwrap();
                Self(dir)
            }

            fn join(&self, name: &str) -> PathBuf {
                self.0.join(name)
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn large_unicode_payload() -> String {
            let chunk = "alpha-é-漢-🦀-0123456789\n";
            let payload = chunk.repeat(OVER_PIPE_BYTES / chunk.len() + 1);
            assert!(payload.len() > OVER_PIPE_BYTES);
            payload
        }

        #[test]
        fn stalled_writer_exceeding_pipe_capacity_hits_bounded_deadline() {
            let payload = large_unicode_payload();
            let started = Instant::now();
            let ok = feed(
                Tool {
                    program: "sh",
                    args: &["-c", "sleep 30 & exec sleep 30"],
                    utf16: false,
                },
                &payload,
                Duration::from_millis(200),
            );
            let elapsed = started.elapsed();

            assert!(!ok);
            assert!(
                elapsed < Duration::from_secs(2),
                "stalled write exceeded bounded deadline: {elapsed:?}"
            );
        }

        #[test]
        fn post_eof_stall_shares_same_deadline() {
            let started = Instant::now();
            let ok = feed_command(
                "sh",
                &["-c", "cat >/dev/null; exec sleep 30"],
                false,
                "hello",
                Duration::from_millis(200),
            );
            let elapsed = started.elapsed();

            assert!(!ok);
            assert!(
                elapsed < Duration::from_secs(2),
                "post-EOF stall exceeded bounded deadline: {elapsed:?}"
            );
        }

        #[test]
        fn truncated_write_fails_even_when_helper_exits_zero() {
            let payload = large_unicode_payload();
            assert!(!feed_command(
                "sh",
                &["-c", "dd bs=16 count=1 >/dev/null 2>&1; exit 0"],
                false,
                &payload,
                Duration::from_secs(5),
            ));
        }

        #[test]
        fn large_unicode_input_delivers_exact_utf8_and_utf16() {
            let dir = TempDir::new("unicode");
            let payload = large_unicode_payload();

            let utf8_file = dir.join("out.utf8");
            let utf8_path = utf8_file.to_str().unwrap();
            assert!(feed_command(
                "sh",
                &["-c", r#"cat > "$1""#, "_", utf8_path],
                false,
                &payload,
                Duration::from_secs(5),
            ));
            let got_utf8 = std::fs::read(&utf8_file).unwrap();
            assert_eq!(got_utf8, payload.as_bytes());
            assert_eq!(std::str::from_utf8(&got_utf8).unwrap(), payload);

            let utf16_file = dir.join("out.utf16");
            let utf16_path = utf16_file.to_str().unwrap();
            assert!(feed_command(
                "sh",
                &["-c", r#"cat > "$1""#, "_", utf16_path],
                true,
                &payload,
                Duration::from_secs(5),
            ));
            assert_eq!(
                std::fs::read(&utf16_file).unwrap(),
                encode_input(&payload, true).as_ref()
            );
        }

        #[test]
        fn clipboard_owner_daemon_survives_successful_copy() {
            let dir = TempDir::new("daemon");
            let out_file = dir.join("copied.txt");
            let hold_file = dir.join("hold");
            let after_file = dir.join("after");
            std::fs::write(&hold_file, b"").unwrap();

            let payload = large_unicode_payload();
            let started = Instant::now();
            let ok = feed_command(
                "sh",
                &[
                    "-c",
                    r#"cat > "$1"; (while [ -f "$2" ]; do sleep 0.05; done; : > "$3") </dev/null >/dev/null 2>&1 & exit 0"#,
                    "_",
                    out_file.to_str().unwrap(),
                    hold_file.to_str().unwrap(),
                    after_file.to_str().unwrap(),
                ],
                false,
                &payload,
                Duration::from_secs(5),
            );
            let elapsed = started.elapsed();

            assert!(ok);
            assert!(
                elapsed < Duration::from_secs(2),
                "feed waited for background clipboard daemon: {elapsed:?}"
            );
            assert_eq!(std::fs::read(&out_file).unwrap(), payload.as_bytes());
            assert!(!after_file.exists(), "daemon exited before release signal");

            // Release the daemon and confirm it was still alive after `feed` returned.
            std::fs::remove_file(&hold_file).unwrap();
            let wait_until = Instant::now() + Duration::from_secs(2);
            while !after_file.exists() && Instant::now() < wait_until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                after_file.is_file(),
                "background clipboard owner daemon did not survive successful exit"
            );
        }
    }
}
