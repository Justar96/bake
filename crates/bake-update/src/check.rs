//! The candidate self-check: proof that the release about to become
//! current runs on this host.
//!
//! Before the pointer moves, the candidate's own executable runs
//! `--self-check` and must exit 0 within [`SELF_CHECK_TIMEOUT`] and print
//! the record `crates/bake-cli/src/self_check.rs` writes, naming the
//! version the manifest publishes and this host's release platform.
//! Otherwise the pointer never moves. Bake 0.3's launch check
//! (`packages/boot/updater/src/verify.ts`) is the reference; the native
//! check needs no Node and no private home, because the record reads
//! nothing.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::layout::executable;

/// How long a candidate has to print its record.
pub const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// How much of each output stream a failed check quotes.
const OUTPUT_LIMIT: usize = 8 * 1024;

/// How long the check waits for the candidate's output once it has
/// exited; see [`collect`].
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// How long a candidate that was just written may be reported as busy.
///
/// A release's binary is executed moments after it is unpacked. On Unix
/// a `fork` in another thread duplicates every open descriptor until
/// that child execs, so a descriptor on the new file can still be open
/// for writing somewhere, and the kernel answers `ETXTBSY`; on Windows a
/// scanner holds a new file open for a moment and the open is denied.
/// Both pass; a candidate that cannot be started at all still fails
/// within this bound.
const BUSY_RETRY: Duration = Duration::from_millis(50);
const BUSY_ATTEMPTS: u32 = 40;

/// Runs a candidate release's own self-check. The updater owns one; a
/// test supplies its own to make a candidate pass or fail without
/// building a binary.
pub trait CandidateCheck: Send + Sync {
    /// Check the release unpacked at `release`.
    ///
    /// # Errors
    ///
    /// The plain reason the release cannot be trusted to run.
    fn check(&self, release: &Path, version: &str, target: &str) -> Result<(), String>;
}

/// The shipped check: run the release's own executable.
#[derive(Debug, Clone)]
pub struct SelfCheck {
    pub timeout: Duration,
}

impl Default for SelfCheck {
    fn default() -> Self {
        Self {
            timeout: SELF_CHECK_TIMEOUT,
        }
    }
}

impl CandidateCheck for SelfCheck {
    fn check(&self, release: &Path, version: &str, target: &str) -> Result<(), String> {
        let Some(command) = executable(release) else {
            return Err("it has no bin/bake".to_owned());
        };
        let run = run_bounded(&command, self.timeout)?;
        if run.timed_out {
            return Err(format!(
                "it printed nothing within {}s",
                self.timeout.as_secs()
            ));
        }
        match run.code {
            Some(0) => {}
            Some(code) => {
                return Err(
                    match first_line(&run.stderr).or_else(|| first_line(&run.stdout)) {
                        Some(line) => format!("it exited {code}: {line}"),
                        None => format!("it exited {code}"),
                    },
                );
            }
            None => return Err("it was stopped by a signal".to_owned()),
        }
        check_record(&run.stdout, version, target)
    }
}

/// Whether a self-check record names this release and host.
///
/// # Errors
///
/// What the record said instead, in one line.
pub fn check_record(line: &str, version: &str, target: &str) -> Result<(), String> {
    let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line.trim()) else {
        return Err(match first_line(line) {
            Some(line) => format!("it printed '{line}', not a self-check record"),
            None => "it printed nothing".to_owned(),
        });
    };
    let member = |name: &str| match record.get(name) {
        Some(Value::String(value)) => Some(value.as_str()),
        _ => None,
    };
    if member("kind") != Some("bake-self-check") {
        return Err("it printed no self-check record".to_owned());
    }
    if record.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err("its self-check record is not schema 1".to_owned());
    }
    match member("version") {
        Some(found) if found == version => {}
        Some(found) => return Err(format!("it reported version {found}, not {version}")),
        None => return Err("its self-check record names no version".to_owned()),
    }
    match member("target") {
        Some(found) if found == target => Ok(()),
        Some(found) => Err(format!("it reported target {found}, not {target}")),
        None => Err("its self-check record names no target".to_owned()),
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run `command --self-check`, killing it once `timeout` passes, and
/// return only after it has exited and both readers have finished.
fn run_bounded(command: &Path, timeout: Duration) -> Result<Run, String> {
    let mut child = spawn_unbusy(command)?;
    let stdout = child.stdout.take().map(reader);
    let stderr = child.stderr.take().map(reader);
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(error) => return Err(format!("it could not be waited for: {error}")),
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok().and_then(|status| status.code());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Run {
        code,
        stdout: collect(stdout),
        stderr: collect(stderr),
        timed_out,
    })
}

/// Start the candidate, waiting out a file the host still reports as
/// busy; see [`BUSY_RETRY`].
fn spawn_unbusy(command: &Path) -> Result<std::process::Child, String> {
    let mut attempt = 0;
    loop {
        let started = Command::new(command)
            .arg("--self-check")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match started {
            Ok(child) => return Ok(child),
            Err(error) if busy(&error) && attempt < BUSY_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(BUSY_RETRY);
            }
            Err(error) => return Err(format!("it could not be started: {error}")),
        }
    }
}

/// Whether the host refused to start the file because something still
/// holds it, rather than because it cannot be run at all.
fn busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    // `ERROR_ACCESS_DENIED` and `ERROR_SHARING_VIOLATION`, which a
    // scanner's open produces.
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(5 | 32))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

