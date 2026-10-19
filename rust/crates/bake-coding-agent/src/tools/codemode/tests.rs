//! Pi v1.3.0's codemode cases that need no session: the description
//! catalog of `test/tool-search.test.ts`, the loadout and option cases of
//! `test/suite/agent-session-codemode.test.ts`, `readCodemodeStore`, the
//! result layout, and `test/codemode-worker-admission.test.ts`, whose
//! workers are Bake's script threads. The session cases are in
//! `agent_session/codemode_tests.rs`.

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use bake_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolContext};
use bake_ai::UserContentBlock;
use bake_codemode::{Failure, OutputItem, ScriptError, ToolNamespace};
use serde_json::{Value, json};

use super::execute::{
    Admission, Item, execute_with, format_error, format_output, join_adjacent_text, save_images,
    truncate_output,
};
use super::*;
use crate::session::SessionEntry;

const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn tool(name: &str, description: &str) -> Arc<AgentTool> {
    Arc::new(AgentTool::new(
        name,
        name,
        description,
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| Box::pin(async { Ok(AgentToolResult::default()) }),
    ))
}

fn namespace(
    name: &str,
    description: Option<&str>,
    instructions: Option<&str>,
) -> CodemodeNamespace {
    CodemodeNamespace {
        namespace: ToolNamespace {
            name: name.to_owned(),
            description: description.map(str::to_owned),
            instructions: instructions.map(str::to_owned),
        },
        aliases: Vec::new(),
    }
}

/// `test/tool-search.test.ts`' catalog: a plain tool, three GitHub tools,
/// and two docs tools, one of them long.
struct Catalog {
    plain: Arc<AgentTool>,
    github: Vec<Arc<AgentTool>>,
    docs: Vec<Arc<AgentTool>>,
    namespaces: HashMap<String, CodemodeNamespace>,
}

impl Catalog {
    fn new() -> Self {
        let plain = tool("read_notes", "Read notes.");
        let github: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|suffix| {
                tool(
                    &format!("mcp__github__{suffix}"),
                    &format!("GitHub {suffix}."),
                )
            })
            .collect();
        let docs = vec![
            tool("mcp__docs__search", "Search docs."),
            tool("mcp__docs__long", &"Long ".repeat(200)),
        ];
        let mut namespaces = HashMap::new();
        for entry in &github {
            namespaces.insert(
                entry.name.clone(),
                namespace("mcp__github", Some("GitHub server"), None),
            );
        }
        for entry in &docs {
            namespaces.insert(entry.name.clone(), namespace("mcp__docs", None, None));
        }
        Self {
            plain,
            github,
            docs,
            namespaces,
        }
    }

    fn all(&self) -> Vec<Arc<AgentTool>> {
        let mut all = vec![Arc::clone(&self.plain)];
        all.extend(self.github.iter().cloned());
        all.extend(self.docs.iter().cloned());
        all
    }

    fn options(&self, inline_budget: Option<f64>) -> CodemodeDescriptionOptions {
        CodemodeDescriptionOptions {
            namespaces: self.namespaces.clone(),
            inline_budget,
            ..CodemodeDescriptionOptions::default()
        }
    }
}

#[test]
fn lists_everything_without_a_budget() {
    let catalog = Catalog::new();
    let description = create_codemode_description(&catalog.all(), &catalog.options(None));
    assert!(description.contains("Nested tools:"));
    assert!(description.contains("## mcp__github\nGitHub server"));
    assert!(description.contains("## mcp__docs\n\n### `mcp__docs"));
    assert!(description.contains("find unlisted tools, such as MCP tools"));
    assert!(description.contains("`await searchTools(query, { limit?, namespace? })`"));
    assert!(description.contains("`await describeTool(name)`"));
    assert!(description.contains("`await describeNamespace(name)`"));
}

