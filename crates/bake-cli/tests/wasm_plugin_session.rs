//! Wasm plugins that a Bake home enables, through real `AgentSession`s:
//! the faux model asks for real `write` and `edit` calls (and calls to
//! marker tools named after the hostile fixture's behaviours), the shipped
//! `bake` runs each plugin as its helper, and every case checks the
//! files, the session file on disk, `plugins.json`, and the helper
//! processes. Each test owns its temporary home and workspace.
//!
//! Only Linux and macOS check who can change plugin files, so only there
//! can a home enable a plugin; elsewhere `PluginHome` refuses, which its
//! unit tests cover.
#![cfg(any(target_os = "linux", target_os = "macos"))]

#[path = "wasm_plugin_host/fixtures.rs"]
mod fixtures;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult, ToolExecutionMode};
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, faux_assistant_blocks, faux_assistant_message,
    faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use bake_coding_agent::agent_session::{AgentSession, AgentSessionOptions};
use bake_coding_agent::auth_storage::AuthStorage;
use bake_coding_agent::model_registry::{ModelConfig, ModelRegistry};
use bake_coding_agent::permissions::{
    ApprovalPolicy, NoApprover, StandingPermissions, ToolPermissions, sandbox_file_tools,
};
use bake_coding_agent::plugins::installed::PluginHome;
use bake_coding_agent::plugins::wasm::{
    CompileQueue, HelperInfo, HostLimits, HostStatus, WasmPackage, WasmSetup,
};
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use bake_coding_agent::settings::Settings;
use bake_coding_agent::system_prompt::SystemPromptOptions;
use bake_coding_agent::tools::index::{ToolsOptions, create_all_tool_definitions};
use bake_sandbox::{SandboxMode, UnavailableProvider};
use fixtures::{TempDir, command, guard, package, process_gone};
use serde_json::{Value, json};

/// The bound on one awaited transition: a state change, never a deadline
/// under test.
const WAIT: Duration = Duration::from_secs(120);

async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

const SECRET: &str = "SECRET=1\n";
const HOSTILE: &[u8] = include_bytes!("../../../plugins/fixtures/hostile/hostile.wasm");