/// Read one of the child's streams on its own thread, so a full pipe
/// never blocks the child's exit.
fn reader<R: std::io::Read + Send + 'static>(mut stream: R) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                break;
            }
            if bytes.len() < OUTPUT_LIMIT {
                bytes.extend_from_slice(&buffer[..read]);
            }
        }
        bytes.truncate(OUTPUT_LIMIT);
        let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    receiver
}

/// Wait briefly for a reader thread's text. The child has exited, so a
/// reader that is still waiting is held open by a process the candidate
/// started and left running; such a process must not hold the check
/// open, so its output is given up instead of waited for.
fn collect(reader: Option<mpsc::Receiver<String>>) -> String {
    reader
        .and_then(|reader| reader.recv_timeout(OUTPUT_GRACE).ok())
        .unwrap_or_default()
}

/// The first non-empty line of some output, shortened for one message.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.len() <= 200 {
        return Some(line.to_owned());
    }
    let mut end = 200;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &line[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = r#"{"kind":"bake-self-check","schema":1,"version":"0.4.1","runtime":"rust","target":"linux-x64","docs":{}}"#;

    #[test]
    fn a_record_must_name_the_published_version_and_this_host() {
        assert_eq!(check_record(RECORD, "0.4.1", "linux-x64"), Ok(()));
        assert_eq!(
            check_record(RECORD, "0.4.2", "linux-x64"),
            Err("it reported version 0.4.1, not 0.4.2".to_owned())
        );
        assert_eq!(
            check_record(RECORD, "0.4.1", "darwin-arm64"),
            Err("it reported target linux-x64, not darwin-arm64".to_owned())
        );
        assert_eq!(
            check_record(
                &RECORD.replace("\"schema\":1", "\"schema\":2"),
                "0.4.1",
                "linux-x64"
            ),
            Err("its self-check record is not schema 1".to_owned())
        );
        assert_eq!(
            check_record(
                &RECORD.replace("bake-self-check", "other"),
                "0.4.1",
                "linux-x64"
            ),
            Err("it printed no self-check record".to_owned())
        );
        assert!(
            check_record("Segmentation fault\n", "0.4.1", "linux-x64")
                .expect_err("not a record")
                .contains("not a self-check record")
        );
        assert_eq!(
            check_record("", "0.4.1", "linux-x64"),
            Err("it printed nothing".to_owned())
        );
    }

    #[test]
    fn a_release_without_an_executable_fails_its_check() {
        let dir = crate::update::tests::temp_dir("check-missing");
        let problem = SelfCheck::default()
            .check(dir.path(), "0.4.1", "linux-x64")
            .expect_err("no binary");
        assert_eq!(problem, "it has no bin/bake");
    }

    #[cfg(unix)]
    #[test]
    fn the_candidates_own_command_decides_the_check() {
        use crate::update::tests::fake_release;
        let dir = crate::update::tests::temp_dir("check-run");
        let check = SelfCheck {
            timeout: Duration::from_secs(20),
        };
        let good = dir.path().join("good");
        fake_release(&good, "0.4.1", "linux-x64", 0);
        assert_eq!(check.check(&good, "0.4.1", "linux-x64"), Ok(()));
        assert!(
            check
                .check(&good, "0.4.2", "linux-x64")
                .expect_err("wrong version")
                .contains("it reported version 0.4.1, not 0.4.2")
        );
        let failing = dir.path().join("failing");
        fake_release(&failing, "0.4.1", "linux-x64", 1);
        assert!(
            check
                .check(&failing, "0.4.1", "linux-x64")
                .expect_err("exits 1")
                .starts_with("it exited 1"),
        );
    }

    /// `ETXTBSY`: the kernel refuses to run a file another descriptor
    /// still holds open for writing, which a `fork` in another thread
    /// can keep open for a moment after an update writes the binary.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_candidate_another_descriptor_holds_open_is_waited_for() {
        use crate::update::tests::fake_release;
        let dir = crate::update::tests::temp_dir("check-busy");
        let release = dir.path().join("release");
        fake_release(&release, "0.4.1", "linux-x64", 0);
        let command = release.join("bin").join("bake");
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&command)
            .expect("a second writer");
        assert!(
            std::process::Command::new(&command)
                .arg("--self-check")
                .output()
                .is_err(),
            "the file is not reported as busy"
        );
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(writer);
        });
        assert_eq!(
            SelfCheck {
                timeout: Duration::from_secs(20),
            }
            .check(&release, "0.4.1", "linux-x64"),
            Ok(())
        );
        holder.join().expect("the holder");
    }

    #[cfg(unix)]
    #[test]
    fn a_candidate_that_never_answers_is_killed_at_the_deadline() {
        let dir = crate::update::tests::temp_dir("check-timeout");
        let release = dir.path().join("release");
        std::fs::create_dir_all(release.join("bin")).expect("bin");
        crate::update::tests::write_script(
            &release.join("bin").join("bake"),
            "#!/bin/sh\nsleep 30\n",
        );
        let problem = SelfCheck {
            timeout: Duration::from_millis(200),
        }
        .check(&release, "0.4.1", "linux-x64")
        .expect_err("a deadline");
        assert!(problem.contains("printed nothing within"), "{problem}");
    }
}