#[test]
fn fills_the_budget_round_robin_cheapest_first_and_says_what_is_missing() {
    let catalog = Catalog::new();
    let description = create_codemode_description(&catalog.all(), &catalog.options(Some(170.0)));
    assert!(description.contains("### `read_notes`"));
    assert!(description.contains("## mcp__docs (some tools not listed)"));
    assert!(description.contains("### `mcp__docs__search`"));
    assert!(!description.contains("### `mcp__docs__long`"));
    assert!(description.contains("## mcp__github (some tools not listed)"));
    assert!(description.contains("find unlisted tools, such as MCP tools"));
    assert_eq!(
        create_codemode_description(&catalog.all(), &catalog.options(Some(170.0))),
        description
    );
}

#[test]
fn leaves_deferred_tools_and_their_namespaces_out_entirely() {
    let catalog = Catalog::new();
    let mut options = catalog.options(None);
    options.deferred = catalog
        .github
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let description = create_codemode_description(&catalog.all(), &options);
    assert!(!description.contains("mcp__github"));
    let mut without = vec![Arc::clone(&catalog.plain)];
    without.extend(catalog.docs.iter().cloned());
    assert_eq!(
        description,
        create_codemode_description(&without, &catalog.options(None))
    );
}

#[test]
fn leaves_namespace_instructions_out() {
    let catalog = Catalog::new();
    let options = CodemodeDescriptionOptions {
        namespaces: catalog
            .github
            .iter()
            .map(|tool| {
                (
                    tool.name.clone(),
                    namespace("mcp__github", None, Some("Long usage guide.")),
                )
            })
            .collect(),
        ..CodemodeDescriptionOptions::default()
    };
    let description = create_codemode_description(&catalog.github, &options);
    assert!(description.contains("## mcp__github\n\n### `mcp__github__a`"));
    assert!(!description.contains("Long usage guide."));
}

#[test]
fn lists_only_namespaces_with_a_zero_budget() {
    let catalog = Catalog::new();
    let description = create_codemode_description(&catalog.all(), &catalog.options(Some(0.0)));
    assert!(description.contains("## mcp__docs (tools not listed)"));
    assert!(!description.contains("codemode tool declaration:"));
}

/// Without tools, the description is Pi's intro and globals with `models`
/// off, exactly.
#[test]
fn the_description_without_tools_is_pis_text() {
    let description = create_codemode_description(&[], &CodemodeDescriptionOptions::default());
    assert_eq!(
        description,
        [
            "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.",
            "- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.",
            "- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`",
            "",
            "Globals:",
            "- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.",
            "- `store(key, value)` and `load(key)` keep JSON values across codemode calls.",
            "- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.",
        ]
        .join("\n")
    );
    assert!(!description.contains("models"));
    let tool = create_codemode_tool(None);
    assert_eq!(tool.description, description);
    assert_eq!(tool.name, "codemode");
    assert_eq!(
        tool.parameters,
        json!({
            "type": "object",
            "required": ["code"],
            "properties": { "code": { "type": "string", "description": "Raw JavaScript source." } },
        })
    );
    assert_eq!(tool.replay, Some(bake_agent::ToolReplay::Never));
    assert!(!tool.nested_callable);
    assert!(tool.constrained_sampling.is_none());
    let prompt = codemode_tool_prompt();
    assert_eq!(
        prompt.snippet.as_deref(),
        Some("Run JavaScript that calls other tools")
    );
    assert_eq!(
        prompt.guidelines,
        [
            "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls."
        ]
    );
}

fn echo() -> Arc<AgentTool> {
    Arc::new(AgentTool::new(
        "echo",
        "Echo",
        "Echo text back.\n\nSecond paragraph.",
        json!({
            "type": "object",
            "required": ["text"],
            "properties": { "text": { "type": "string", "description": "Text to echo" } },
        }),
        |_id, _args, _signal, _update, _ctx| Box::pin(async { Ok(AgentToolResult::default()) }),
    ))
}