struct Fixture {
    dir: TempDir,
    home: PluginHome,
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
    setup: WasmSetup,
    /// Calls that reached a marker tool, by name.
    ran: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
        let apis = Arc::new(ApiRegistry::new());
        apis.register(faux.clone());
        let mut registry = ModelRegistry::new(
            ModelConfig::from_value(json!({}), "models.json"),
            AuthStorage::empty(),
        );
        registry
            .add_provider_config(
                "faux",
                json!({
                    "baseUrl": "http://127.0.0.1:9/unused",
                    "api": faux.model().api,
                    "models": [{ "id": "faux-1" }],
                }),
                Some("test-key".into()),
            )
            .expect("faux provider");
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.path().join("ws")).expect("ws");
        let home = PluginHome::new(&dir.path().join("home"));
        Self {
            dir,
            home,
            faux,
            registry: Arc::new(registry),
            apis,
            setup: WasmSetup {
                command: Some(command()),
                queue: CompileQueue::new(8),
                // Debug helpers compile slowly; the queue and calls get
                // room, and fuel is low so a spin traps quickly.
                limits: HostLimits {
                    startup_timeout: Duration::from_secs(60),
                    compile_timeout: Duration::from_secs(120),
                    call_timeout: Duration::from_secs(60),
                    call_fuel: 200_000_000,
                    max_component_bytes: 16 << 20,
                    ..fixtures::limits()
                },
            },
            ran: Arc::default(),
        }
    }

    fn ws(&self) -> std::path::PathBuf {
        self.dir.path().join("ws")
    }

    fn new_store(&self) -> SessionManager {
        SessionManager::create(
            &self.ws().to_string_lossy(),
            Some(&self.dir.path().join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("session")
    }

    /// Installs and enables `package`; its hash.
    fn enable(&self, package: &WasmPackage) -> String {
        let installed = self.home.install(package).expect("install");
        self.home.enable(&installed.sha256).expect("enable");
        installed.sha256
    }

    /// A tool that records each call it runs.
    fn marker(&self, name: &str) -> Arc<AgentTool> {
        let ran = Arc::clone(&self.ran);
        let tool_name = name.to_owned();
        let mut tool = AgentTool::new(
            name,
            name,
            "a marker",
            json!({ "type": "object" }),
            move |_id, _args, _signal, _update, _ctx| {
                ran.lock().expect("ran").push(tool_name.clone());
                Box::pin(async { Ok(AgentToolResult::text("ran")) })
            },
        );
        tool.execution_mode = Some(ToolExecutionMode::Sequential);
        Arc::new(tool)
    }

    /// A session over Pi's file tools under danger-full-access, where D24
    /// allows every write, and the `markers`, with the plugins the home
    /// enables now, loaded as `bake` loads them.
    async fn session(&self, store: SessionManager, markers: &[&str]) -> AgentSession {
        let loaded = self
            .home
            .load(&self.setup, Some(&self.ws()))
            .expect("plugins.json loads");
        let ws = self.ws();
        let permissions = ToolPermissions::new(
            Arc::new(StandingPermissions::new(
                SandboxMode::DangerFullAccess,
                ApprovalPolicy::Never,
                "test",
                &ws,
                Some("wasm-session".into()),
            )),
            Arc::new(NoApprover),
            Arc::new(UnavailableProvider::new(None)),
        );
        let mut tools: Vec<Arc<AgentTool>> = sandbox_file_tools(
            create_all_tool_definitions(&ws, &ToolsOptions::default()),
            &ws,
            &permissions,
        )
        .into_iter()
        .filter(|definition| matches!(definition.name(), "write" | "edit"))
        .map(|definition| Arc::new(definition.tool))
        .collect();
        tools.extend(markers.iter().map(|name| self.marker(name)));
        AgentSession::create(AgentSessionOptions {
            session: store,
            registry: Arc::clone(&self.registry).into(),
            settings_store: None,
            selection: None,
            apis: Arc::clone(&self.apis),
            settings: Settings::default(),
            model: self.registry.model("faux", "faux-1"),
            thinking_level: None,
            active_tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            tools,
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(ws.to_string_lossy()),
            docs_command: "bake docs".into(),
            plugins: loaded.registry,
            work: None,
            jobs: None,
        })
        .await
        .expect("session")
    }

    /// The model asks for `calls` (tool, args, id) in one response, then
    /// answers "done".
    fn script(&self, calls: &[(&str, Value, &str)]) {
        self.faux.append_responses(vec![
            faux_assistant_blocks(
                calls
                    .iter()
                    .map(|(tool, args, id)| faux_tool_call(tool, args.clone(), Some(id)))
                    .collect(),
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("done").into(),
        ]);
    }

    /// The model asks for `calls` in one response, for a run the test
    /// interrupts, so no answer is left queued for the next prompt.
    fn script_interrupted(&self, calls: &[(&str, Value, &str)]) {
        self.faux.append_responses(vec![
            faux_assistant_blocks(
                calls
                    .iter()
                    .map(|(tool, args, id)| faux_tool_call(tool, args.clone(), Some(id)))
                    .collect(),
                StopReason::ToolUse,
            )
            .into(),
        ]);
    }

    fn ran(&self) -> Vec<String> {
        self.ran.lock().expect("ran").clone()
    }

    fn failure(&self, id: &str) -> Option<(String, String)> {
        self.home.enabled().expect("plugins.json")[id]
            .failure
            .as_ref()
            .map(|failure| (failure.kind.clone(), failure.message.clone()))
    }
}

/// The session file's entries, read from disk.
fn file_entries(session: &AgentSession) -> Vec<Value> {
    let path = session.session_file().expect("persisted");
    std::fs::read_to_string(path)
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .filter(|entry: &Value| entry["type"] != "session")
        .collect()
}

fn decision<'a>(entries: &'a [Value], id: &str) -> Option<(usize, &'a Value)> {
    entries.iter().enumerate().find_map(|(index, entry)| {
        (entry["type"] == "custom"
            && entry["customType"] == "bake.plugin.decision"
            && entry["data"]["toolCallId"] == id)
            .then(|| (index, &entry["data"]))
    })
}

/// The index, text, and error flag of the `toolResult` entry for `id`.
fn tool_result(entries: &[Value], id: &str) -> (usize, String, bool) {
    entries
        .iter()
        .enumerate()
        .find_map(|(index, entry)| {
            let message = &entry["message"];
            (entry["type"] == "message"
                && message["role"] == "toolResult"
                && message["toolCallId"] == id)
                .then(|| {
                    let text = message["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    (index, text, message["isError"] == true)
                })
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

/// Asserts that `id` was denied with a decision written before its result;
/// the decision's data.
fn denied<'a>(entries: &'a [Value], id: &str) -> (&'a Value, String) {
    let (at, data) = decision(entries, id).unwrap_or_else(|| panic!("no decision for {id}"));
    let (result, text, is_error) = tool_result(entries, id);
    assert!(at < result, "{id}: the decision precedes the denial");
    assert!(is_error, "{id}: an error result");
    assert_eq!(data["reason"], text.as_str(), "{id}");
    (data, text)
}

/// Shuts `session` down, bounded, and asserts that every helper it ever
/// listed has been reaped.
async fn shutdown(session: &AgentSession, seen: &[HelperInfo]) {
    within("shutdown", session.shutdown()).await;
    for helper in seen.iter().chain(&session.plugin_helpers()) {
        if let Some(pid) = helper.pid {
            assert!(process_gone(pid), "helper {pid} survived the session");
        }
    }
}

/// Waits until a helper of `session` reaches a state `ready` accepts.
async fn helper_in(session: &AgentSession, ready: impl Fn(&HelperInfo) -> bool) -> HelperInfo {
    within("a helper state", async {
        loop {
            if let Some(helper) = session.plugin_helpers().into_iter().find(|h| ready(h)) {
                return helper;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

fn guard_calls() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "write",
            json!({ "path": ".env", "content": "pwned" }),
            "w-env",
        ),
        (
            "edit",
            json!({ "path": ".env", "edits": [{ "oldText": "SECRET=1", "newText": "SECRET=0" }] }),
            "e-env",
        ),
        (
            "write",
            json!({ "path": "notes.txt", "content": "kept" }),
            "w-notes",
        ),
    ]
}

// The owner's configuration in action: the home enables the hash-pinned
// guard package, and a session started from it blocks Pi's protected
// paths through the guard's helper, writing each decision, with the
// package hash, before its denial; the allowed write lands. The helper is
// the session's: alive while it runs, reaped by its shutdown. Control:
// once disabled, the same calls rewrite `.env`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_enabled_guard_blocks_protected_paths_in_a_real_session() {
    let fx = Fixture::new();
    let env = fx.ws().join(".env");
    std::fs::write(&env, SECRET).expect("seed");
    let hash = fx.enable(&guard());

    fx.script(&guard_calls());
    let session = fx.session(fx.new_store(), &[]).await;
    assert!(
        session.plugin_helpers().is_empty(),
        "no helper before a call"
    );
    within("prompt", session.prompt("go", Vec::new()))
        .await
        .expect("prompt");
    assert_eq!(std::fs::read_to_string(&env).unwrap(), SECRET);
    assert_eq!(
        std::fs::read_to_string(fx.ws().join("notes.txt")).unwrap(),
        "kept"
    );
    let entries = file_entries(&session);
    for id in ["w-env", "e-env"] {
        let (data, text) = denied(&entries, id);
        assert_eq!(text, "Path \".env\" is protected");
        assert_eq!(data["plugin"], "guard");
        assert_eq!(data["decision"], "block");
        assert_eq!(data["sha256"], hash.as_str());
    }
    assert!(decision(&entries, "w-notes").is_none());
    assert!(!tool_result(&entries, "w-notes").2);
    let helpers = session.plugin_helpers();
    assert_eq!(helpers.len(), 1, "one helper answered every call");
    assert_eq!(
        (helpers[0].generation, helpers[0].status),
        (1, HostStatus::Idle)
    );
    let pid = helpers[0].pid.expect("pid");
    assert!(!process_gone(pid), "the helper runs while the session does");
    shutdown(&session, &helpers).await;
    assert_eq!(fx.failure("guard"), None);

    // Control: disabled, nothing protects `.env`.
    fx.home.disable("guard").expect("disable");
    fx.script(&guard_calls());
    let unguarded = fx.session(fx.new_store(), &[]).await;
    within("prompt", unguarded.prompt("go", Vec::new()))
        .await
        .expect("prompt");
    assert_eq!(std::fs::read_to_string(&env).unwrap(), "pwned");
    assert!(decision(&file_entries(&unguarded), "w-env").is_none());
    shutdown(&unguarded, &[]).await;
}

// Hostile guests, one plugin per behaviour: a fuel-exhausting spin, a
// memory grab past the store total, a panic, an oversized reason, and an
// output flood. Each failure denies its call before the tool runs, is
// recorded in `plugins.json`, and leaves no helper; the flood, bounded by
// its pipes, is an ordinary block. A restarted process (a new session from
// the same home) and the resumed session file both block the failed
// plugins' tools at once, with no helper started, until the owner disables
// a plugin; enabled again, it is asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_failures_deny_persist_and_block_after_restart() {
    let fx = Fixture::new();
    let cases = [
        ("h-spin", "spin", json!({}), "trap"),
        ("h-grow", "grow", json!({ "mib": 100 }), "trap"),
        ("h-panic", "panic", json!({}), "trap"),
        ("h-reason", "reason", json!({ "bytes": 17 * 1024 }), "trap"),
        ("h-error", "error", json!({}), "error"),
    ];
    let mut hashes = Vec::new();
    for (id, tool, _, _) in &cases {
        hashes.push(fx.enable(&package(id, &[tool], HOSTILE.to_vec())));
    }
    fx.enable(&package("h-flood", &["flood-output"], HOSTILE.to_vec()));
    let markers = ["spin", "grow", "panic", "reason", "error", "flood-output"];
    // One call per case, with ids unique to the run.
    let calls = |run: &str| -> Vec<(&'static str, Value, String)> {
        cases
            .iter()
            .map(|(_, tool, args, _)| (*tool, args.clone(), format!("{tool}-{run}")))
            .collect()
    };
    let script = |calls: &[(&'static str, Value, String)]| {
        let calls: Vec<(&str, Value, &str)> = calls
            .iter()
            .map(|(tool, args, id)| (*tool, args.clone(), id.as_str()))
            .collect();
        fx.script(&calls);
    };
    let mut first = calls("1");
    first.push(("flood-output", json!({}), "flood".to_owned()));
    script(&first);
    let session = fx.session(fx.new_store(), &markers).await;
    within("prompt", session.prompt("go", Vec::new()))
        .await
        .expect("prompt");
    assert!(
        fx.ran().is_empty(),
        "no hostile call reached its tool: {:?}",
        fx.ran()
    );
    let entries = file_entries(&session);
    for (id, tool, _, kind) in &cases {
        let (data, text) = denied(&entries, &format!("{tool}-1"));
        assert_eq!(data["plugin"], *id);
        assert_eq!(data["decision"], "failed");
        assert_eq!(data["failure"]["kind"], *kind, "{id}: {text}");
        assert_eq!(data["failure"]["earlier"], false);
        assert!(text.starts_with(&format!("Plugin \"{id}\" failed, blocking execution: ")));
        let (recorded, _) = fx.failure(id).unwrap_or_else(|| panic!("{id} recorded"));
        assert_eq!(recorded, *kind);
    }
    let (flood, reason) = denied(&entries, "flood");
    assert_eq!(flood["decision"], "block");
    let report: Value = serde_json::from_str(&reason).expect("the flood's report");
    assert!(
        report["stdout"]["written"].as_u64().unwrap() < 4 << 20,
        "{report}"
    );
    assert_eq!(fx.failure("h-flood"), None);
    // Failed generations were shut down as they failed.
    let helpers = session.plugin_helpers();
    for helper in &helpers {
        if helper.plugin.as_str() != "h-flood" {
            assert_eq!(helper.status, HostStatus::Stopped, "{helper:?}");
            assert!(process_gone(helper.pid.expect("pid")));
        }
    }
    let file = session.session_file().expect("file");
    shutdown(&session, &helpers).await;

    // A new process: a new session, and the resumed one, from the same home.
    for (run, store) in [
        ("new", fx.new_store()),
        (
            "resumed",
            SessionManager::open(&file, None, None).expect("resume"),
        ),
    ] {
        script(&calls(run));
        let restarted = fx.session(store, &markers).await;
        within("prompt", restarted.prompt("again", Vec::new()))
            .await
            .expect("prompt");
        assert!(fx.ran().is_empty(), "{:?}", fx.ran());
        let entries = file_entries(&restarted);
        for (id, tool, _, kind) in &cases {
            let (data, text) = denied(&entries, &format!("{tool}-{run}"));
            assert_eq!(data["failure"]["earlier"], true, "{id}");
            assert_eq!(data["failure"]["kind"], *kind);
            assert!(text.starts_with(&format!("Plugin \"{id}\" failed earlier")));
        }
        assert!(
            restarted.plugin_helpers().is_empty(),
            "no failed plugin is started"
        );
        shutdown(&restarted, &[]).await;
    }

    // Only disabling resets a plugin: enabled again, the spin is asked
    // (and fails) afresh.
    fx.home.disable("h-spin").expect("disable");
    fx.home.enable(&hashes[0]).expect("enable");
    assert_eq!(fx.failure("h-spin"), None);
    fx.script(&[("spin", json!({}), "spin-3")]);
    let reset = fx.session(fx.new_store(), &markers).await;
    within("prompt", reset.prompt("reset", Vec::new()))
        .await
        .expect("prompt");
    let entries = file_entries(&reset);
    let (data, _) = denied(&entries, "spin-3");
    assert_eq!(data["failure"]["earlier"], false);
    assert!(fx.ran().is_empty());
    let helpers = reset.plugin_helpers();
    shutdown(&reset, &helpers).await;
}

// Esc while the guest runs: the call is answered `Operation aborted`, no
// decision or failure is recorded, and the helper is killed and reaped
// before the abort returns. The plugin still protects its tools: the next
// call starts a fresh helper generation, which answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_check_fails_nothing_and_a_new_helper_answers_next() {
    let mut fx = Fixture::new();
    // Far past the test's own bound: only the cancellation can end the
    // call in time, not the host's deadline.
    fx.setup.limits.call_timeout = Duration::from_secs(600);
    fx.enable(&package("sleepy", &["sleep", "grow"], HOSTILE.to_vec()));
    fx.script_interrupted(&[("sleep", json!({}), "s1")]);
    let session = Arc::new(fx.session(fx.new_store(), &["sleep", "grow"]).await);
    let prompt = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    let first = helper_in(&session, |h| matches!(h.status, HostStatus::Calling(_))).await;
    #[cfg(windows)]
    let handle = fixtures::ProcessHandle::open(first.pid.expect("pid"));
    within("abort", session.abort()).await;
    let _ = within("prompt", prompt).await.expect("prompt task");
    assert!(
        process_gone(first.pid.expect("pid")),
        "the cancelled helper was reaped"
    );
    #[cfg(windows)]
    assert!(handle.exit_code().is_some());
    let entries = file_entries(&session);
    assert!(
        decision(&entries, "s1").is_none(),
        "a cancellation decides nothing"
    );
    assert_eq!(tool_result(&entries, "s1").1, "Operation aborted");
    assert_eq!(fx.failure("sleepy"), None);
    assert!(fx.ran().is_empty());

    fx.script(&[("grow", json!({ "mib": 1 }), "g1")]);
    within("prompt", session.prompt("again", Vec::new()))
        .await
        .expect("prompt");
    let entries = file_entries(&session);
    let (data, text) = denied(&entries, "g1");
    assert_eq!(
        (data["decision"].as_str(), text.as_str()),
        (Some("block"), "grew 1048576")
    );
    let helpers = session.plugin_helpers();
    let current = helpers
        .iter()
        .find(|helper| helper.status == HostStatus::Idle)
        .expect("a live generation");
    assert_eq!(current.generation, 2);
    assert!(fx.ran().is_empty());
    shutdown(&session, &[first]).await;
}

// Shutting the session down while the guest runs, and while a helper
// compiles: shutdown interrupts both, records no failure, and returns only
// after every helper has been reaped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_a_call_or_a_compile_reaps_every_helper() {
    let fx = Fixture::new();
    fx.enable(&package("sleepy", &["sleep"], HOSTILE.to_vec()));
    fx.enable(&package(
        "slow",
        &["write"],
        fixtures::slow_component(1400, 1000),
    ));
    for (tool, args, state) in [
        ("sleep", json!({}), "calling"),
        (
            "write",
            json!({ "path": "x.txt", "content": "x" }),
            "compiling",
        ),
    ] {
        fx.script_interrupted(&[(tool, args, "c1")]);
        let session = Arc::new(fx.session(fx.new_store(), &["sleep"]).await);
        let prompt = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.prompt("go", Vec::new()).await }
        });
        let helper = helper_in(&session, |h| match state {
            "calling" => matches!(h.status, HostStatus::Calling(_)),
            _ => matches!(h.status, HostStatus::Compiling(_)),
        })
        .await;
        let pid = helper.pid.expect("pid");
        #[cfg(windows)]
        let handle = fixtures::ProcessHandle::open(pid);
        shutdown(&session, &[helper]).await;
        assert!(process_gone(pid), "{state}: reaped by shutdown");
        #[cfg(windows)]
        assert!(handle.exit_code().is_some());
        let _ = within("prompt", prompt).await.expect("prompt task");
        assert_eq!(fx.failure("sleepy"), None, "{state}");
        assert_eq!(fx.failure("slow"), None, "{state}");
        assert!(!fx.ws().join("x.txt").exists(), "{state}");
        assert!(fx.ran().is_empty());
    }
}

