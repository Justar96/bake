//! The `codemode` tool in a real session: Pi v1.3.0's
//! `test/suite/agent-session-codemode.test.ts` (without `models`), and
//! Bake's cases: requests without code mode stay as they are, nested calls
//! meet the session's approvals and plugins, an abort cancels and reaps a
//! nested `bash`, the store follows each branch through `/tree`, a fork,
//! and a resume, and a script cannot call `codemode` itself.

use std::path::PathBuf;
use std::time::Duration;

use bake_agent::AgentToolResult;
use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_tool_call,
};
use bake_ai::transcript::{get_current_system_prompt, get_current_tools};
use bake_ai::{StopReason, TranscriptContext, UsageCost};
use bake_sandbox::testing::RecordingProvider;
use bake_sandbox::{EscalationOutcome, SandboxMode, SandboxProvider};
use serde_json::{Value, json};

use super::tests::{Harness, harness, options};
use super::*;
use crate::permissions::approval::ApprovalFuture;
use crate::permissions::{
    ApprovalPolicy, ApprovalRequest, Approver, StandingPermissions, ToolPermissions,
    owned_sandbox_file_tools,
};
use crate::plugins::tests::{Scripted, block};
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;
use crate::tools::bash::BashToolOptions;
use crate::tools::codemode::{CODEMODE_STORE_ENTRY_TYPE, CodemodeMode};
use crate::tools::definition::ToolDefinition;
use crate::tools::index::{ToolsOptions, create_all_tool_definitions, create_bash_tool_definition};

const WAIT: Duration = Duration::from_secs(60);

const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn echo() -> AgentTool {
    AgentTool::new(
        "echo",
        "Echo",
        "Echo text back.\n\nSecond paragraph.",
        json!({
            "type": "object",
            "required": ["text"],
            "properties": { "text": { "type": "string", "description": "Text to echo" } },
        }),
        |_id, args, _signal, _update, _ctx| {
            Box::pin(async move {
                Ok(AgentToolResult::text(format!(
                    "echo: {}",
                    args["text"].as_str().unwrap_or_default()
                )))
            })
        },
    )
}

fn stats() -> AgentTool {
    let mut stats = AgentTool::new(
        "stats",
        "Stats",
        "Return structured stats",
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| {
            Box::pin(async {
                Ok(AgentToolResult {
                    structured_content: Some(json!({ "files": 2, "names": ["a", "b"] })),
                    ..AgentToolResult::text("2 files")
                })
            })
        },
    );
    stats.output_schema = Some(json!({
        "type": "object",
        "required": ["files", "names"],
        "properties": {
            "files": { "type": "number" },
            "names": { "type": "array", "items": { "type": "string" } },
        },
    }));
    stats
}

fn screenshot() -> AgentTool {
    AgentTool::new(
        "screenshot",
        "Screenshot",
        "Return a screenshot",
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![
                        UserContentBlock::Text(bake_ai::TextContent::new("captured")),
                        UserContentBlock::Image(bake_ai::ImageContent {
                            data: TINY_PNG_BASE64.to_owned(),
                            mime_type: "image/png".to_owned(),
                        }),
                    ],
                    ..AgentToolResult::default()
                })
            })
        },
    )
}

fn billed() -> AgentTool {
    AgentTool::new(
        "billed",
        "Billed",
        "Run a model",
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| {
            Box::pin(async {
                Ok(AgentToolResult {
                    usage: Some(bake_ai::Usage {
                        input: 100,
                        total_tokens: 100,
                        cost: UsageCost {
                            input: 0.25,
                            total: 0.25,
                            ..UsageCost::default()
                        },
                        ..bake_ai::Usage::default()
                    }),
                    ..AgentToolResult::text("ran")
                })
            })
        },
    )
}

type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