fn stats() -> Arc<AgentTool> {
    let mut stats = AgentTool::new(
        "stats",
        "Stats",
        "Return structured stats",
        json!({ "type": "object", "properties": {} }),
        |_id, _args, _signal, _update, _ctx| Box::pin(async { Ok(AgentToolResult::default()) }),
    );
    stats.output_schema = Some(json!({
        "type": "object",
        "required": ["files", "names"],
        "properties": {
            "files": { "type": "number" },
            "names": { "type": "array", "items": { "type": "string" } },
        },
    }));
    Arc::new(stats)
}

fn loadout(
    declared: &[Arc<AgentTool>],
    mode: CodemodeMode,
    budget: Option<f64>,
) -> ToolLoadoutChanges {
    let no_namespace = |_: &str| None;
    let guidelines = |name: &str| {
        if name == "read" {
            vec!["Use read to examine files instead of cat or sed.".to_owned()]
        } else {
            Vec::new()
        }
    };
    prepare_codemode_loadout(
        &ToolLoadout {
            declared,
            namespace: &no_namespace,
            guidelines: &guidelines,
        },
        mode,
        budget,
    )
}

fn description_of<'a>(changes: &'a ToolLoadoutChanges, name: &str) -> Option<&'a str> {
    changes
        .descriptions
        .iter()
        .find(|(tool, _)| tool == name)
        .map(|(_, description)| description.as_str())
}

/// Pi's "presents callable tools per codemode.mode".
#[test]
fn presents_callable_tools_per_codemode_mode() {
    let read = tool("read", "Read a file.");
    let declared = vec![read, echo(), Arc::new(create_codemode_tool(None))];

    // on: declared tools say how scripts call them and are not listed again.
    let on = loadout(&declared, CodemodeMode::On, None);
    let echo_description = description_of(&on, "echo").expect("echo");
    assert!(echo_description.contains("Codemode: `tools.echo(args)` resolves to"));
    assert!(!echo_description.contains("codemode tool declaration:"));
    assert!(
        !description_of(&on, "codemode")
            .unwrap()
            .contains("### `echo`")
    );
    assert!(on.hidden_declarations.is_empty());
    assert_eq!(
        echo_description,
        "Echo text back.\n\nSecond paragraph.\n\nCodemode: `tools.echo(args)` resolves to a string."
    );

    // only: codemode lists the tools, which requests leave out.
    let only = loadout(&declared, CodemodeMode::Only, None);
    assert!(description_of(&only, "echo").is_none());
    let codemode = description_of(&only, "codemode").unwrap();
    assert!(codemode.contains("### `echo`"));
    assert!(codemode.contains("### `read`"));
    // Hidden tools' guidelines move to their codemode sections.
    assert!(codemode.contains("- Use read to examine files instead of cat or sed."));
    assert_eq!(only.hidden_declarations, ["read", "echo"]);
}

/// Pi's #10343: with no budget left, `only` lists no declaration.
#[test]
fn a_zero_budget_lists_no_declaration() {
    let declared = vec![
        tool("read", "Read a file."),
        Arc::new(create_codemode_tool(None)),
    ];
    let only = loadout(&declared, CodemodeMode::Only, Some(0.0));
    assert!(
        !description_of(&only, "codemode")
            .unwrap()
            .contains("### `read`")
    );
    assert!(
        description_of(&only, "codemode")
            .unwrap()
            .contains("Nested tools:")
    );
}

/// A tool that may not be called from scripts is neither annotated,
/// listed, nor hidden.
#[test]
fn tools_scripts_cannot_call_keep_their_declarations() {
    let mut private = (*tool("private", "Private.")).clone();
    private.nested_callable = false;
    let declared = vec![Arc::new(private), Arc::new(create_codemode_tool(None))];
    let on = loadout(&declared, CodemodeMode::On, None);
    assert!(description_of(&on, "private").is_none());
    let only = loadout(&declared, CodemodeMode::Only, None);
    assert!(only.hidden_declarations.is_empty());
    assert!(
        !description_of(&only, "codemode")
            .unwrap()
            .contains("private")
    );
}