// A package the helper refuses (an import outside the allowlist) is
// enabled: its first call is denied with a recorded start failure before
// the tool runs, and the helper is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_component_fails_closed_and_is_recorded() {
    let fx = Fixture::new();
    let evil = fixtures::WIT.replace("world guest {", "world guest {\n  import evil: func();");
    let module = fixtures::plugin_module(
        "call $evil",
        r#"(import "$root" "evil" (func $evil))"#,
        fixtures::ON_TOOL_CALL,
    );
    fx.enable(&package(
        "evil",
        &["write"],
        fixtures::componentize(&module, &evil, "guest"),
    ));
    fx.script(&[("write", json!({ "path": "x.txt", "content": "x" }), "w")]);
    let session = fx.session(fx.new_store(), &[]).await;
    within("prompt", session.prompt("go", Vec::new()))
        .await
        .expect("prompt");
    assert!(!fx.ws().join("x.txt").exists());
    let entries = file_entries(&session);
    let (data, text) = denied(&entries, "w");
    assert_eq!(data["failure"]["kind"], "start", "{text}");
    assert!(text.contains("evil"), "{text}");
    assert_eq!(
        fx.failure("evil").map(|(kind, _)| kind).as_deref(),
        Some("start")
    );
    let helpers = session.plugin_helpers();
    shutdown(&session, &helpers).await;
}