struct Fixture {
    dir: TempDir,
    h: Harness,
    requests: Requests,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.join("ws")).expect("ws");
        Self {
            dir,
            h: harness(),
            requests: Arc::default(),
        }
    }

    fn ws(&self) -> PathBuf {
        self.dir.join("ws")
    }

    /// A step that records the request and answers `message`.
    fn recording(&self, message: bake_ai::AssistantMessage) -> FauxResponseStep {
        let requests = Arc::clone(&self.requests);
        FauxResponseStep::from_fn(move |context, _, _, _| {
            requests.lock().unwrap().push(context.clone());
            Ok(message.clone())
        })
    }

    /// The model calls `codemode` with `code` as `c<turn>`, then answers
    /// "done".
    fn script(&self, code: &str, id: &str) {
        self.h.faux.append_responses(vec![
            self.recording(faux_assistant_blocks(
                vec![faux_tool_call(
                    "codemode",
                    json!({ "code": code }),
                    Some(id),
                )],
                StopReason::ToolUse,
            )),
            self.recording(faux_assistant_message("done")),
        ]);
    }

    /// A persisted session over `tools`, of which `active` are active, with
    /// `settings` and the prompts of `prompts`.
    async fn session_with(
        &self,
        tools: Vec<Arc<AgentTool>>,
        prompts: Vec<ToolPrompt>,
        active: &[&str],
        settings: Settings,
    ) -> AgentSession {
        let ws = self.ws();
        let store = SessionManager::create(
            &ws.to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("store");
        self.open(store, tools, prompts, active, settings).await
    }

    async fn open(
        &self,
        store: SessionManager,
        tools: Vec<Arc<AgentTool>>,
        prompts: Vec<ToolPrompt>,
        active: &[&str],
        settings: Settings,
    ) -> AgentSession {
        let mut options = options(&self.h, store, &self.ws().to_string_lossy());
        options.tools = tools;
        options.tool_prompts = prompts;
        options.active_tool_names = active.iter().map(|name| (*name).to_owned()).collect();
        options.settings = settings;
        AgentSession::create(options).await.expect("session")
    }

    async fn session(&self, tools: Vec<AgentTool>, active: &[&str]) -> AgentSession {
        self.session_with(
            tools.into_iter().map(Arc::new).collect(),
            Vec::new(),
            active,
            Settings::default(),
        )
        .await
    }
}

async fn prompt(session: &AgentSession) {
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt settles")
        .expect("prompt");
}

/// The last `codemode` result.
fn codemode_result(session: &AgentSession) -> bake_ai::ToolResultMessage {
    session
        .messages()
        .iter()
        .rev()
        .find_map(|message| {
            message
                .as_tool_result()
                .filter(|result| result.tool_name == "codemode")
                .cloned()
        })
        .expect("a codemode result")
}