#[test]
fn describes_structured_results_by_their_fields() {
    assert_eq!(
        describe_script_call(&stats()),
        "Return structured stats\n\nCodemode: `tools.stats(args)` resolves to `{ files, names }`."
    );
    let mut bash = (*tool("bash", "Run a command.")).clone();
    bash.output_schema = Some(crate::tools::bash::bash_tool_output_schema());
    assert_eq!(
        describe_script_call(&bash),
        "Run a command.\n\nCodemode: `tools.bash(args)` resolves to `{ output, truncated, full_output_path?, exit_code, wall_time_seconds }`."
    );
    let mut dashed = (*tool("mcp__dev-radius__search", "Search.")).clone();
    dashed.output_schema = Some(json!({ "type": "array", "items": { "type": "string" } }));
    assert_eq!(
        describe_script_call(&dashed),
        "Search.\n\nCodemode: `tools.mcp__dev_radius__search(args)` resolves to `Array<string>`."
    );
}

fn custom(data: Value, custom_type: &str) -> SessionEntry {
    let Value::Object(raw) = json!({
        "type": "custom",
        "customType": custom_type,
        "data": data,
        "id": format!("{custom_type}-{data}"),
        "parentId": null,
        "timestamp": "1970-01-01T00:00:00.000Z",
    }) else {
        unreachable!()
    };
    SessionEntry::from_json(raw).expect("entry")
}

/// Pi's "folds store entries from the root, ignoring malformed data".
#[test]
fn folds_store_entries_from_the_root_ignoring_malformed_data() {
    let entries = [
        custom(
            json!({ "set": { "a": 1, "b": { "c": 2 } }, "delete": [] }),
            CODEMODE_STORE_ENTRY_TYPE,
        ),
        custom(
            json!({ "set": { "a": 3 }, "delete": ["b"] }),
            CODEMODE_STORE_ENTRY_TYPE,
        ),
        custom(json!({ "set": { "z": 1 } }), CODEMODE_STORE_ENTRY_TYPE),
        custom(
            json!({ "set": { "other": 1 }, "delete": [] }),
            "other-extension",
        ),
    ];
    let branch: Vec<&SessionEntry> = entries.iter().collect();
    assert_eq!(
        Value::Object(read_codemode_store(&branch)),
        json!({ "a": 3 })
    );
}

fn text(text: &str) -> OutputItem {
    OutputItem::Text {
        text: text.to_owned(),
        console: false,
    }
}

fn console(text: &str) -> OutputItem {
    OutputItem::Text {
        text: text.to_owned(),
        console: true,
    }
}

/// Pi's "marks where each text item starts and puts console lines last in
/// one text block".
#[test]
fn marks_text_items_and_puts_console_lines_last_in_one_block() {
    let items = join_adjacent_text(format_output(&[
        text("one\ntwo"),
        console("a"),
        console("b"),
        text("three\n"),
        text("4"),
    ]));
    assert_eq!(
        items,
        [Item::Text(
            "==> text 1/3 <==\none\ntwo\n==> text 2/3 <==\nthree\n==> text 3/3 <==\n4\n<console_output>\na\nb\n</console_output>".to_owned()
        )]
    );
    // One text item has no marker.
    assert_eq!(
        format_output(&[text("only")]),
        [Item::Text("only".to_owned())]
    );
}

#[test]
fn joins_text_around_images_on_their_own_lines() {
    let image = Item::Image(bake_ai::ImageContent {
        data: TINY_PNG_BASE64.to_owned(),
        mime_type: "image/png".to_owned(),
    });
    let items = join_adjacent_text(vec![
        Item::Text("a".to_owned()),
        Item::Text(String::new()),
        Item::Text("b\n".to_owned()),
        Item::Text("c".to_owned()),
        image.clone(),
        Item::Text("d".to_owned()),
    ]);
    assert_eq!(
        items,
        [
            Item::Text("a\nb\nc".to_owned()),
            image,
            Item::Text("d".to_owned())
        ]
    );
}

