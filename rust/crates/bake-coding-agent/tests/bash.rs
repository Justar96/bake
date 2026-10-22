//! Pi v1.1.0's bash tool. `fixtures/pi-bash/golden.json` records Pi's own
//! definition, results, errors, and updates (its generator runs Pi's
//! code); the tests below port the bash cases of Pi's `test/tools.test.ts`,
//! `test/tool-system-prompt-contributions.test.ts`,
//! `test/bash-close-hang-windows.test.ts`, and regressions #5208 and #5303,
//! each naming the Pi test it follows, plus Bake's process-ownership cases.
//!
//! Tests that run a real shell skip with a printed note where no bash
//! resolves (Windows without Git Bash).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bake_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, BoxFuture};
use bake_ai::{AbortController, UserContentBlock};
use bake_coding_agent::tools::bash::shell::{
    CommandTransport, ShellConfig, bash_shell_config, get_shell_config,
    is_legacy_wsl_bash_path_under, windows_shell_config,
};
use bake_coding_agent::tools::bash::{
    BashExecError, BashExecOptions, BashModel, BashOperations, BashToolContext, BashToolOptions,
    LocalShellOperations, SESSION_VARIABLES, bash_tool_prompt, create_bash_tool,
    create_local_bash_operations,
};
use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("bake-bash-test-{id}"));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn options(&self) -> BashToolOptions {
        BashToolOptions {
            spill_directory: Some(self.0.clone()),
            ..BashToolOptions::default()
        }
    }

    /// A path inside the directory as bash spells it, single-quoted.
    fn bash_path(&self, name: &str) -> String {
        let path = self.0.join(name).to_string_lossy().replace('\\', "/");
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/pi-bash/golden.json")).unwrap()
}

/// Whether a local bash resolves; prints why a test is skipped otherwise.
fn have_bash(test: &str) -> bool {
    match get_shell_config(None) {
        Ok(_) => true,
        Err(error) => {
            println!("skipping {test}: no bash found ({error})");
            false
        }
    }
}

fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run(tool: &AgentTool, params: Value) -> Result<AgentToolResult, String> {
    (tool.execute)(
        "call".to_owned(),
        params,
        None,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await
}

/// Bake's `SENSITIVE_ENV_PATTERN` contract from
/// `packages/subprocess/subprocess/src/index.ts`, through the real bash tool.
/// The re-executed fixture owns its environment; parallel tests inherit none
/// of these synthetic values.
#[tokio::test]
async fn bash_drops_ambient_credentials_and_keeps_ordinary_variables() {
    if !have_bash("bash_drops_ambient_credentials_and_keeps_ordinary_variables") {
        return;
    }
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "bash_environment_host", "--nocapture"])
        .env("BAKE_BASH_ENV_FIXTURE", "1")
        .env("BAKE_BASH_PROBE_KEY", "synthetic-key")
        .env("BAKE_BASH_PROBE_PASSWORD", "synthetic-password")
        .env("BAKE_BASH_PROBE_secret", "synthetic-secret")
        .env("BAKE_BASH_PROBE_myToken", "synthetic-token")
        .env("BAKE_BASH_PROBE_NORMAL", "ordinary")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(status) => status.unwrap(),
        Err(_) => {
            child.kill().await.unwrap();
            panic!("bash environment fixture did not finish within 30 seconds");
        }
    };
    assert!(status.success(), "bash environment fixture exited {status}");
}

#[tokio::test]
async fn bash_environment_host() {
    if std::env::var_os("BAKE_BASH_ENV_FIXTURE").is_none() {
        return;
    }
    let dir = TempDir::new();
    let tool = create_bash_tool(&dir.0, dir.options());
    let result = run(
        &tool,
        json!({
            "command": "printf '%s|%s|%s|%s|%s' \"${BAKE_BASH_PROBE_KEY-}\" \"${BAKE_BASH_PROBE_PASSWORD-}\" \"${BAKE_BASH_PROBE_secret-}\" \"${BAKE_BASH_PROBE_myToken-}\" \"${BAKE_BASH_PROBE_NORMAL-}\"",
            "timeout": 10,
        }),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text(&result));
    assert_eq!(text(&result), "||||ordinary");
}

// ---------------------------------------------------------------------------
// Scripted operations from the golden file.

fn expand(spec: &Value) -> Vec<Vec<u8>> {
    if let Some(text) = spec.get("text").and_then(Value::as_str) {
        return vec![text.as_bytes().to_vec()];
    }
    if let Some(bytes) = spec.get("bytes").and_then(Value::as_array) {
        return vec![
            bytes
                .iter()
                .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
                .collect(),
        ];
    }
    if let Some(repeat) = spec.get("repeat") {
        let text = repeat["text"].as_str().unwrap();
        let count = usize::try_from(repeat["count"].as_u64().unwrap()).unwrap();
        return vec![text.repeat(count).into_bytes()];
    }
    let lines = &spec["lines"];
    let prefix = lines["prefix"].as_str().unwrap();
    let pad = usize::try_from(lines["pad"].as_u64().unwrap()).unwrap();
    (lines["from"].as_u64().unwrap()..=lines["to"].as_u64().unwrap())
        .map(|index| format!("{prefix}{index:0pad$}\n").into_bytes())
        .collect()
}