/// The output after the script header, which is checked on the way.
fn result_text(result: &bake_ai::ToolResultMessage) -> String {
    let (header, items) = result.content.split_first().expect("a header");
    let UserContentBlock::Text(header) = header else {
        panic!("no header");
    };
    let lines: Vec<&str> = header.text.split('\n').collect();
    assert!(
        matches!(lines[0], "Script completed" | "Script failed"),
        "{}",
        header.text
    );
    assert!(lines[1].starts_with("Wall time ") && lines[1].ends_with(" seconds"));
    assert_eq!(&lines[2..], ["Output:", ""]);
    items
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => "<image>".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn calls(result: &bake_ai::ToolResultMessage) -> Vec<(String, String, String)> {
    result.details.as_ref().expect("details")["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|call| {
            (
                call["id"].as_str().unwrap_or_default().to_owned(),
                call["name"].as_str().unwrap_or_default().to_owned(),
                call["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn store_entries(session: &AgentSession) -> Vec<Value> {
    lock(&session.session)
        .branch_entries(None)
        .iter()
        .filter(|entry| {
            entry.entry_type() == "custom"
                && entry.get("customType").and_then(Value::as_str)
                    == Some(CODEMODE_STORE_ENTRY_TYPE)
        })
        .filter_map(|entry| entry.get("data").cloned())
        .collect()
}

fn read_definition(ws: &std::path::Path) -> ToolDefinition {
    create_all_tool_definitions(ws, &ToolsOptions::default())
        .into_iter()
        .find(|definition| definition.name() == "read")
        .expect("read")
}

fn declared(context: &TranscriptContext) -> Vec<bake_ai::Tool> {
    get_current_tools(context.messages())
}

/// Without `codemode` active, the session sends what it sent before code
/// mode: the active tools' own declarations and no codemode rule, although
/// the session registers the tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_without_active_codemode_are_unchanged() {
    let fx = Fixture::new("inactive");
    let read = read_definition(&fx.ws());
    let tools = vec![Arc::new(read.tool.clone()), Arc::new(echo())];
    let expected: Vec<bake_ai::Tool> = tools.iter().map(|tool| tool.declaration()).collect();
    let session = fx
        .session_with(
            tools,
            vec![read.prompt()],
            &["read", "echo"],
            Settings::default(),
        )
        .await;
    assert!(session.tools.iter().any(|tool| tool.name == "codemode"));
    fx.h.faux
        .append_responses(vec![fx.recording(faux_assistant_message("ok"))]);
    prompt(&session).await;
    let requests = fx.requests.lock().unwrap().clone();
    session.shutdown().await;
    assert_eq!(declared(&requests[0]), expected);
    let prompt = get_current_system_prompt(requests[0].messages());
    assert!(!prompt.contains("Use codemode"), "{prompt}");
    assert!(!prompt.contains("Codemode:"), "{prompt}");
    assert!(prompt.contains("Use read to examine files instead of cat or sed."));
}

/// Pi's "presents callable tools per codemode.mode", through the session:
/// with `on` declared tools say how scripts call them; with `only` they are
/// listed in `codemode` and left out of requests and the rules, while the
/// transcript still declares them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presents_callable_tools_per_codemode_mode() {
    for mode in [CodemodeMode::On, CodemodeMode::Only] {
        let fx = Fixture::new("codemode-mode");
        let read = read_definition(&fx.ws());
        let settings = Settings {
            codemode_mode: mode,
            ..Settings::default()
        };
        let session = fx
            .session_with(
                vec![Arc::new(read.tool.clone()), Arc::new(echo())],
                vec![read.prompt()],
                &["read", "echo", "codemode"],
                settings,
            )
            .await;
        fx.h.faux
            .append_responses(vec![fx.recording(faux_assistant_message("ok"))]);
        prompt(&session).await;
        let requests = fx.requests.lock().unwrap().clone();
        let transcript: Vec<String> = {
            let messages: Vec<bake_ai::Message> = session
                .messages()
                .iter()
                .filter_map(|message| message.as_system().cloned().map(bake_ai::Message::System))
                .collect();
            get_current_tools(&messages)
                .into_iter()
                .map(|tool| tool.name)
                .collect()
        };
        session.shutdown().await;
        let tools = declared(&requests[0]);
        let description = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| tool.description.clone())
        };
        let system = get_current_system_prompt(requests[0].messages());
        assert_eq!(transcript, ["read", "echo", "codemode"]);
        match mode {
            CodemodeMode::On => {
                let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
                assert_eq!(names, ["read", "echo", "codemode"]);
                assert_eq!(
                    description("echo").unwrap(),
                    "Echo text back.\n\nSecond paragraph.\n\nCodemode: `tools.echo(args)` resolves to a string."
                );
                assert!(
                    description("read")
                        .unwrap()
                        .contains("Codemode: `tools.read(args)` resolves to")
                );
                assert!(!description("codemode").unwrap().contains("Nested tools:"));
                assert!(system.contains("Use read to examine files"));
                assert!(system.contains("Use codemode to batch independent tool calls"));
            }
            CodemodeMode::Only => {
                let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
                assert_eq!(names, ["codemode"]);
                let codemode = description("codemode").unwrap();
                assert!(codemode.contains("### `echo`"));
                assert!(codemode.contains("### `read`"));
                assert!(codemode.contains("- Use read to examine files instead of cat or sed."));
                // Hidden tools' guidelines move to their codemode sections.
                assert!(!system.contains("Use read to examine files"));
                assert!(system.contains("Use codemode to batch independent tool calls"));
            }
        }
    }
}

