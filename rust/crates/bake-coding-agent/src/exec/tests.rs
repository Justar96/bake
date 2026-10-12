use super::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use bake_ai::AbortController;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WAIT: Duration = Duration::from_secs(30);

struct Fixture {
    dir: PathBuf,
    exe: PathBuf,
}

impl Fixture {
    fn get() -> Arc<Self> {
        static FIXTURE: Mutex<Weak<Fixture>> = Mutex::new(Weak::new());
        let mut slot = FIXTURE.lock().unwrap();
        if let Some(fixture) = slot.upgrade() {
            return fixture;
        }
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).unwrap();
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-exec-{id}"));
        std::fs::create_dir(&dir).unwrap();
        let fixture = Arc::new(Self {
            exe: dir.join(format!("exec-child{}", std::env::consts::EXE_SUFFIX)),
            dir,
        });
        let output = std::process::Command::new("rustc")
            .args(["--edition=2024", "--crate-name=exec_child"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/pi-exec/child.rs"
            ))
            .arg("-o")
            .arg(&fixture.exe)
            .output()
            .expect("compile the native subprocess fixture with the pinned toolchain");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        *slot = Arc::downgrade(&fixture);
        fixture
    }

    async fn run(&self, args: &[&str], options: ExecOptions) -> ExecResult {
        let args: Vec<_> = args.iter().map(OsString::from).collect();
        tokio::time::timeout(
            WAIT,
            exec_command(self.exe.as_os_str(), &args, &self.dir, options),
        )
        .await
        .expect("exec settles")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Windows can hold the executable mapping briefly after its last
        // control connection closes, especially on the drop path.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => break,
                Err(error)
                    if cfg!(windows)
                        && std::time::Instant::now() < deadline
                        && matches!(
                            error.kind(),
                            io::ErrorKind::PermissionDenied | io::ErrorKind::DirectoryNotEmpty
                        ) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("remove subprocess fixture: {error}"),
            }
        }
    }
}

#[tokio::test]
async fn outputs_and_exit_codes_match_pi() {
    let fixture = Fixture::get();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/pi-exec/golden.json")).unwrap();
    for case in golden["cases"].as_array().unwrap() {
        let args: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let result = fixture.run(&args, ExecOptions::default()).await;
        assert_eq!(result.stdout, case["result"]["stdout"]);
        assert_eq!(result.stderr, case["result"]["stderr"]);
        assert_eq!(result.code, case["result"]["code"].as_i64().unwrap());
        assert!(!result.killed && !result.aborted && !result.timed_out);
        assert!(result.error.is_none() && result.signal.is_none());
    }
}

#[tokio::test]
async fn cwd_and_closed_stdin_are_honored_and_zero_timeout_is_disabled() {
    let fixture = Fixture::get();
    std::fs::write(fixture.dir.join("marker"), "the working directory").unwrap();
    let result = fixture.run(&["cwd"], ExecOptions::default()).await;
    assert_eq!(result.stdout, "the working directory");
    assert_eq!(result.code, 0);
    let result = fixture
        .run(
            &["stdin"],
            ExecOptions {
                timeout: Some(Duration::ZERO),
                ..ExecOptions::default()
            },
        )
        .await;
    assert_eq!(result.stdout, "0");
    assert_eq!(result.code, 0);
    assert!(!result.timed_out);
}

#[tokio::test]
async fn both_streams_are_drained_without_truncation_or_mixing() {
    let fixture = Fixture::get();
    let result = fixture.run(&["large"], ExecOptions::default()).await;
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, "o".repeat(64 * 8192));
    assert_eq!(result.stderr, "e".repeat(64 * 8192));
    assert!(!result.killed && !result.timed_out && !result.aborted);
}

