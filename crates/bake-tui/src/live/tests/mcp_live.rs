//! MCP lifecycle notices through the live port, with a real native stdio
//! server. Each fixture owns its directory and each port awaits its server
//! before the runtime or directory is dropped, including after an assertion.

use bake_coding_agent::mcp::{
    McpOptions, McpServerEntry, McpServerSettings, McpServerState, McpServers, McpStdioConfig,
    McpTimings, McpToolFilter, McpTransportConfig,
};
use bake_coding_agent::permissions::{ApprovalPolicy, StandingPermissions};
use bake_sandbox::{SandboxMode, UnavailableProvider};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "bake-tui-mcp-{}",
                bake_coding_agent::session::id::uuid_v4()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create fixture directory: {error}"),
            }
        };
        let fixture = Self(root);
        let output = std::process::Command::new("rustc")
            .args(["--edition=2024", "--crate-name=mcp_live_fixture"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/live/tests/fixtures/mcp_server.rs"
            ))
            .arg("-o")
            .arg(fixture.exe())
            .output()
            .expect("compile native fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn exe(&self) -> PathBuf {
        self.0
            .join(format!("server{}", std::env::consts::EXE_SUFFIX))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct McpHarness {
    h: Harness,
    servers: McpServers,
    control: BufReader<TcpStream>,
}

impl McpHarness {
    fn new(fixture: &Fixture, fail_start: bool) -> Self {
        let (runtime, faux, live) = setup(Vec::new(), Vec::new(), Some("test-key"));
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("fixture control listener");
        let address = listener.local_addr().expect("control address").to_string();
        let servers = runtime
            .block_on(async {
                live.session.start_mcp(McpOptions {
                    servers: vec![McpServerEntry {
                        name: "fixture".into(),
                        transport: McpTransportConfig::Stdio(McpStdioConfig {
                            command: fixture.exe().to_string_lossy().into_owned(),
                            args: vec![
                                address,
                                if fail_start { "fail-start" } else { "serve" }.into(),
                            ],
                            env: Vec::new(),
                            cwd: Some(fixture.0.to_string_lossy().into_owned()),
                        }),
                        settings: McpServerSettings::default(),
                    }],
                    standing: Arc::new(StandingPermissions::new(
                        SandboxMode::DangerFullAccess,
                        ApprovalPolicy::Never,
                        "custom",
                        &fixture.0,
                        None,
                    )),
                    sandbox: Arc::new(UnavailableProvider::new(None)),
                    timings: McpTimings::default(),
                    tool_filter: McpToolFilter::default(),
                    http: bake_coding_agent::mcp::McpHttpOptions::default(),
                    resolver: None,
                })
            })
            .expect("start servers");
        let (sender, updates) = std_mpsc::channel();
        let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
        let control = runtime
            .block_on(async {
                tokio::time::timeout(PATIENCE, listener.accept())
                    .await
                    .expect("fixture connects")
                    .expect("control connection")
                    .0
            })
            .into_std()
            .expect("control socket");
        control
            .set_nonblocking(false)
            .expect("blocking control socket");
        control
            .set_read_timeout(Some(PATIENCE))
            .expect("control read timeout");
        control
            .set_write_timeout(Some(PATIENCE))
            .expect("control write timeout");
        // The port owns teardown while the driver is held before begin.
        if fail_start {
            wait_state(&runtime, &servers, |state| {
                matches!(state, McpServerState::Failed { .. })
            });
        }
        port.begin();
        let h = Harness {
            runtime,
            faux,
            port: Some(port),
            updates,
        };
        Self {
            h,
            servers,
            control: BufReader::new(control),
        }
    }

    fn wait(&self, predicate: impl Fn(&McpServerState) -> bool) {
        wait_state(&self.h.runtime, &self.servers, predicate);
    }

    fn call(&mut self, name: &str) {
        writeln!(self.control.get_mut(), "{name}").expect("control request");
        self.control.get_mut().flush().expect("control flush");
        let mut acknowledgement = String::new();
        self.control
            .read_line(&mut acknowledgement)
            .expect("control reply");
        assert_eq!(acknowledgement.trim_end(), name);
    }

    fn error(&self, expected: &str) {
        let updates = self.h.until(|update| {
            matches!(update,
            RuntimeUpdate::Commit(rows) if rows.iter().any(|row| matches!(row,
                Row::Error(text) if text == expected)))
        });
        assert_eq!(committed(&updates), [Row::Error(expected.to_owned())]);
    }

    fn stop(&mut self) {
        self.h
            .port
            .take()
            .expect("port")
            .stop()
            .expect("port shutdown");
        let remaining: Vec<_> = self.h.updates.try_iter().collect();
        assert!(
            committed(&remaining).is_empty(),
            "shutdown emitted notices: {remaining:?}"
        );
    }
}

impl Drop for McpHarness {
    fn drop(&mut self) {
        if let Some(port) = self.h.port.take() {
            let _ = port.stop();
        }
    }
}

fn wait_state(
    runtime: &tokio::runtime::Runtime,
    servers: &McpServers,
    predicate: impl Fn(&McpServerState) -> bool,
) {
    runtime.block_on(async {
        let mut changes = servers.subscribe();
        tokio::time::timeout(PATIENCE, async {
            loop {
                changes.borrow_and_update();
                if servers
                    .statuses()
                    .iter()
                    .any(|status| predicate(&status.state))
                {
                    return;
                }
                changes.changed().await.expect("server owner alive");
            }
        })
        .await
        .expect("expected MCP state");
    });
}

#[test]
fn mcp_failures_reach_an_idle_view_without_server_output() {
    let fixture = Fixture::new();
    let mut live = McpHarness::new(&fixture, false);
    live.wait(|state| matches!(state, McpServerState::Connected { .. }));
    live.call("refresh_fail");
    live.error("MCP server \"fixture\" could not refresh its tools; using its previous tool list.");
    live.call("refresh_ok");
    live.wait(|state| {
        matches!(
            state,
            McpServerState::Connected {
                generation: 2,
                refresh_error: None,
                ..
            }
        )
    });
    live.call("refresh_fail");
    live.error("MCP server \"fixture\" could not refresh its tools; using its previous tool list.");
    live.call("disconnect");
    live.error("MCP server \"fixture\" disconnected; its tools are unavailable.");
    assert_eq!(live.h.faux.call_count(), 0);
    live.stop();
}

#[test]
fn an_mcp_startup_failure_is_shown_and_normal_shutdown_is_quiet() {
    let fixture = Fixture::new();
    let mut failed = McpHarness::new(&fixture, true);
    failed.error("MCP server \"fixture\" failed to start; its tools are unavailable.");
    failed.stop();
    let mut healthy = McpHarness::new(&fixture, false);
    healthy.wait(|state| matches!(state, McpServerState::Connected { .. }));
    healthy.stop();
}