/// Pi's "runs nested calls in parallel and returns only the script result".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_nested_calls_in_parallel_and_returns_only_the_script_result() {
    let fx = Fixture::new("codemode-parallel");
    let session = fx
        .session(
            vec![echo(), stats(), screenshot()],
            &["echo", "stats", "screenshot", "codemode"],
        )
        .await;
    fx.script(
        r#"
            const [a, b, stats] = await Promise.all([
                tools.echo({ text: "one" }),
                tools.echo({ text: "two" }),
                tools.stats({}),
            ]);
            console.log("files", stats.files);
            text(ALL_TOOLS.map((tool) => tool.name).join(","));
            return { a, b, names: stats.names };
        "#,
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    let tool_results = session
        .messages()
        .iter()
        .filter(|message| message.as_tool_result().is_some())
        .count();
    session.shutdown().await;
    assert!(!result.is_error);
    assert_eq!(
        result_text(&result),
        "==> text 1/2 <==\necho,stats,screenshot\n==> text 2/2 <==\n{\"a\":\"echo: one\",\"b\":\"echo: two\",\"names\":[\"a\",\"b\"]}\n<console_output>\nfiles 2\n</console_output>"
    );
    let calls = calls(&result);
    assert_eq!(
        calls
            .iter()
            .map(|(_, name, status)| (name.as_str(), status.as_str()))
            .collect::<Vec<_>>(),
        [("echo", "ok"), ("echo", "ok"), ("stats", "ok")]
    );
    assert!(calls.iter().all(|(id, _, _)| id.starts_with("c1/")));
    // Nested calls never become transcript tool results.
    assert_eq!(tool_results, 1);
    // The loop's own record of the nested calls is on the result too.
    assert_eq!(
        result
            .nested_calls
            .as_ref()
            .map(|nested| nested.calls.len()),
        Some(3)
    );
}

fn slow() -> AgentTool {
    AgentTool::new(
        "slow",
        "Slow",
        "Answer after a while",
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(AgentToolResult::text("slow"))
            })
        },
    )
}

/// Nested ids, the result's `nested_calls`, and the details follow the
/// order the script made its calls, although the later call finishes first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_calls_are_numbered_in_the_order_the_script_makes_them() {
    let fx = Fixture::new("codemode-order");
    let session = fx
        .session(vec![slow(), echo()], &["slow", "echo", "codemode"])
        .await;
    fx.script(
        "return await Promise.all([tools.slow({}), tools.echo({ text: \"fast\" })]);",
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    assert_eq!(result_text(&result), "[\"slow\",\"echo: fast\"]");
    assert_eq!(
        calls(&result),
        [
            ("c1/1".to_owned(), "slow".to_owned(), "ok".to_owned()),
            ("c1/2".to_owned(), "echo".to_owned(), "ok".to_owned()),
        ]
    );
    let recorded: Vec<(String, String)> = result
        .nested_calls
        .expect("nested calls")
        .calls
        .into_iter()
        .map(|record| (record.id, record.name))
        .collect();
    assert_eq!(
        recorded,
        [
            ("c1/1".to_owned(), "slow".to_owned()),
            ("c1/2".to_owned(), "echo".to_owned()),
        ]
    );
}