#[tokio::test]
async fn pre_abort_and_invalid_timeout_do_not_spawn() {
    let fixture = Fixture::get();
    let marker = fixture.dir.join("must-not-exist");
    let marker = marker.to_str().unwrap();
    let controller = AbortController::new();
    controller.abort();
    let result = fixture
        .run(
            &["create", marker],
            ExecOptions {
                signal: Some(controller.signal()),
                ..ExecOptions::default()
            },
        )
        .await;
    assert!(result.aborted && result.killed);
    assert!(!result.timed_out);
    assert!(!Path::new(marker).exists());
    let result = fixture
        .run(
            &["create", marker],
            ExecOptions {
                timeout: Some(Duration::MAX),
                ..ExecOptions::default()
            },
        )
        .await;
    assert_eq!(result.code, 1);
    assert_eq!(result.error, Some(io::ErrorKind::InvalidInput));
    assert!(!Path::new(marker).exists());
}

#[tokio::test]
async fn spawn_errors_are_distinct_from_exit_one() {
    let fixture = Fixture::get();
    let result = exec_command(
        fixture.dir.join("absent").as_os_str(),
        &[],
        &fixture.dir,
        ExecOptions::default(),
    )
    .await;
    assert_eq!(result.code, 1);
    assert_eq!(result.error, Some(io::ErrorKind::NotFound));
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    let result = exec_command(
        OsStr::new("invalid\0program"),
        &[],
        &fixture.dir,
        ExecOptions::default(),
    )
    .await;
    assert_eq!(result.code, 1);
    assert!(result.error.is_some());
    let result = fixture
        .run(&["capture", "", "", "1"], ExecOptions::default())
        .await;
    assert_eq!(result.code, 1);
    assert!(result.error.is_none());
}

#[test]
fn credential_names_follow_the_typescript_pattern() {
    // packages/subprocess/subprocess/src/index.ts SENSITIVE_ENV_PATTERN;
    // names from tests/service.spec.ts, plus mixed case and substring cases.
    let names = [
        "PATH",
        "HOME",
        "LANG",
        "NORMAL",
        "SCRUB_PROBE_PASSWORD",
        "API_KEY",
        "client_secret",
        "myToken",
        "MONKEY",
        "key",
        "password",
        "secret",
        "token",
    ];
    let env = names.map(|name| (OsString::from(name), OsString::from("synthetic")));
    let kept: Vec<_> = scrub_environment(env).map(|(name, _)| name).collect();
    assert_eq!(kept, ["PATH", "HOME", "LANG", "NORMAL"].map(OsString::from));
}

struct Running {
    call: tokio::task::JoinHandle<ExecResult>,
    controller: AbortController,
    _fixture: Arc<Fixture>,
}

impl Running {
    fn new(fixture: Arc<Fixture>, mode: &str, address: String, timeout: Option<Duration>) -> Self {
        let controller = AbortController::new();
        let signal = controller.signal();
        let child_fixture = Arc::clone(&fixture);
        let mode = mode.to_owned();
        let call = tokio::spawn(async move {
            exec_command(
                child_fixture.exe.as_os_str(),
                &[mode.into(), address.into()],
                &child_fixture.dir,
                ExecOptions {
                    signal: Some(signal),
                    timeout,
                },
            )
            .await
        });
        Self {
            call,
            controller,
            _fixture: fixture,
        }
    }
    async fn done(&mut self) -> ExecResult {
        tokio::time::timeout(WAIT, &mut self.call)
            .await
            .expect("call settles")
            .expect("call task")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.controller.abort();
        self.call.abort();
    }
}

async fn ready(listener: &TcpListener) -> TcpStream {
    tokio::time::timeout(WAIT, async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut ready = [0; 5];
        stream.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        stream
    })
    .await
    .expect("child reaches readiness barrier")
}

async fn stopped(stream: &mut TcpStream) {
    let mut byte = [0];
    let read = tokio::time::timeout(WAIT, stream.read(&mut byte))
        .await
        .expect("child releases its connection");
    assert!(
        matches!(read, Ok(0))
            || read.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset)
    );
}