#[test]
fn formats_each_failure_kind_with_the_calls_that_ran() {
    let call = CodemodeNestedCall {
        id: "c/1".to_owned(),
        name: "echo".to_owned(),
        args: "{}".to_owned(),
        status: CodemodeNestedCallStatus::Ok,
        duration_ms: Some(1.0),
        error: None,
    };
    let script = Failure::Exception(ScriptError {
        name: Some("TypeError".to_owned()),
        message: "x is not a function".to_owned(),
        stack: None,
    });
    assert_eq!(
        format_error(&script, std::slice::from_ref(&call)),
        "TypeError: x is not a function\n\nTool calls made before the failure (they are not undone): echo (ok)"
    );
    assert_eq!(
        format_error(
            &Failure::Timeout {
                message: "Execution timed out after 5 ms".to_owned()
            },
            &[]
        ),
        "Script timed out: Execution timed out after 5 ms\n\nNo tool calls were made."
    );
    assert_eq!(
        format_error(
            &Failure::Aborted {
                message: CODEMODE_ABORT_MESSAGE.to_owned()
            },
            &[]
        ),
        "Script aborted: This operation was aborted\n\nNo tool calls were made."
    );
    assert_eq!(
        format_error(
            &Failure::Protocol {
                message: "broken".to_owned()
            },
            &[]
        ),
        "Script sandbox failed: broken\n\nNo tool calls were made."
    );
}

fn image_item() -> Item {
    Item::Image(bake_ai::ImageContent {
        data: TINY_PNG_BASE64.to_owned(),
        mime_type: "image/png".to_owned(),
    })
}

/// The path of an `[Image saved to <path> (image/png, …)]` label, checked
/// to hold the tiny PNG.
fn saved_image_path(label: &str) -> std::path::PathBuf {
    let path = label
        .strip_prefix("[Image saved to ")
        .and_then(|rest| rest.split(" (image/png, ").next())
        .expect("a saved-image label");
    let path = std::path::PathBuf::from(path);
    assert_eq!(
        std::fs::read(&path).expect("saved image"),
        crate::mcp::tools::decode_base64(TINY_PNG_BASE64)
    );
    path
}