/// Pi's "routes nested calls through extension hooks", with a plugin veto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_nested_calls_through_plugin_vetoes() {
    let fx = Fixture::new("codemode-veto");
    let guard = Arc::new(Scripted::new("guard", &["echo"], block("echo is blocked")));
    let mut plugins = PluginRegistry::new();
    plugins
        .register(Arc::clone(&guard) as Arc<dyn crate::plugins::Plugin>, 0)
        .expect("register");
    let store = SessionManager::create(
        &fx.ws().to_string_lossy(),
        Some(&fx.dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("store");
    let mut options = options(&fx.h, store, &fx.ws().to_string_lossy());
    options.tools = vec![Arc::new(echo()), Arc::new(stats())];
    options.active_tool_names = ["echo", "stats", "codemode"].map(str::to_owned).to_vec();
    options.plugins = plugins;
    let session = AgentSession::create(options).await.expect("session");
    fx.script(
        r#"
            let blocked;
            try {
                await tools.echo({ text: "forbidden" });
            } catch (error) {
                blocked = error.message;
            }
            const stats = await tools.stats({});
            return { blocked, stats };
        "#,
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    assert_eq!(guard.calls(), ["echo:c1/1"]);
    assert_eq!(
        serde_json::from_str::<Value>(&result_text(&result)).unwrap(),
        json!({ "blocked": "echo is blocked", "stats": { "files": 2, "names": ["a", "b"] } })
    );
    assert_eq!(
        calls(&result)
            .iter()
            .map(|(_, _, status)| status.as_str())
            .collect::<Vec<_>>(),
        ["error", "ok"]
    );
}

/// Pi's "adds the usage of nested results to the codemode result".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adds_the_usage_of_nested_results_to_the_codemode_result() {
    let fx = Fixture::new("codemode-usage");
    let session = fx
        .session(vec![billed(), echo()], &["billed", "echo", "codemode"])
        .await;
    fx.script(
        "await tools.billed({}); await tools.billed({}); await tools.echo({ text: \"x\" });",
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    let usage = result.usage.expect("usage");
    assert_eq!((usage.input, usage.total_tokens), (200, 200));
    assert!((usage.cost.total - 0.5).abs() < 1e-9);
}

/// Pi's "attaches only the images the script passes to image()".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attaches_only_the_images_the_script_passes_to_image() {
    let fx = Fixture::new("codemode-images");
    let session = fx
        .session(vec![screenshot()], &["screenshot", "codemode"])
        .await;
    fx.script(
        &format!(
            "const shot = await tools.screenshot({{}});\ntext(shot);\nimage(\"data:image/png;base64,{TINY_PNG_BASE64}\");\nimage(\"data:image/png;base64,{TINY_PNG_BASE64}\");\ntext(\"after\");"
        ),
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    let text = result_text(&result);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        lines,
        [
            "==> text 1/2 <==",
            "captured",
            lines[2],
            "<image>",
            lines[2],
            "<image>",
            "==> text 2/2 <==",
            "after"
        ]
    );
    let path = lines[2]
        .strip_prefix("[Image saved to ")
        .and_then(|rest| rest.split(" (image/png, ").next())
        .expect("a saved-image label");
    assert_eq!(
        std::fs::read(path).expect("saved"),
        crate::mcp::tools::decode_base64(TINY_PNG_BASE64)
    );
    let path = std::path::Path::new(path);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir(path.parent().expect("parent"));
}

/// Pi's "resolves bash calls to structured results" and "resolves read calls
/// to text for text files and to image blocks that image() shows".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_bash_and_read_calls_as_pi_documents() {
    let fx = Fixture::new("codemode-bash-read");
    std::fs::write(fx.ws().join("notes.txt"), "hello").expect("notes");
    std::fs::write(
        fx.ws().join("pixel.png"),
        crate::mcp::tools::decode_base64(TINY_PNG_BASE64),
    )
    .expect("pixel");
    let bash = create_bash_tool_definition(
        &fx.ws(),
        BashToolOptions {
            spill_directory: Some(fx.dir.path().to_path_buf()),
            ..BashToolOptions::default()
        },
    );
    let read = read_definition(&fx.ws());
    let session = fx
        .session_with(
            vec![Arc::new(bash.tool.clone()), Arc::new(read.tool.clone())],
            vec![bash.prompt(), read.prompt()],
            &["bash", "read", "codemode"],
            Settings::default(),
        )
        .await;
    fx.script(
        "const r = await tools.bash({ command: \"echo out; exit 3\" });\ntext(JSON.stringify([r.output, r.exit_code, typeof r.wall_time_seconds]));\ntext(await tools.read({ path: \"notes.txt\" }));\nconst shot = await tools.read({ path: \"pixel.png\" });\ntext(shot.note);\nimage(shot);",
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    assert!(!result.is_error, "{}", result_text(&result));
    let text = result_text(&result);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        &lines[..6],
        [
            "==> text 1/3 <==",
            "[\"out\\n\",3,\"number\"]",
            "==> text 2/3 <==",
            "hello",
            "==> text 3/3 <==",
            "Read image file [image/png]",
        ]
    );
    assert!(lines[6].starts_with("[Image saved to "), "{text}");
    assert_eq!(lines[7], "<image>");
    let path = lines[6]
        .strip_prefix("[Image saved to ")
        .and_then(|rest| rest.split(" (image/png, ").next())
        .expect("a saved-image label");
    let path = std::path::Path::new(path);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir(path.parent().expect("parent"));
}