#[tokio::test]
async fn abort_stops_the_parent_and_grandchild() {
    let fixture = Fixture::get();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut call = Running::new(
        fixture,
        "tree",
        listener.local_addr().unwrap().to_string(),
        None,
    );
    let mut parent = ready(&listener).await;
    let mut leaf = ready(&listener).await;
    call.controller.abort();
    let result = call.done().await;
    assert!(result.aborted && result.killed);
    assert!(!result.timed_out);
    stopped(&mut parent).await;
    stopped(&mut leaf).await;
}

#[tokio::test]
async fn timeout_stops_a_running_process() {
    let fixture = Fixture::get();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut call = Running::new(
        fixture,
        "leaf",
        listener.local_addr().unwrap().to_string(),
        Some(Duration::from_secs(3600)),
    );
    let mut child = ready(&listener).await;
    // Startup is real I/O; only advance the deadline after readiness.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3600)).await;
    tokio::time::resume();
    let result = call.done().await;
    assert!(result.timed_out && result.killed);
    assert!(!result.aborted);
    stopped(&mut child).await;
}

#[tokio::test]
async fn dropping_the_future_stops_the_parent_and_grandchild() {
    let fixture = Fixture::get();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut call = Running::new(
        fixture,
        "tree",
        listener.local_addr().unwrap().to_string(),
        None,
    );
    let mut parent = ready(&listener).await;
    let mut leaf = ready(&listener).await;
    call.call.abort();
    assert!((&mut call.call).await.unwrap_err().is_cancelled());
    stopped(&mut parent).await;
    stopped(&mut leaf).await;
}

/// Pi's Windows inherited-stdio regression, using the same executable on
/// every OS. The leaf owns a control connection so cleanup can await it.
#[tokio::test]
async fn normal_exit_settles_while_a_descendant_holds_idle_pipes() {
    let fixture = Fixture::get();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut call = Running::new(
        fixture,
        "orphan",
        listener.local_addr().unwrap().to_string(),
        None,
    );
    let mut leaf = ready(&listener).await;
    let result = call.done().await;
    // Close the background fixture before assertions, including on failure.
    leaf.write_all(b"x").await.unwrap();
    stopped(&mut leaf).await;
    assert_eq!(result.stdout, "HEAD\n");
    assert_eq!(result.code, 0);
    assert!(!result.killed && !result.timed_out && !result.aborted);
}

#[cfg(unix)]
#[tokio::test]
async fn signal_exit_is_separate_from_pis_zero_code() {
    let fixture = Fixture::get();
    let result = exec_command(
        OsStr::new("/bin/sh"),
        &["-c".into(), "kill -TERM $$".into()],
        &fixture.dir,
        ExecOptions::default(),
    )
    .await;
    assert_eq!(result.code, 0);
    assert_eq!(result.signal, Some(15));
    assert!(!result.killed && !result.aborted && !result.timed_out);
    assert!(result.error.is_none());
}

#[test]
fn child_environment_drops_credentials_and_keeps_ordinary_variables() {
    let fixture = Fixture::get();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "exec::tests::environment_host", "--nocapture"])
        .env("BAKE_EXEC_FIXTURE_EXE", &fixture.exe)
        .env("BAKE_EXEC_FIXTURE_DIR", &fixture.dir)
        .env("BAKE_EXEC_PROBE_TOKEN", "synthetic-credential")
        .env("BAKE_EXEC_PROBE_NORMAL", "ordinary")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// A separate process owns the seeded environment, avoiding global mutations
// alongside the concurrent native tests.
#[test]
fn environment_host() {
    let Some(exe) = std::env::var_os("BAKE_EXEC_FIXTURE_EXE") else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os("BAKE_EXEC_FIXTURE_DIR").unwrap());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (name, expected) in [
                ("BAKE_EXEC_PROBE_TOKEN", ""),
                ("BAKE_EXEC_PROBE_NORMAL", "ordinary"),
            ] {
                let result = exec_command(
                    &exe,
                    &["env".into(), name.into()],
                    &dir,
                    ExecOptions::default(),
                )
                .await;
                assert_eq!(result.code, 0);
                assert_eq!(result.stdout, expected);
            }
        });
}