fn remove_output(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Pi's saved images: each image follows its path, and one shown twice is
/// saved once.
#[tokio::test]
async fn saves_each_image_once_and_names_it_before_the_image() {
    let items = save_images(
        vec![Item::Text("a".to_owned()), image_item(), image_item()],
        None,
    )
    .await;
    assert_eq!(items.len(), 5);
    let (Item::Text(first), Item::Text(second)) = (&items[1], &items[3]) else {
        panic!("labels before the images: {items:?}");
    };
    assert_eq!(first, second);
    assert!(first.ends_with(" (image/png, 70B)]"), "{first}");
    assert_eq!(items[2], image_item());
    let path = saved_image_path(first);
    remove_output(&path);
}

/// Pi's "truncates output to the token budget and spills the full text".
#[tokio::test]
async fn truncates_output_to_the_token_budget_and_spills_the_full_text() {
    let output: Vec<OutputItem> = (0..100).map(|i| text(&format!("row {i}"))).collect();
    let mut items = format_output(&output);
    items.push(image_item());
    let (items, path) = truncate_output(join_adjacent_text(items), 30, None).await;
    let path = path.expect("a spill file");
    let Item::Text(text) = &items[0] else {
        panic!("text first: {items:?}");
    };
    assert!(text.starts_with("Warning: truncated output (original token count: "));
    assert!(text.contains("tokens truncated"));
    assert!(text.ends_with(&format!(
        "\n\n[Full output: {path} (read with offset/limit)]"
    )));
    assert!(!text.contains("row 50\n"));
    assert_eq!(items[1], image_item());
    assert_eq!(items.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&path).expect("spill"),
        (0..100)
            .map(|i| format!("==> text {}/100 <==\nrow {i}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
    remove_output(std::path::Path::new(&path));

    let (small, path) =
        truncate_output(vec![Item::Text("{\"ok\":true}".to_owned())], 10, None).await;
    assert!(path.is_none());
    assert_eq!(small, [Item::Text("{\"ok\":true}".to_owned())]);
}

/// The text of each block after the header, images as `<image>`, and the
/// header checked.
fn result_text(result: &AgentToolResult) -> String {
    let (header, items) = result.content.split_first().expect("a header");
    let UserContentBlock::Text(header) = header else {
        panic!("no header");
    };
    let lines: Vec<&str> = header.text.split('\n').collect();
    assert!(
        matches!(lines.first(), Some(&("Script completed" | "Script failed"))),
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

async fn run_detached(code: &str) -> Result<AgentToolResult, String> {
    execute_with(
        "direct".to_owned(),
        json!({ "code": code }),
        None,
        AgentToolUpdateCallback::ignore(),
        ToolContext::detached(),
        None,
        Admission::process(),
    )
    .await
}

/// Pi's "runs without a session, starting from an empty store".
#[tokio::test]
async fn runs_without_a_session_starting_from_an_empty_store() {
    let result = run_detached(
        "const next = (load(\"count\") ?? 0) + 1;\nstore(\"count\", next);\nreturn next;",
    )
    .await
    .expect("result");
    assert!(!result.is_error);
    assert_eq!(result_text(&result), "1");
    assert_eq!(result.details, Some(json!({ "calls": [] })));
}

/// A host whose store cannot be written.
struct UnwritableStore;

impl CodemodeHost for UnwritableStore {
    fn tool_namespace(&self, _name: &str) -> Option<CodemodeNamespace> {
        None
    }

    fn tool_guidelines(&self, _name: &str) -> Vec<String> {
        Vec::new()
    }

    fn tool_description(&self, _name: &str) -> Option<String> {
        None
    }

    fn load_store(&self) -> BoxFuture<'static, Result<bake_codemode::StoreValues, String>> {
        Box::pin(async { Ok(bake_codemode::StoreValues::new()) })
    }

    fn append_store(
        &self,
        _data: CodemodeStoreEntryData,
    ) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Err("disk full".to_owned()) })
    }

    fn work(&self) -> Option<crate::owned_work::WorkGate> {
        None
    }
}

/// A store write that fails keeps the script's result and says so after
/// its output.
#[tokio::test]
async fn a_failed_store_write_keeps_the_result() {
    let result = execute_with(
        "c".to_owned(),
        json!({ "code": "store(\"count\", 1);\ntext(\"stored\");\nreturn 1;" }),
        None,
        AgentToolUpdateCallback::ignore(),
        ToolContext::detached(),
        Some(Arc::new(UnwritableStore)),
        Admission::process(),
    )
    .await
    .expect("result");
    assert!(!result.is_error);
    assert_eq!(
        result_text(&result),
        "==> text 1/2 <==\nstored\n==> text 2/2 <==\n1\n[The codemode store could not be written: disk full]"
    );
}

/// Pi's "applies the timeout_ms option and rejects invalid options".
#[tokio::test]
async fn applies_the_timeout_option_and_rejects_invalid_options() {
    let timed_out = run_detached("// @options: {\"timeout_ms\": 200}\nwhile (true) {}")
        .await
        .expect("result");
    assert!(timed_out.is_error);
    assert!(result_text(&timed_out).contains("Script error:\nScript timed out"));

    assert_eq!(
        run_detached("// @options: {\"yield\": 1}\ntext(1)").await,
        Err("@options only supports `max_output_tokens` and `timeout_ms`; got `yield`".to_owned())
    );
    assert_eq!(
        run_detached("// count\n// @options: {\"timeout_ms\": 200}\nwhile (true) {}").await,
        Err(
            "The // @options: line must be the first line of the input; found one on line 2"
                .to_owned()
        )
    );
}

/// Pi's "limits script memory so runaway allocations fail inside the
/// script".
#[tokio::test]
async fn limits_script_memory_so_runaway_allocations_fail_inside_the_script() {
    let result = run_detached(
        "// @options: {\"timeout_ms\": 30000}\nlet a = [];\ntry { while (true) a.push(\"x\".repeat(1 << 20) + a.length); } catch (error) { const n = a.length; a = null; return { n, error: String(error) }; }",
    )
    .await
    .expect("result");
    assert!(!result.is_error, "{}", result_text(&result));
    let value: Value = serde_json::from_str(&result_text(&result)).expect("json");
    assert!(value["error"].as_str().unwrap().contains("out of memory"));
    assert!(value["n"].as_u64().unwrap() < 512);
}

/// Pi's "formats output at the item limit in linear time".
#[tokio::test]
async fn formats_output_at_the_item_limit_in_linear_time() {
    let started = std::time::Instant::now();
    let result = run_detached("for (let i = 0; i < 100000; i++) text(\"\");")
        .await
        .expect("result");
    let elapsed = started.elapsed();
    if let Some(path) = result
        .details
        .as_ref()
        .and_then(|details| details["fullOutputPath"].as_str())
    {
        remove_output(std::path::Path::new(path));
    }
    assert!(!result.is_error);
    assert!(result_text(&result).contains("==> text 100000/100000 <=="));
    assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");
}

/// Pi's "reports script failures as results that keep partial output".
#[tokio::test]
async fn reports_failures_with_partial_output() {
    let result =
        run_detached("text(\"partial\");\nconsole.log(\"log\");\nthrow new Error(\"boom\");")
            .await
            .expect("result");
    assert!(result.is_error);
    let text = result_text(&result);
    assert!(
        text.starts_with(
            "partial\n<console_output>\nlog\n</console_output>\nScript error:\nError: boom\n"
        ),
        "{text}"
    );
    assert!(text.contains("codemode.js:3"), "{text}");
    assert!(text.ends_with("No tool calls were made."), "{text}");
}

/// Pi's saved images through a script: the label before each image.
#[tokio::test]
async fn image_output_is_saved_and_returned_as_image_content() {
    let result = run_detached(&format!(
        "image(\"data:image/png;base64,{TINY_PNG_BASE64}\");\ntext(\"after\");"
    ))
    .await
    .expect("result");
    let text = result_text(&result);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines[1], "<image>");
    assert_eq!(lines[2], "after");
    remove_output(&saved_image_path(lines[0]));
    assert_eq!(
        result.content[2],
        UserContentBlock::Image(bake_ai::ImageContent {
            data: TINY_PNG_BASE64.to_owned(),
            mime_type: "image/png".to_owned(),
        })
    );
}