/// Pi's "reports script failures as results that keep partial output and
/// the calls that ran".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_failures_with_partial_output_and_the_calls_that_ran() {
    let fx = Fixture::new("codemode-failure");
    let session = fx.session(vec![echo()], &["echo", "codemode"]).await;
    fx.script(
        "text(\"partial\");\nconsole.log(\"log\");\nawait tools.echo({ text: \"x\" });\nthrow new Error(\"boom\");",
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    assert!(result.is_error);
    let text = result_text(&result);
    assert!(
        text.starts_with(
            "partial\n<console_output>\nlog\n</console_output>\nScript error:\nError: boom\n"
        ),
        "{text}"
    );
    assert!(text.contains("codemode.js:4"), "{text}");
    assert!(text.contains("Tool calls made before the failure (they are not undone): echo (ok)"));
}

/// A script cannot reach `codemode` itself: it is not on `tools`, not in
/// `ALL_TOOLS`, and the nested executor does not run it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codemode_is_not_callable_from_a_script() {
    let fx = Fixture::new("codemode-self");
    let session = fx.session(vec![echo()], &["echo", "codemode"]).await;
    fx.script(
        "let missing;\ntry { tools.codemode; } catch (error) { missing = error.message.split(\". \")[0]; }\nreturn [missing, ALL_TOOLS.map((tool) => tool.name), await describeTool(\"codemode\") === undefined];",
        "c1",
    );
    prompt(&session).await;
    let result = codemode_result(&session);
    session.shutdown().await;
    assert_eq!(
        result_text(&result),
        "[\"tools.codemode does not exist\",[\"echo\"],true]"
    );
}

const INCREMENT: &str =
    "const next = (load(\"count\") ?? 0) + 1;\nstore(\"count\", next);\nreturn next;";

/// Pi's "persists store() writes as custom entries for later calls" and
/// "appends nothing for failed scripts or scripts without writes".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persists_store_writes_as_custom_entries() {
    let fx = Fixture::new("codemode-store");
    let session = fx.session(Vec::new(), &["codemode"]).await;
    fx.script("store(\"count\", 5);\nthrow new Error(\"boom\");", "c0");
    prompt(&session).await;
    assert!(codemode_result(&session).is_error);
    fx.script("return load(\"count\") ?? \"missing\";", "c00");
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "missing");
    assert!(store_entries(&session).is_empty());

    fx.script(INCREMENT, "c1");
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "1");
    fx.script(INCREMENT, "c2");
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "2");
    assert_eq!(
        store_entries(&session),
        [
            json!({ "set": { "count": 1 }, "delete": [] }),
            json!({ "set": { "count": 2 }, "delete": [] }),
        ]
    );
    fx.script(
        "store(\"count\", undefined);\nreturn load(\"count\") === undefined;",
        "c3",
    );
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "true");
    assert_eq!(
        store_entries(&session).last(),
        Some(&json!({ "set": {}, "delete": ["count"] }))
    );
    // The entries are in the file, so a resumed session reads them.
    let file = session.session_file().expect("file");
    session.shutdown().await;
    let resumed = fx
        .open(
            SessionManager::open(&file, None, None).expect("open"),
            Vec::new(),
            Vec::new(),
            &["codemode"],
            Settings::default(),
        )
        .await;
    fx.script("return load(\"count\") ?? \"deleted\";", "c4");
    prompt(&resumed).await;
    assert_eq!(result_text(&codemode_result(&resumed)), "deleted");
    resumed.shutdown().await;
}