// Arguments the host will not send are blocked without failing the plugin:
// the next ordinary call is asked and allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsendable_arguments_block_the_call_but_fail_nothing() {
    let fx = Fixture::new();
    fx.enable(&guard());
    let huge = "x".repeat((1 << 20) + 1);
    fx.script(&[
        (
            "write",
            json!({ "path": "big.txt", "content": huge }),
            "big",
        ),
        ("write", json!({ "path": "ok.txt", "content": "ok" }), "ok"),
    ]);
    let session = fx.session(fx.new_store(), &[]).await;
    // The 1 MiB arguments cross the faux model's compaction threshold.
    // Reserve the scripted responses for the plugin-denial exchange.
    session
        .set_auto_compaction_enabled(false)
        .await
        .expect("disable fixture compaction");
    let result = within("prompt", session.prompt("go", Vec::new())).await;
    let entries = file_entries(&session);
    let helpers = session.plugin_helpers();
    shutdown(&session, &helpers).await;
    assert_eq!(
        fx.faux.call_count(),
        2,
        "only the tool-call response and final answer are scripted"
    );
    result.expect("prompt");
    let answer = entries
        .iter()
        .rev()
        .find(|entry| entry["message"]["role"] == "assistant")
        .expect("final assistant answer");
    assert_eq!(answer["message"]["stopReason"], "stop");
    assert_eq!(
        answer["message"]["content"],
        json!([{ "type": "text", "text": "done" }])
    );
    let (data, text) = denied(&entries, "big");
    assert_eq!(data["decision"], "unchecked");
    assert!(
        text.starts_with("Plugin \"guard\" cannot check this call"),
        "{text}"
    );
    assert!(!fx.ws().join("big.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fx.ws().join("ok.txt")).unwrap(),
        "ok"
    );
    assert_eq!(fx.failure("guard"), None);
}

// The failure record is part of the denial path: a home whose
// `plugins.json` cannot be rewritten stops the run with that reason after
// the decision is written, so no session goes on believing the failure is
// recorded.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrecordable_failure_stops_the_run() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.enable(&package("h-panic", &["panic"], HOSTILE.to_vec()));
    fx.script(&[("panic", json!({}), "p")]);
    let session = fx.session(fx.new_store(), &["panic"]).await;
    // Read-only: the lock file and the replacement cannot be created.
    let plugins_dir = fx.home.config_path().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&plugins_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = within("prompt", session.prompt("go", Vec::new())).await;
    std::fs::set_permissions(&plugins_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.expect_err("the run stopped");
    assert!(error.contains("could not be recorded"), "{error}");
    let entries = file_entries(&session);
    let (at, data) = decision(&entries, "p").expect("the decision is written");
    assert_eq!(data["failure"]["kind"], "trap");
    let (result_at, text, _) = tool_result(&entries, "p");
    assert!(at < result_at);
    assert!(text.contains("could not be recorded"), "{text}");
    assert!(fx.ran().is_empty());
    let helpers = session.plugin_helpers();
    shutdown(&session, &helpers).await;
}
