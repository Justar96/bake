//! Runs the built `bake-rs session inspect --root --id` over plain and Zstd
//! v0, v1, and v2 logs whose request header holds an MCP tool schema, and
//! whose tool result holds a `meta` value, nested 10,000 and 1,000,000
//! containers deep. Each log migrates in memory and restores, as
//! TypeScript's read `open` migrates it; a recursive copy, comparison, or
//! drop of the payload overflows the child's main-thread stack instead.
//!
//! Each run owns a fresh directory and an empty home directory that the
//! child sees as `HOME`, `BAKE_HOME`, `DSH_HOME`, and `USERPROFILE`; the
//! layout must be unchanged and the home empty afterwards. Every child is
//! waited on with a deadline and killed and reaped when it passes.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Bounds a hung child, never a result.
const DEADLINE: Duration = Duration::from_secs(600);

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-cli-deep-{}-{name}-{count}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Run {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn finish(mut child: Child) -> Run {
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                stream.read_to_end(&mut bytes)?;
            }
            Ok::<_, std::io::Error>(bytes)
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let polled = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() <= DEADLINE => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => break Err(format!("bake-rs did not exit within {DEADLINE:?}")),
            Err(error) => break Err(format!("polling bake-rs failed: {error}")),
        }
    };
    let status = polled.map_err(|failure| {
        // A child that exited after the last poll makes kill fail; wait still reaps it.
        let killed = child.kill();
        let reaped = child.wait();
        format!("{failure} (kill: {killed:?}, wait: {reaped:?})")
    });
    let (stdout, stderr) = (stdout.join(), stderr.join());
    let status = status.unwrap_or_else(|failure| panic!("{failure}"));
    Run {
        status,
        stdout: stdout.expect("stdout drain").expect("read stdout"),
        stderr: stderr.expect("stderr drain").expect("read stderr"),
    }
}

/// One Zstandard frame of raw blocks: no content size or checksum, a 128 KiB
/// window, and blocks of at most 128 KiB.
fn raw_frame(content: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    let mut blocks = content.chunks(131_072).peekable();
    if blocks.peek().is_none() {
        frame.extend([1, 0, 0]);
    }
    while let Some(block) = blocks.next() {
        let header =
            (u32::try_from(block.len()).unwrap() << 3) | u32::from(blocks.peek().is_none());
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(block);
    }
    frame
}

/// An object chain `depth` containers deep around `1`.
fn nested(depth: usize) -> String {
    format!("{}1{}", "{\"k\":".repeat(depth), "}".repeat(depth))
}

/// The header record and rows of a completed v0, v1, or v2 turn that holds
/// `deep` in its header tool schema and as its tool result's `meta`.
fn released_log(version: u64, deep: &str) -> (String, String) {
    let header = if version == 2 {
        r#"{"type":"session","version":2,"id":"deep","createdAt":1000,"isSeeded":false,"delegationDepth":0}"#.to_owned()
    } else {
        format!(
            r#"{{"type":"session","version":{version},"id":"deep","createdAt":1000,"delegationDepth":0}}"#
        )
    };
    let stream = if version == 2 { r#","stream":[]"# } else { "" };
    let rows = [
        r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"request/header","seq":2,"time":1003,"data":{{"header":{{"config":{{"provider":"p","model":"m"}},"system":"sys","tools":[{{"name":"echo","description":"d","parameters":{{"type":"object","properties":{{"x":{deep}}}}}}}]}},"reason":"initial"}}}}"#
        ),
        r#"{"type":"user/message","seq":3,"time":1004,"data":{"role":"user","id":"u1","content":[{"type":"text","text":"hi"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#.to_owned(),
        format!(
            r#"{{"type":"assistant/message","seq":4,"time":1005,"data":{{"turn":1,"step":1,"message":{{"id":"a1","role":"assistant","content":[{{"type":"tool-call","id":"c1","name":"echo","arguments":"{{}}"}}],"source":{{"kind":"model","provider":"p","model":"m"}}}}{stream}}},"surfaceOp":"append"}}"#
        ),
        r#"{"type":"tool/call","seq":5,"time":1006,"data":{"turn":1,"step":1,"callId":"c1","name":"echo","arguments":"{}"}}"#.to_owned(),
        format!(
            r#"{{"type":"tool/result","seq":6,"time":1007,"data":{{"turn":1,"step":1,"message":{{"id":"r1","role":"user","content":[{{"type":"tool-result","toolCallId":"c1","content":[{{"type":"text","text":"out"}}]}}],"source":{{"kind":"tool","callId":"c1"}}}},"meta":{deep}}},"surfaceOp":"append"}}"#
        ),
        r#"{"type":"step/end","seq":7,"time":1008,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"turn/end","seq":8,"time":1009,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
    ];
    let body = rows.iter().map(|row| format!("{row}\n")).collect();
    (format!("{header}\n"), body)
}

/// Every file beneath `dir`, with its bytes.
fn tree(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}

/// Inspect the v0, v1, and v2 logs holding `depth`-deep payloads, plain and
/// Zstd, through the lookup form.
fn inspect_deep(depth: usize) {
    let deep = nested(depth);
    for version in [0_u64, 1, 2] {
        for zstd in [false, true] {
            let scratch = Scratch::new("case");
            let (case, home) = (scratch.0.join("case"), scratch.0.join("home"));
            let dir = case.join("root").join("_no-cwd").join("deep");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir(&home).unwrap();
            let name = match version {
                0 => "session.jsonl",
                1 => "session.v1.jsonl",
                _ => "session.v2.jsonl",
            };
            let (header, body) = released_log(version, &deep);
            let (name, bytes) = if zstd {
                let mut bytes = raw_frame(header.as_bytes());
                bytes.extend(raw_frame(body.as_bytes()));
                (format!("{name}.zstd"), bytes)
            } else {
                (name.to_owned(), format!("{header}{body}").into_bytes())
            };
            std::fs::write(dir.join(name), &bytes).unwrap();
            let before = tree(&case);
            let budget = (bytes.len() * 2).to_string();
            let mut command = Command::new(env!("CARGO_BIN_EXE_bake-rs"));
            command
                .args(["session", "inspect", "--root", "root", "--id", "deep"])
                .args(["--max-bytes", &budget, "--max-source-seqs", "64"])
                .args(["--max-entries", "64"])
                .args(["--compression", if zstd { "zstd" } else { "none" }])
                .current_dir(&case)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for variable in ["HOME", "BAKE_HOME", "DSH_HOME", "USERPROFILE"] {
                command.env(variable, &home);
            }
            let run = finish(command.spawn().unwrap());
            let context = format!("v{version} zstd={zstd} depth {depth}");
            assert!(
                run.status.success(),
                "{context}: {:?}: {}",
                run.status,
                String::from_utf8_lossy(&run.stderr)
            );
            let record: Value = serde_json::from_slice(&run.stdout).expect("one JSON record");
            assert_eq!(record["status"], "restored", "{context}: {record}");
            assert_eq!(record["storedEventCount"], 11, "{context}: {record}");
            assert_eq!(
                record["projection"]["messageCount"], 4,
                "{context}: {record}"
            );
            assert_eq!(tree(&case), before, "{context}: the layout changed");
            assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "{context}");
        }
    }
}

#[test]
fn lookup_migrates_payloads_ten_thousand_deep() {
    inspect_deep(10_000);
}

#[test]
fn lookup_migrates_payloads_a_million_deep() {
    inspect_deep(1_000_000);
}