/// Pi's "loads the values written on the current branch", through `/tree`
/// and a fork.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_the_values_written_on_the_current_branch() {
    let fx = Fixture::new("codemode-branch");
    let session = fx.session(Vec::new(), &["codemode"]).await;
    fx.script(INCREMENT, "c1");
    prompt(&session).await;
    let first_prompt = lock(&session.session)
        .branch_entries(None)
        .iter()
        .find(|entry| {
            entry.entry_type() == "message"
                && entry.get("message").and_then(|message| message.get("role"))
                    == Some(&json!("user"))
        })
        .map(|entry| entry.id().to_owned())
        .expect("first prompt");
    fx.script(INCREMENT, "c2");
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "2");

    // A fork at the second result keeps both writes on its path.
    let leaf = session.leaf_id().expect("leaf");
    let forked = session
        .fork_session_manager(&leaf, navigation::ForkPosition::At)
        .await
        .expect("fork");
    assert_eq!(
        Value::Object(crate::tools::codemode::read_codemode_store(
            &forked.session.branch_entries(None)
        )),
        json!({ "count": 2 })
    );

    // `/tree` back to the first prompt: the writes after it are on another
    // path.
    session
        .navigate_tree(&first_prompt, TreeNavigationOptions::default(), None)
        .await
        .expect("navigate");
    fx.script(INCREMENT, "c3");
    prompt(&session).await;
    assert_eq!(result_text(&codemode_result(&session)), "1");
    session.shutdown().await;
}

/// How the fixture's answerer answers.
struct Answerer {
    outcome: EscalationOutcome,
    asked: Arc<Mutex<Vec<ApprovalRequest>>>,
}

impl Approver for Answerer {
    fn request(&self, request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.asked.lock().unwrap().push(request);
        Box::pin(std::future::ready(self.outcome))
    }
}

/// A nested `write` from a script meets the session's preset: under
/// `read-only` with `ask`, the escalation is asked for the nested call by
/// its own id, naming the script's call, and the answer decides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_calls_meet_the_preset_and_its_approvals() {
    for (outcome, written) in [
        (EscalationOutcome::AllowedOnce, true),
        (EscalationOutcome::Rejected, false),
    ] {
        let fx = Fixture::new("codemode-approval");
        let ws = fx.ws();
        let asked: Arc<Mutex<Vec<ApprovalRequest>>> = Arc::default();
        let standing = StandingPermissions::new(
            SandboxMode::ReadOnly,
            ApprovalPolicy::Ask,
            "test",
            &ws,
            None,
        );
        let permissions = ToolPermissions::new(
            Arc::new(standing),
            Arc::new(Answerer {
                outcome,
                asked: Arc::clone(&asked),
            }),
            Arc::new(RecordingProvider::default()) as Arc<dyn SandboxProvider>,
        );
        let work = crate::owned_work::WorkGate::default();
        let tools: Vec<Arc<AgentTool>> = owned_sandbox_file_tools(
            create_all_tool_definitions(&ws, &ToolsOptions::default())
                .into_iter()
                .filter(|definition| definition.name() == "write")
                .collect(),
            &ws,
            &permissions,
            Some(work.clone()),
        )
        .into_iter()
        .map(|definition| Arc::new(definition.tool))
        .collect();
        let store = SessionManager::create(
            &ws.to_string_lossy(),
            Some(&fx.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("store");
        let mut options = options(&fx.h, store, &ws.to_string_lossy());
        options.tools = tools;
        options.active_tool_names = ["write", "codemode"].map(str::to_owned).to_vec();
        options.work = Some(work);
        let session = AgentSession::create(options).await.expect("session");
        session
            .record_approvals(permissions.engine.recorder_slot())
            .expect("record approvals");
        fx.script(
            "try { return await tools.write({ path: \"a.txt\", content: \"x\", sandbox_permissions: \"workspace-write\", justification: \"the script writes its output\" }); } catch (error) { return \"refused: \" + error.message; }",
            "c1",
        );
        prompt(&session).await;
        let result = codemode_result(&session);
        session.shutdown().await;
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "{outcome:?}");
        assert_eq!(asked[0].call_id, "c1/1");
        assert_eq!(asked[0].parent_tool_call_id.as_deref(), Some("c1"));
        assert_eq!(ws.join("a.txt").exists(), written, "{outcome:?}");
        let text = result_text(&result);
        assert_eq!(text.starts_with("refused: "), !written, "{text}");
    }
}