/// Without a session the discovery functions see no tools.
#[tokio::test]
async fn discovery_without_tools_finds_nothing() {
    let result = run_detached(
        "return [ALL_TOOLS.length, (await searchTools(\"read\")).length, await describeTool(\"read\") === undefined, await describeNamespace(\"x\") === undefined]",
    )
    .await
    .expect("result");
    assert_eq!(result_text(&result), "[0,0,true,true]");
    let invalid = run_detached(
        "const attempt = async (fn) => { try { await fn(); return 'ok'; } catch (error) { return error.message; } };\nreturn [await attempt(() => searchTools(1)), await attempt(() => searchTools('x', { limit: 0 })), await attempt(() => searchTools('x', { namespace: 1 })), await attempt(() => describeTool()), await attempt(() => describeNamespace(2))]",
    )
    .await
    .expect("result");
    assert_eq!(
        serde_json::from_str::<Value>(&result_text(&invalid)).unwrap(),
        json!([
            "searchTools() expects a query string",
            "searchTools() limit must be a positive integer",
            "searchTools() namespace must be a string",
            "describeTool() expects a tool name",
            "describeNamespace() expects a namespace name",
        ])
    );
}

/// The partial results publish the call records, empty before the first
/// call.
#[tokio::test]
async fn publishes_the_running_script_before_its_first_call() {
    let updates: Arc<Mutex<Vec<AgentToolResult>>> = Arc::default();
    let seen = Arc::clone(&updates);
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let result = execute_with(
        "c".to_owned(),
        json!({ "code": "return 1" }),
        None,
        AgentToolUpdateCallback::new(move |partial| seen.lock().unwrap().push(partial)),
        ToolContext::detached(),
        None,
        Admission::new(&COUNT),
    )
    .await
    .expect("result");
    assert_eq!(result_text(&result), "1");
    let updates = updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert!(updates[0].content.is_empty());
    assert_eq!(updates[0].details, Some(json!({ "calls": [] })));
}