#[derive(Clone)]
struct Scripted {
    chunks: Vec<Vec<u8>>,
    outcome: Result<Option<i64>, BashExecError>,
}

impl Scripted {
    fn from_golden(scenario: &Value) -> Self {
        let chunks = scenario["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(expand)
            .collect();
        let outcome = &scenario["outcome"];
        let outcome = match outcome.get("throw").and_then(Value::as_str) {
            Some("aborted") => Err(BashExecError::Aborted),
            Some(message) => match message.strip_prefix("timeout:") {
                Some(seconds) => Err(BashExecError::TimedOut(seconds.parse().unwrap())),
                None => Err(BashExecError::Failed(message.to_owned())),
            },
            None => Ok(outcome["exitCode"].as_i64()),
        };
        Self { chunks, outcome }
    }
}

impl BashOperations for Scripted {
    fn exec<'a>(
        &'a self,
        _command: String,
        _cwd: PathBuf,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>> {
        Box::pin(async move {
            for chunk in &self.chunks {
                (options.on_data)(chunk);
            }
            self.outcome.clone()
        })
    }
}

fn scripted_tool(dir: &TempDir, operations: impl BashOperations + 'static) -> AgentTool {
    create_bash_tool(
        &dir.0,
        BashToolOptions {
            operations: Some(Arc::new(operations)),
            ..dir.options()
        },
    )
}

/// The generator's abbreviation of long strings.
fn abbreviate(text: &str) -> Value {
    let points: Vec<char> = text.chars().collect();
    if points.len() <= 4096 {
        return Value::String(text.to_owned());
    }
    json!({ "$long": {
        "bytes": text.len(),
        "head": points[..256].iter().collect::<String>(),
        "tail": points[points.len() - 256..].iter().collect::<String>(),
    }})
}

/// The generator's normalization: spill paths, wall time, long strings.
fn normalize(value: &Value, path: Option<&str>) -> Value {
    match value {
        Value::String(text) => abbreviate(&match path {
            Some(path) => text.replace(path, "<FULL_OUTPUT_PATH>"),
            None => text.clone(),
        }),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| normalize(item, path)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| *key != "wall_time_seconds")
                .map(|(key, item)| (key.clone(), normalize(item, path)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn path_in(message: &str) -> Option<String> {
    let start = message.find("Full output: ")? + "Full output: ".len();
    let rest = &message[start..];
    let end = rest.find([']', '\n']).unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

/// Pi's `test/tools.test.ts` cases over custom operations ("should reject a
/// null exit code from custom operations", "should include full output path
/// for truncated timeout and abort errors", "should not count a trailing
/// newline as an extra truncated bash output line", "should decode UTF-8
/// characters split across output chunks") and every other scripted
/// scenario in the golden file: result, thrown text, each streamed update,
/// and the spill file, byte for byte. Tokio's paused clock keeps the update
/// throttle's timing as it was in Pi's single synchronous run.
#[tokio::test(start_paused = true)]
async fn scripted_results_errors_and_updates_match_pi() {
    let oracle = golden();
    for scenario in oracle["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let dir = TempDir::new();
        let scripted = Scripted::from_golden(scenario);
        let all_bytes: Vec<u8> = scripted.chunks.concat();
        let tool = scripted_tool(&dir, scripted);
        let updates = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&updates);
        let outcome = (tool.execute)(
            "call".to_owned(),
            json!({ "command": "scripted" }),
            None,
            AgentToolUpdateCallback::new(move |update| sink.lock().unwrap().push(update)),
            bake_agent::ToolContext::detached(),
        )
        .await;
        let path = match &outcome {
            Ok(result) => result
                .details
                .as_ref()
                .and_then(|details| details.get("fullOutputPath"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            Err(message) => path_in(message),
        };
        match &outcome {
            Ok(result) => assert_eq!(
                normalize(&serde_json::to_value(result).unwrap(), path.as_deref()),
                scenario["result"],
                "{name}: result"
            ),
            Err(message) => assert_eq!(
                normalize(&Value::String(message.clone()), path.as_deref()),
                scenario["error"],
                "{name}: error"
            ),
        }
        let updates: Vec<Value> = updates
            .lock()
            .unwrap()
            .iter()
            .map(|update| normalize(&serde_json::to_value(update).unwrap(), path.as_deref()))
            .collect();
        assert_eq!(
            Value::Array(updates),
            scenario["updates"],
            "{name}: updates"
        );
        match scenario["spillBytes"].as_u64() {
            Some(length) => {
                let spilled = fs::read(path.unwrap()).unwrap();
                assert_eq!(spilled.len() as u64, length, "{name}: spill length");
                assert_eq!(spilled, all_bytes, "{name}: spill keeps the raw bytes");
            }
            None => assert!(path.is_none(), "{name}: no spill"),
        }
    }
}

/// Pi `test/tool-system-prompt-contributions.test.ts`: the definition and
/// prompt text, and the guideline hidden with the session environment.
#[test]
fn definition_and_prompt_match_pi() {
    let oracle = golden();
    let definition = &oracle["definition"];
    let tool = create_bash_tool("/workspace", BashToolOptions::default());
    assert_eq!(tool.name, definition["name"]);
    assert_eq!(tool.label, definition["label"]);
    assert_eq!(tool.description, definition["description"]);
    assert_eq!(
        serde_json::to_string(&tool.parameters).unwrap(),
        definition["parametersJson"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::to_string(tool.output_schema.as_ref().unwrap()).unwrap(),
        definition["outputSchemaJson"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::to_value(tool.constrained_sampling.as_ref().unwrap()).unwrap(),
        definition["constrainedSampling"]
    );
    let declaration = tool.declaration();
    assert_eq!(declaration.parameters, definition["parameters"]);

    // Pi's one guideline names the `PI_*` variables, so Bake's tool
    // contributes none (D33); the snippet is Pi's.
    let prompt = bash_tool_prompt();
    assert_eq!(prompt.name, "bash");
    assert_eq!(
        prompt.snippet.as_deref(),
        definition["promptSnippet"].as_str()
    );
    assert_eq!(json!(prompt.snippet), definition["contribution"]["snippet"]);
    assert_eq!(
        definition["promptGuidelines"],
        json!([
            "You can inspect PI_* environment variables for current model and session details."
        ])
    );
    assert!(prompt.guidelines.is_empty());
}

/// Failures Pi's local operations raise before spawning: invalid timeouts,
/// "should throw error when cwd does not exist", and "should pass shellPath
/// through to shell resolution".
#[tokio::test]
async fn local_failures_before_spawn_match_pi() {
    let oracle = golden();
    let dir = TempDir::new();
    for case in oracle["local"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let cwd = case["cwd"]
            .as_str()
            .map_or_else(|| dir.0.clone(), PathBuf::from);
        let shell_path = case["options"]["shellPath"].as_str().map(PathBuf::from);
        if name == "missing-cwd" && !have_bash(name) {
            continue;
        }
        let tool = create_bash_tool(
            cwd,
            BashToolOptions {
                shell_path,
                ..dir.options()
            },
        );
        let error = run(&tool, case["params"].clone()).await.unwrap_err();
        assert_eq!(error, case["error"].as_str().unwrap(), "{name}");
    }
}

/// Pi's Windows "No bash shell found" message, recorded with Git Bash
/// missing from both program directories and `PATH`.
#[test]
fn windows_without_bash_matches_pi() {
    let oracle = golden();
    let case = &oracle["windowsNoBash"];
    let error = windows_shell_config(
        case["programFiles"].as_str(),
        case["programFilesX86"].as_str(),
        |_| false,
        || None,
    )
    .unwrap_err();
    assert_eq!(error, case["error"].as_str().unwrap());

    let found = windows_shell_config(
        Some("C:\\Program Files"),
        None,
        |path| path == Path::new("C:\\Program Files\\Git\\bin\\bash.exe"),
        || None,
    )
    .unwrap();
    assert_eq!(found.args, ["-c"]);
    let on_path = windows_shell_config(
        None,
        None,
        |_| false,
        || Some(PathBuf::from("C:\\Windows\\System32\\bash.exe")),
    )
    .unwrap();
    assert_eq!(on_path.command_transport, CommandTransport::Stdin);
}

/// Pi "should resolve legacy WSL bash.exe to stdin command transport".
#[test]
fn legacy_wsl_bash_reads_the_command_from_stdin() {
    assert_eq!(
        bash_shell_config(PathBuf::from("C:\\Windows\\System32\\bash.exe")),
        ShellConfig {
            shell: PathBuf::from("C:\\Windows\\System32\\bash.exe"),
            args: vec!["-s".to_owned()],
            command_transport: CommandTransport::Stdin,
        }
    );
    // Without `%SystemRoot%`, `X:\Windows` on any drive.
    for path in [
        "c:/windows/sysnative/BASH.EXE",
        "D:\\Windows\\System32\\bash.exe",
    ] {
        assert!(
            is_legacy_wsl_bash_path_under(Path::new(path), None),
            "{path}"
        );
    }
    for path in [
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "C:\\Windows\\System32\\bash.exe.bak",
        "\\\\?\\C:\\Windows\\System32\\bash.exe",
        "/bin/bash",
        "",
    ] {
        assert!(
            !is_legacy_wsl_bash_path_under(Path::new(path), None),
            "{path}"
        );
    }
}

/// Pi v1.4.0 `test/windows-shell-config.test.ts`, "legacy WSL bash
/// detection" (#9490): launchers under the actual Windows directory.
#[test]
fn legacy_wsl_bash_is_found_under_the_windows_directory() {
    for (system_root, shell, stdin) in [
        (Some("D:\\WinNT\\"), "d:/WINNT/System32/BASH.EXE", true),
        (Some("D:\\WinNT"), "D:\\WinNT\\Sysnative\\bash.exe", true),
        (None, "E:\\Windows\\System32\\bash.exe", true),
        (Some("D:\\WinNT"), "D:\\Git\\bin\\bash.exe", false),
        (Some("D:\\WinNT"), "E:\\Windows\\System32\\bash.exe", false),
        (Some("WinNT"), "C:\\Windows\\System32\\bash.exe", false),
    ] {
        assert_eq!(
            is_legacy_wsl_bash_path_under(Path::new(shell), system_root),
            stdin,
            "{system_root:?} / {shell}"
        );
    }
}

/// Pi "should coalesce streaming updates for chatty output".
#[tokio::test(start_paused = true)]
async fn chatty_output_coalesces_updates() {
    let dir = TempDir::new();
    let chunks = (0..5000)
        .map(|index| format!("line {index}\n").into_bytes())
        .collect();
    let tool = scripted_tool(
        &dir,
        Scripted {
            chunks,
            outcome: Ok(Some(0)),
        },
    );
    let count = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&count);
    let result = (tool.execute)(
        "call".to_owned(),
        json!({ "command": "chatty" }),
        None,
        AgentToolUpdateCallback::new(move |_| *counter.lock().unwrap() += 1),
        bake_agent::ToolContext::detached(),
    )
    .await
    .unwrap();
    assert!(*count.lock().unwrap() < 25);
    assert!(text(&result).contains("line 4999"));
}

/// Pi `test/suite/regressions/5208-late-bash-output.test.ts`: output after
/// the operation settles is ignored.
#[tokio::test]
async fn late_output_after_exec_settles_is_ignored() {
    struct Late(Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>);
    impl BashOperations for Late {
        fn exec<'a>(
            &'a self,
            _command: String,
            _cwd: PathBuf,
            options: BashExecOptions,
        ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>> {
            Box::pin(async move {
                (options.on_data)(b"before\n");
                let on_data = Arc::clone(&options.on_data);
                *self.0.lock().unwrap() = Some(tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    on_data(b"late\n");
                }));
                Ok(Some(0))
            })
        }
    }
    let dir = TempDir::new();
    let task = Arc::new(Mutex::new(None));
    let tool = scripted_tool(&dir, Late(Arc::clone(&task)));
    let result = run(&tool, json!({ "command": "late-output" }))
        .await
        .unwrap();
    let late = task.lock().unwrap().take().unwrap();
    late.await.unwrap();
    assert_eq!(text(&result).trim(), "before");
}

/// Pi's `resolveSpawnContext`: the prefix, the context's directory and
/// session variables under Bake's `BAKE_*` names in place of Pi's `PI_*`,
/// and the hook's view.
#[tokio::test]
async fn spawn_context_carries_prefix_context_and_session_environment() {
    type Env = Vec<(OsString, OsString)>;
    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, PathBuf, Env)>>);
    impl BashOperations for Capture {
        fn exec<'a>(
            &'a self,
            command: String,
            cwd: PathBuf,
            options: BashExecOptions,
        ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>> {
            self.0
                .lock()
                .unwrap()
                .push((command, cwd, options.env.unwrap_or_default()));
            Box::pin(async { Ok(Some(0)) })
        }
    }
    let dir = TempDir::new();
    let capture = Arc::new(Capture::default());
    let context = BashToolContext {
        cwd: Some(dir.0.join("session")),
        session_id: "session-1".to_owned(),
        session_file: Some("/sessions/one.jsonl".to_owned()),
        model: Some(BashModel {
            provider: "cliproxyapi".to_owned(),
            id: "gpt".to_owned(),
        }),
        thinking_level: Some("high".to_owned()),
    };
    let make = |expose: bool| {
        let context = context.clone();
        let operations: Arc<dyn BashOperations> = Arc::clone(&capture) as Arc<dyn BashOperations>;
        create_bash_tool(
            &dir.0,
            BashToolOptions {
                operations: Some(operations),
                command_prefix: Some("export A=1".to_owned()),
                expose_session_environment: expose,
                context: Some(Arc::new(move || context.clone())),
                spawn_hook: Some(Arc::new(|mut spawn| {
                    spawn.env.push(("FROM_HOOK".into(), "yes".into()));
                    spawn
                })),
                bin_dir: Some(dir.0.join("bin")),
                ..dir.options()
            },
        )
    };
    run(&make(true), json!({ "command": "echo hi" }))
        .await
        .unwrap();
    run(&make(false), json!({ "command": "echo hi" }))
        .await
        .unwrap();
    let calls = capture.0.lock().unwrap();
    let lookup = |env: &[(OsString, OsString)], name: &str| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    };
    let (command, cwd, env) = &calls[0];
    assert_eq!(command, "export A=1\necho hi");
    assert_eq!(cwd, &dir.0.join("session"));
    assert_eq!(lookup(env, "BAKE_SESSION_ID").as_deref(), Some("session-1"));
    assert_eq!(
        lookup(env, "BAKE_SESSION_FILE").as_deref(),
        Some("/sessions/one.jsonl")
    );
    assert_eq!(lookup(env, "BAKE_PROVIDER").as_deref(), Some("cliproxyapi"));
    assert_eq!(lookup(env, "BAKE_MODEL").as_deref(), Some("gpt"));
    assert_eq!(lookup(env, "BAKE_REASONING_LEVEL").as_deref(), Some("high"));
    // Only an inherited value, as when a host agent such as Pi runs the
    // tests, may appear: the tool never sets Pi's name itself.
    assert_eq!(
        lookup(env, "PI_SESSION_ID"),
        std::env::var("PI_SESSION_ID").ok()
    );
    assert_eq!(lookup(env, "FROM_HOOK").as_deref(), Some("yes"));
    let path = env
        .iter()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("path"))
        .map(|(_, value)| PathBuf::from(value))
        .unwrap();
    assert_eq!(std::env::split_paths(&path).next(), Some(dir.0.join("bin")));
    let (_, _, hidden) = &calls[1];
    for name in SESSION_VARIABLES {
        assert!(lookup(hidden, name).is_none(), "{name}");
    }
}

/// Pi "should handle process spawn errors": a shell that does not exist.
#[tokio::test]
async fn spawn_errors_carry_nodes_code() {
    let dir = TempDir::new();
    let operations = LocalShellOperations::new("bash", || {
        Ok(ShellConfig {
            shell: PathBuf::from("/nonexistent-shell-path-xyz123"),
            args: vec!["-c".to_owned()],
            command_transport: CommandTransport::Argv,
        })
    });
    let tool = scripted_tool(&dir, operations);
    let error = run(&tool, json!({ "command": "echo test" }))
        .await
        .unwrap_err();
    assert_eq!(error, "spawn /nonexistent-shell-path-xyz123 ENOENT");
}

/// Pi "should pass shellPath through to shell resolution".
#[tokio::test]
async fn shell_path_reaches_resolution() {
    let dir = TempDir::new();
    let error = create_local_bash_operations(Some(PathBuf::from("/custom/bash")))
        .exec(
            "echo test".to_owned(),
            dir.0.clone(),
            BashExecOptions {
                on_data: Arc::new(|_| {}),
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        BashExecError::Failed("Custom shell path not found: /custom/bash".to_owned())
    );
}

// ---------------------------------------------------------------------------
// A real shell.

fn local_tool(dir: &TempDir) -> AgentTool {
    create_bash_tool(&dir.0, dir.options())
}

/// Pi "should execute simple commands" and "should report non-zero exit
/// codes as error results with structured content".
#[tokio::test]
async fn simple_commands_and_exit_codes() {
    if !have_bash("simple_commands_and_exit_codes") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let result = run(&tool, json!({ "command": "echo 'test output'" }))
        .await
        .unwrap();
    assert!(text(&result).contains("test output"));
    assert!(result.details.is_none());

    let failed = run(&tool, json!({ "command": "echo out; exit 3" }))
        .await
        .unwrap();
    assert!(failed.is_error);
    assert_eq!(text(&failed), "out\n\n\nCommand exited with code 3");
    let structured = failed.structured_content.unwrap();
    assert_eq!(structured["output"], "out\n");
    assert_eq!(structured["truncated"], false);
    assert_eq!(structured["exit_code"], 3);
    assert!(structured["wall_time_seconds"].is_number());
    assert!(structured.get("full_output_path").is_none());

    let ok = run(&tool, json!({ "command": "echo fine" })).await.unwrap();
    assert!(!ok.is_error);
    assert_eq!(ok.structured_content.as_ref().unwrap()["output"], "fine\n");
    assert_eq!(ok.structured_content.as_ref().unwrap()["exit_code"], 0);

    let empty = run(&tool, json!({ "command": "true" })).await.unwrap();
    assert_eq!(text(&empty), "(no output)");
    assert_eq!(empty.structured_content.as_ref().unwrap()["output"], "");

    for (command, code) in [("exit 255", 255), ("exit 256", 0), ("exit 1", 1)] {
        let result = run(&tool, json!({ "command": command })).await.unwrap();
        assert_eq!(
            result.structured_content.as_ref().unwrap()["exit_code"],
            code,
            "{command}"
        );
        assert_eq!(result.is_error, code != 0, "{command}");
    }
}

/// Pi "should map signal-killed commands to 128 plus the signal number"
/// and "should report signal-killed commands as errors while preserving
/// partial output".
#[cfg(unix)]
#[tokio::test]
async fn signal_killed_commands_exit_with_128_plus_the_signal() {
    let dir = TempDir::new();
    let operations = create_local_bash_operations(None);
    for (signal, code) in [("KILL", 137), ("TERM", 143)] {
        let exit = operations
            .exec(
                format!("kill -{signal} $$"),
                dir.0.clone(),
                BashExecOptions {
                    on_data: Arc::new(|_| {}),
                    signal: None,
                    timeout: None,
                    env: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(exit, Some(code));
        let tool = local_tool(&dir);
        let result = run(
            &tool,
            json!({ "command": format!("printf 'before-kill\\n'; kill -{signal} $$") }),
        )
        .await
        .unwrap();
        assert!(result.is_error);
        assert_eq!(
            text(&result),
            format!("before-kill\n\n\nCommand exited with code {code}")
        );
    }
}

/// Pi "should return up to 1 MiB of output in structured content" and
/// "should persist full output when truncation happens by line count
/// only": large output spills to a file the result names.
#[tokio::test]
async fn large_output_spills_to_a_file() {
    if !have_bash("large_output_spills_to_a_file") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let medium = run(&tool, json!({ "command": "seq 1 3000" }))
        .await
        .unwrap();
    assert!(!text(&medium).contains("\n1\n2\n"));
    let details = medium.details.as_ref().unwrap();
    assert_eq!(details["truncation"]["truncated"], true);
    assert_eq!(details["truncation"]["truncatedBy"], "lines");
    let medium_text = text(&medium);
    assert!(medium_text.ends_with(&format!(
        "\n\n[Showing lines 1001-3000 of 3000. Full output: {}]",
        details["fullOutputPath"].as_str().unwrap()
    )));
    let expected: String = (1..=3000).map(|index| format!("{index}\n")).collect();
    let structured = medium.structured_content.as_ref().unwrap();
    assert_eq!(structured["truncated"], false);
    assert_eq!(structured["output"], expected.as_str());
    assert_eq!(
        fs::read_to_string(details["fullOutputPath"].as_str().unwrap()).unwrap(),
        expected
    );

    let large = run(&tool, json!({ "command": "seq 1 300000" }))
        .await
        .unwrap();
    let structured = large.structured_content.as_ref().unwrap();
    let output = structured["output"].as_str().unwrap();
    assert_eq!(structured["truncated"], true);
    assert!(output.starts_with("1\n2\n3\n"));
    assert!(output.ends_with("299999\n300000\n"));
    assert!(output.contains("\n\n[... "));
    assert!(output.len() < 1024 * 1024 + 100);
    let path = structured["full_output_path"].as_str().unwrap();
    assert_eq!(
        Some(path),
        large.details.as_ref().unwrap()["fullOutputPath"].as_str()
    );
    assert!(fs::read_to_string(path).unwrap().ends_with("300000\n"));
    assert!(text(&large).contains(&format!(
        "\n\n[Showing lines 298001-300000 of 300000. Full output: {path}]"
    )));
}

/// Non-UTF-8 output: invalid bytes become U+FFFD for the model and stay
/// raw in the spill file.
#[tokio::test]
async fn non_utf8_output_is_replaced_for_the_model() {
    if !have_bash("non_utf8_output_is_replaced_for_the_model") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let result = run(&tool, json!({ "command": r"printf '\377\376 abc\n'" }))
        .await
        .unwrap();
    assert_eq!(text(&result), "\u{FFFD}\u{FFFD} abc\n");
    let big = run(
        &tool,
        json!({ "command": r"for i in $(seq 1 2100); do printf 'x\377\n'; done" }),
    )
    .await
    .unwrap();
    let path = big.details.as_ref().unwrap()["fullOutputPath"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fs::read(path).unwrap(), b"x\xff\n".repeat(2100));
    assert!(text(&big).starts_with("x\u{FFFD}\n"));
}

/// Pi "should prepend command prefix when configured", "should include
/// output from both prefix and command", and "should work without command
/// prefix".
#[tokio::test]
async fn command_prefix_runs_first() {
    if !have_bash("command_prefix_runs_first") {
        return;
    }
    let dir = TempDir::new();
    let with = |prefix: &str| {
        create_bash_tool(
            &dir.0,
            BashToolOptions {
                command_prefix: Some(prefix.to_owned()),
                ..dir.options()
            },
        )
    };
    let result = run(
        &with("export TEST_VAR=hello"),
        json!({ "command": "echo $TEST_VAR" }),
    )
    .await
    .unwrap();
    assert_eq!(text(&result).trim(), "hello");
    let result = run(
        &with("echo prefix-output"),
        json!({ "command": "echo command-output" }),
    )
    .await
    .unwrap();
    assert_eq!(text(&result).trim(), "prefix-output\ncommand-output");
    let result = run(&local_tool(&dir), json!({ "command": "echo no-prefix" }))
        .await
        .unwrap();
    assert_eq!(text(&result).trim(), "no-prefix");
}

/// Pi "bash uses ctx.cwd when provided" (`tool cwd resolution`), checked
/// through a file so Git Bash's path spelling does not matter.
#[tokio::test]
async fn context_directory_wins_over_the_tools() {
    if !have_bash("context_directory_wins_over_the_tools") {
        return;
    }
    let dir = TempDir::new();
    fs::write(dir.0.join("marker.txt"), "from ctx.cwd").unwrap();
    let cwd = dir.0.clone();
    let tool = create_bash_tool(
        std::env::temp_dir(),
        BashToolOptions {
            expose_session_environment: false,
            context: Some(Arc::new(move || BashToolContext {
                cwd: Some(cwd.clone()),
                ..BashToolContext::default()
            })),
            ..dir.options()
        },
    );
    let result = run(&tool, json!({ "command": "cat marker.txt" }))
        .await
        .unwrap();
    assert_eq!(text(&result), "from ctx.cwd");
}

/// Pi "should expose local bash operations for extension reuse".
#[tokio::test]
async fn local_operations_take_the_given_environment() {
    if !have_bash("local_operations_take_the_given_environment") {
        return;
    }
    let dir = TempDir::new();
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    env.push(("TEST_LOCAL_BASH_OPS".into(), "from-local-ops".into()));
    env.push(("TEST_LOCAL_BASH_TOKEN".into(), "explicit-credential".into()));
    let exit = create_local_bash_operations(None)
        .exec(
            "printf '%s|%s' \"$TEST_LOCAL_BASH_OPS\" \"$TEST_LOCAL_BASH_TOKEN\"".to_owned(),
            dir.0.clone(),
            BashExecOptions {
                on_data: Arc::new(move |data| sink.lock().unwrap().extend_from_slice(data)),
                signal: None,
                timeout: None,
                env: Some(env),
            },
        )
        .await
        .unwrap();
    assert_eq!(exit, Some(0));
    assert_eq!(
        String::from_utf8_lossy(&chunks.lock().unwrap()).trim(),
        "from-local-ops|explicit-credential"
    );
}

/// Pi "should send commands over stdin when shell resolution requires it",
/// with `bash -c cat` standing in for Pi's echoing Node script.
#[tokio::test]
async fn stdin_transport_writes_the_command_to_the_shell() {
    let Ok(bash) = get_shell_config(None) else {
        println!("skipping stdin_transport_writes_the_command_to_the_shell: no bash found");
        return;
    };
    let dir = TempDir::new();
    let operations = LocalShellOperations::new("bash", move || {
        Ok(ShellConfig {
            shell: bash.shell.clone(),
            args: vec!["-c".to_owned(), "cat".to_owned()],
            command_transport: CommandTransport::Stdin,
        })
    });
    let command = r#"name='World'; echo "Hello, ${name}!"; count=3; for i in $(seq 1 ${count}); do echo "Iteration ${i} of ${count}"; done"#;
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let exit = operations
        .exec(
            command.to_owned(),
            dir.0.clone(),
            BashExecOptions {
                on_data: Arc::new(move |data| sink.lock().unwrap().extend_from_slice(data)),
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(exit, Some(0));
    assert_eq!(String::from_utf8_lossy(&chunks.lock().unwrap()), command);
}

/// Pi `test/bash-close-hang-windows.test.ts` ("bash tool resolves after the
/// shell exits even if inherited stdio handles stay open"), on every
/// platform: a background child keeps the pipes open after the shell exits.
#[tokio::test]
async fn shell_exit_settles_while_a_background_child_holds_the_pipes() {
    if !have_bash("shell_exit_settles_while_a_background_child_holds_the_pipes") {
        return;
    }
    let dir = TempDir::new();
    let pid_file = dir.bash_path("background.pid");
    let tool = local_tool(&dir);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            &tool,
            json!({ "command": format!("sleep 60 & echo $! > {pid_file}; echo child-exiting") }),
        ),
    )
    .await
    .expect("the call settles while the child holds the pipes")
    .unwrap();
    assert!(text(&result).contains("child-exiting"));
    assert!(started.elapsed() < Duration::from_secs(10));
    // As in Pi, a normal exit leaves background children running; end it.
    run(
        &tool,
        json!({ "command": format!("kill $(cat {pid_file})") }),
    )
    .await
    .unwrap();
}

/// Pi `test/suite/regressions/5303-bash-output-truncation.test.ts`, with a
/// real process: output a background child writes after the shell exits
/// is still read while it keeps arriving. The child writes from its first
/// instant and never pauses, so only the grace after its last line ends the
/// read: a `$(seq ...)` list, computed before the first line, outlasted the
/// 100 ms grace on a loaded macOS runner.
#[cfg(unix)]
#[tokio::test]
async fn output_written_after_the_shell_exits_is_kept() {
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let result = run(
        &tool,
        json!({ "command": "(i=0; while [ $i -lt 100000 ]; do i=$((i+1)); echo TICK$i; done) & echo HEAD" }),
    )
    .await
    .unwrap();
    let output = result.structured_content.unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(output.starts_with("HEAD\n") || output.contains("\nHEAD\n"));
    assert!(
        output.ends_with("TICK100000\n"),
        "{}",
        &output[output.len().saturating_sub(100)..]
    );
}

/// Pi "should respect timeout", plus a timeout that kills the whole tree.
#[tokio::test]
async fn timeout_kills_the_command() {
    if !have_bash("timeout_kills_the_command") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let started = Instant::now();
    let error = run(
        &tool,
        json!({ "command": "echo started; sleep 30", "timeout": 0.5 }),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "started\n\n\nCommand timed out after 0.5 seconds");
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A command that closes stdout and stderr but keeps running: the call
/// waits for its exit, and a timeout still ends it.
#[tokio::test]
async fn closed_output_does_not_end_the_call() {
    if !have_bash("closed_output_does_not_end_the_call") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let started = Instant::now();
    let result = run(
        &tool,
        json!({ "command": "echo before; exec >&- 2>&-; sleep 1; exit 4" }),
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(text(&result), "before\n\n\nCommand exited with code 4");

    let started = Instant::now();
    let error = run(
        &tool,
        json!({ "command": "exec >&- 2>&-; sleep 30", "timeout": 0.3 }),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "Command timed out after 0.3 seconds");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
fn process_gone(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        // A killed orphan can linger as a zombie where PID 1 does not reap.
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: signal 0 only checks that the pid exists.
        unsafe { libc::kill(pid, 0) == -1 }
    }
}

#[cfg(unix)]
async fn wait_for_pids(path: &Path, count: usize) -> Vec<i32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            let pids: Vec<i32> = text
                .split_whitespace()
                .filter_map(|pid| pid.parse().ok())
                .collect();
            if pids.len() == count {
                return pids;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the command never wrote its pids"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
async fn assert_gone(pids: &[i32]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pids.iter().all(|pid| process_gone(*pid)) {
        assert!(Instant::now() < deadline, "processes {pids:?} survived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The shell and its grandchild, as `sleep 30 & sleep 30` leaves them.
#[cfg(unix)]
fn grandchild_command(dir: &TempDir) -> String {
    let pid_file = dir.bash_path("tree.pid");
    format!("sleep 30 & echo $$ $! > {pid_file}; sleep 30")
}

/// Abort mid-command kills the shell and its grandchild and awaits them.
#[cfg(unix)]
#[tokio::test]
async fn abort_kills_the_whole_tree() {
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let controller = AbortController::new();
    let call = (tool.execute)(
        "call".to_owned(),
        json!({ "command": grandchild_command(&dir) }),
        Some(controller.signal()),
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    );
    let call = tokio::spawn(call);
    let pids = wait_for_pids(&dir.0.join("tree.pid"), 2).await;
    let started = Instant::now();
    controller.abort();
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error, "Command aborted");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_gone(&pids).await;
}

/// A timeout kills the grandchild too.
#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_the_whole_tree() {
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let error = run(
        &tool,
        json!({ "command": grandchild_command(&dir), "timeout": 0.5 }),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "Command timed out after 0.5 seconds");
    let pids = wait_for_pids(&dir.0.join("tree.pid"), 2).await;
    assert_gone(&pids).await;
}

/// Dropping the call, as an agent shutdown does, kills the tree.
#[cfg(unix)]
#[tokio::test]
async fn dropping_the_call_kills_the_whole_tree() {
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let call = tokio::spawn((tool.execute)(
        "call".to_owned(),
        json!({ "command": grandchild_command(&dir) }),
        None,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    ));
    let pids = wait_for_pids(&dir.0.join("tree.pid"), 2).await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    assert_gone(&pids).await;
}

/// Abort on Windows: the call settles promptly with Pi's text.
#[cfg(windows)]
#[tokio::test]
async fn abort_ends_the_command() {
    if !have_bash("abort_ends_the_command") {
        return;
    }
    let dir = TempDir::new();
    let tool = local_tool(&dir);
    let controller = AbortController::new();
    let marker = dir.bash_path("started");
    let call = tokio::spawn((tool.execute)(
        "call".to_owned(),
        json!({ "command": format!("sleep 30 & touch {marker}; sleep 30") }),
        Some(controller.signal()),
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !dir.0.join("started").exists() {
        assert!(Instant::now() < deadline, "the command never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = Instant::now();
    controller.abort();
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error, "Command aborted");
    assert!(started.elapsed() < Duration::from_secs(10));
}