/// Esc during a script cancels its nested `bash`, whose process tree is
/// gone once the result is written; the result says the script was
/// aborted.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_mid_script_cancels_and_reaps_a_nested_bash() {
    let fx = Fixture::new("codemode-abort");
    let pid_file = fx.ws().join("pid");
    let work = crate::owned_work::WorkGate::default();
    let bash = create_bash_tool_definition(
        &fx.ws(),
        BashToolOptions {
            spill_directory: Some(fx.dir.path().to_path_buf()),
            owner: Some(work.clone()),
            ..BashToolOptions::default()
        },
    );
    let store = SessionManager::create(
        &fx.ws().to_string_lossy(),
        Some(&fx.dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("store");
    let mut options = options(&fx.h, store, &fx.ws().to_string_lossy());
    options.tools = vec![Arc::new(bash.tool)];
    options.active_tool_names = ["bash", "codemode"].map(str::to_owned).to_vec();
    options.work = Some(work);
    let session = Arc::new(AgentSession::create(options).await.expect("session"));
    fx.script(
        "await tools.bash({ command: \"echo $$ > pid; sleep 30; echo late > late\" });\nreturn \"finished\";",
        "c1",
    );
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    let deadline = std::time::Instant::now() + WAIT;
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            break pid;
        }
        assert!(std::time::Instant::now() < deadline, "bash never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let started = std::time::Instant::now();
    session.abort().await;
    let _ = tokio::time::timeout(WAIT, running)
        .await
        .expect("prompt settles");
    assert!(started.elapsed() < Duration::from_secs(20));
    let result = codemode_result(&session);
    session.shutdown().await;
    assert!(result.is_error);
    let text = result_text(&result);
    assert!(
        text.contains("Script error:\nScript aborted: This operation was aborted"),
        "{text}"
    );
    assert_eq!(
        calls(&result)
            .iter()
            .map(|(id, name, status)| (id.as_str(), name.as_str(), status.as_str()))
            .collect::<Vec<_>>(),
        [("c1/1", "bash", "cancelled")]
    );
    let alive = process_exists(pid);
    assert!(!alive, "the nested bash {pid} survived the abort");
    assert!(!fx.ws().join("late").exists());
}

/// Whether process `pid` exists, through `kill -0`.
#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// An event's kind and tool, its parent call, and its partial details.
type SeenEvent = (String, Option<String>, Option<Value>);

/// While a script runs, its partial results reach the session's events with
/// the nested calls so far, and the nested calls' own events carry the
/// script's id as their parent, which the terminal skips.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_results_carry_the_nested_calls() {
    let fx = Fixture::new("codemode-updates");
    let session = fx.session(vec![echo()], &["echo", "codemode"]).await;
    let events: Arc<Mutex<Vec<SeenEvent>>> = Arc::default();
    let seen = Arc::clone(&events);
    let subscription = session.subscribe(move |event| {
        if let SessionEvent::Agent(event) = event {
            match event {
                AgentEvent::ToolExecutionUpdate {
                    tool_name,
                    partial_result,
                    parent_tool_call_id,
                    ..
                } => seen.lock().unwrap().push((
                    format!("update:{tool_name}"),
                    parent_tool_call_id.clone(),
                    partial_result.details.clone(),
                )),
                AgentEvent::ToolExecutionStart {
                    tool_name,
                    parent_tool_call_id,
                    ..
                } => seen.lock().unwrap().push((
                    format!("start:{tool_name}"),
                    parent_tool_call_id.clone(),
                    None,
                )),
                _ => {}
            }
        }
    });
    fx.script("return await tools.echo({ text: \"x\" });", "c1");
    prompt(&session).await;
    subscription.unsubscribe();
    session.shutdown().await;
    let events = events.lock().unwrap().clone();
    assert!(events.contains(&("start:echo".to_owned(), Some("c1".to_owned()), None)));
    let updates: Vec<&Value> = events
        .iter()
        .filter(|(kind, parent, _)| kind == "update:codemode" && parent.is_none())
        .filter_map(|(_, _, details)| details.as_ref())
        .collect();
    assert_eq!(updates.first(), Some(&&json!({ "calls": [] })));
    let last = updates.last().expect("updates");
    assert_eq!(last["calls"][0]["name"], "echo");
    assert_eq!(last["calls"][0]["status"], "ok");
    assert_eq!(last["calls"][0]["id"], "c1/1");
}