/// Pi's `codemode-worker-admission.test.ts`, with Bake's script threads as
/// the workers: an already-aborted script takes no slot, and a slot is
/// released when the thread exits, not when the result returns.
#[tokio::test]
async fn admits_two_scripts_and_releases_a_slot_when_its_thread_exits() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let run = |code: &'static str, signal: Option<bake_ai::AbortSignal>| {
        execute_with(
            "worker-admission".to_owned(),
            json!({ "code": code }),
            signal,
            AgentToolUpdateCallback::ignore(),
            ToolContext::detached(),
            None,
            Admission::new(&COUNT),
        )
    };
    // Two slots held, as by scripts whose threads are still exiting.
    COUNT.store(
        MAX_CONCURRENT_CODEMODE_SCRIPTS,
        std::sync::atomic::Ordering::SeqCst,
    );
    let blocked = run("return 3", None).await.expect("result");
    assert!(blocked.is_error);
    assert_eq!(blocked.details, Some(json!({ "calls": [] })));
    assert!(result_text(&blocked).contains(
        "Script sandbox failed: Codemode worker limit reached (2 workers are still running or shutting down). Retry after they exit."
    ));
    let controller = bake_ai::AbortController::new();
    controller.abort();
    let aborted = run("while (true) {}", Some(controller.signal()))
        .await
        .expect("result");
    assert!(result_text(&aborted).contains("Script aborted: This operation was aborted"));
    assert_eq!(COUNT.load(std::sync::atomic::Ordering::SeqCst), 2);

    // Releasing one slot permits exactly one script, whose slot is free
    // again once its thread has exited.
    COUNT.store(1, std::sync::atomic::Ordering::SeqCst);
    let admitted = run("return 42", None).await.expect("result");
    assert!(!admitted.is_error);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while COUNT.load(std::sync::atomic::Ordering::SeqCst) != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the slot was not released"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Pi's "admits only two workers from a concurrent burst".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admits_only_two_scripts_from_a_concurrent_burst() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let runs: Vec<_> = (0..6)
        .map(|_| {
            tokio::spawn(execute_with(
                "burst".to_owned(),
                json!({ "code": "// @options: {\"timeout_ms\": 300}\nwhile (true) {}" }),
                None,
                AgentToolUpdateCallback::ignore(),
                ToolContext::detached(),
                None,
                Admission::new(&COUNT),
            ))
        })
        .collect();
    let mut limited = 0;
    let mut timed_out = 0;
    for run in runs {
        let result = run.await.expect("task").expect("result");
        let text = result_text(&result);
        if text.contains("Codemode worker limit reached") {
            limited += 1;
        } else if text.contains("Script timed out") {
            timed_out += 1;
        }
    }
    assert_eq!((timed_out, limited), (2, 4));
}
