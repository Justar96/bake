//! Bake's file tools against Pi v1.1.0's own output, recorded by
//! `tests/fixtures/pi-tools/generate.ts` into `golden.json`.
//!
//! Each test names the Pi test it follows; the recorded cases cover the
//! non-bash cases of Pi's `test/tools.test.ts`, `edit-tool-legacy-input`,
//! `builtin-tool-strict-mode`, `tool-result-images`, `default-tools-setting`,
//! and `path-utils` resolution through the tools.
// The recorded cases use POSIX paths and modes, so most cases and their
// helpers build on Unix only.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod support;

use std::path::Path;
use std::sync::Arc;

use bake_agent::{AgentToolResult, AgentToolUpdateCallback};
use bake_ai::{AbortController, ConstrainedSampling, ImageContent, Model, UserContentBlock};
use bake_coding_agent::system_prompt::{SystemPromptOptions, build_system_prompt_sections};
use bake_coding_agent::tools::definition::ToolDefinition;
use bake_coding_agent::tools::edit::prepare_edit_arguments;
use bake_coding_agent::tools::image::ImageResizeOptions;
use bake_coding_agent::tools::image::normalize_tool_result_images;
use bake_coding_agent::tools::index::{
    ALL_TOOL_NAMES, DEFAULT_TOOL_NAMES, NoTools, ToolSelectionOptions, ToolsOptions,
    apply_tool_modifiers, create_all_tool_definitions, create_coding_tool_definitions,
    create_read_only_tool_definitions, create_tool_definition, get_tool_list_error,
    resolve_default_tools, select_tools, tool_name_matcher,
};
use bake_coding_agent::tools::read::ReadToolOptions;
use base64::Engine as _;
use serde_json::{Value, json};
use support::{golden, scrub, setup, substitute};

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_owned())
        .collect()
}

fn definitions(options: &ToolsOptions) -> Vec<ToolDefinition> {
    create_all_tool_definitions(Path::new("/workspace"), options)
}

fn constrained_sampling(sampling: &Option<ConstrainedSampling>) -> Value {
    match sampling {
        None => Value::Null,
        Some(ConstrainedSampling::JsonSchema { strict }) => {
            json!({ "type": "json_schema", "strict": strict })
        }
        Some(other) => panic!("unexpected sampling {other:?}"),
    }
}

/// Pi: `builtin-tool-strict-mode.test.ts` and the definitions every tool
/// test builds: names, labels, descriptions, schemas, output schemas,
/// sampling, prompt text, legacy-input shims, and execution modes.
#[test]
fn definitions_match_pi() {
    let recorded = golden()["definitions"].as_object().expect("definitions");
    let built = definitions(&ToolsOptions::default());
    let names: Vec<&str> = built.iter().map(|definition| definition.name()).collect();
    assert_eq!(
        names,
        ["read", "edit", "write", "grep", "find", "ls"],
        "bash and powershell are not ported"
    );
    for definition in &built {
        let tool = &definition.tool;
        let pi = &recorded[&tool.name];
        let mine = json!({
            "name": tool.name,
            "label": tool.label,
            "description": tool.description,
            "parameters": tool.parameters,
            "outputSchema": tool.output_schema,
            "constrainedSampling": constrained_sampling(&tool.constrained_sampling),
            "promptSnippet": definition.prompt_snippet,
            "promptGuidelines": definition.prompt_guidelines,
            "hasPrepareArguments": tool.prepare_arguments.is_some(),
            "executionMode": tool.execution_mode.map(|mode| json!(mode)),
        });
        assert_eq!(&mine, pi, "definition of {}", tool.name);
        // Byte for byte, keys in Pi's order.
        assert_eq!(
            serde_json::to_string(&tool.parameters).expect("json"),
            serde_json::to_string(&pi["parameters"]).expect("json"),
            "parameter order of {}",
            tool.name
        );
        let contribution = &golden()["contributions"][&tool.name];
        assert_eq!(
            definition.prompt_snippet.as_deref(),
            contribution["snippet"].as_str()
        );
        assert_eq!(
            definition.prompt_guidelines.clone().unwrap_or_default(),
            strings(&contribution["guidelines"]),
        );
    }
}

/// Pi: `index.ts`'s sets, and the bash slot.
#[test]
fn tool_sets_follow_pi_and_take_bash_from_its_slot() {
    let cwd = Path::new("/workspace");
    let names =
        |set: Vec<ToolDefinition>| set.iter().map(|d| d.name().to_owned()).collect::<Vec<_>>();
    let none = ToolsOptions::default();
    assert_eq!(
        names(create_coding_tool_definitions(cwd, &none)),
        ["read", "edit", "write"]
    );
    assert_eq!(
        names(create_read_only_tool_definitions(cwd, &none)),
        ["read", "grep", "find", "ls"]
    );
    let bash = bake_coding_agent::tools::definition::create_tool_definition_from_agent_tool(
        bake_agent::AgentTool::new(
            "bash",
            "bash",
            "stand-in",
            json!({"type": "object"}),
            |_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("")) }),
        ),
    );
    let with_bash = ToolsOptions {
        bash: Some(bash),
        ..ToolsOptions::default()
    };
    assert_eq!(
        names(create_coding_tool_definitions(cwd, &with_bash)),
        DEFAULT_TOOL_NAMES
    );
    assert_eq!(
        names(create_all_tool_definitions(cwd, &with_bash)),
        ["read", "bash", "edit", "write", "grep", "find", "ls"]
    );
    assert_eq!(ALL_TOOL_NAMES.len(), 8);
    assert!(create_tool_definition("powershell", cwd, &with_bash).is_none());
    assert!(create_tool_definition("nope", cwd, &with_bash).is_none());
}

/// Pi: `system-prompt.test.ts`'s rules for the sets the session builds:
/// after Bake's own rules, the tool guidelines Pi lists, in Pi's order,
/// from the tool definitions. Pi's last two rules and its `bash` guideline,
/// which names the `PI_*` variables, are Pi's own and left out (D33).
#[test]
fn tool_guidelines_match_pi() {
    const PI_ONLY: [&str; 3] = [
        "- Be concise in your responses",
        "- Show file paths clearly when working with files",
        "- You can inspect PI_* environment variables for current model and session details.",
    ];
    let built = definitions(&ToolsOptions::default());
    let lines = |section: &str| -> Vec<String> {
        section
            .lines()
            .filter(|line| *line != "<rules>" && *line != "</rules>")
            .map(str::to_owned)
            .collect()
    };
    for case in golden()["prompts"].as_array().expect("prompts") {
        let mut options = SystemPromptOptions::new("/work");
        options.selected_tools = strings(&case["selected"]);
        for definition in &built {
            if let Some(guidelines) = definition
                .prompt_guidelines
                .as_ref()
                .filter(|g| !g.is_empty())
            {
                options
                    .tool_guidelines
                    .push((definition.name().to_owned(), guidelines.clone()));
            }
        }
        let sections = build_system_prompt_sections(&options).expect("sections");
        let rules = sections
            .iter()
            .find(|(name, _)| name == "rules")
            .map(|(_, text)| lines(text))
            .unwrap_or_default();
        let pi: Vec<String> = lines(case["rules"].as_str().expect("rules"))
            .into_iter()
            .filter(|line| !PI_ONLY.contains(&line.as_str()))
            .collect();
        assert!(rules.ends_with(&pi), "{:?}: {rules:?}", case["selected"]);
        let own = if options.selected_tools.is_empty() {
            4
        } else {
            5
        };
        assert_eq!(rules.len() - pi.len(), own, "{:?}", case["selected"]);
    }
}

/// Pi: `edit-tool-legacy-input.test.ts`.
#[test]
fn edit_legacy_input_matches_pi() {
    for case in golden()["prepareArguments"].as_array().expect("cases") {
        assert_eq!(
            prepare_edit_arguments(case["input"].clone()),
            case["output"],
            "{}",
            case["input"]
        );
    }
    let tool = &definitions(&ToolsOptions::default())[1].tool;
    let prepare = tool
        .prepare_arguments
        .as_ref()
        .expect("edit prepares arguments");
    assert_eq!(
        prepare(json!({"path": "f", "oldText": "a", "newText": "b"})).expect("prepared"),
        json!({"path": "f", "edits": [{"oldText": "a", "newText": "b"}]})
    );
}

/// Pi: `default-tools-setting.test.ts` and `settings-manager` tool-list
/// tests: list errors, modifiers, the `defaultTools` resolution, and
/// `createToolNameMatcher`.
#[test]
fn tool_selection_rules_match_pi() {
    let selection = &golden()["toolSelection"];
    for case in selection["listErrors"].as_array().expect("cases") {
        assert_eq!(
            get_tool_list_error(&strings(&case["entries"]))
                .map(Value::String)
                .unwrap_or(Value::Null),
            case["error"]
        );
    }
    for case in selection["modifiers"].as_array().expect("cases") {
        assert_eq!(
            apply_tool_modifiers(&strings(&case["base"]), &strings(&case["entries"])),
            strings(&case["result"])
        );
    }
    for case in selection["defaults"].as_array().expect("cases") {
        assert_eq!(
            resolve_default_tools(&strings(&case["entries"])),
            strings(&case["result"])
        );
    }
    for case in selection["matchers"].as_array().expect("cases") {
        let matcher = tool_name_matcher(&strings(&case["entries"]));
        let matches: Vec<bool> = strings(&case["names"])
            .iter()
            .map(|name| matcher(name))
            .collect();
        assert_eq!(json!(matches), case["matches"]);
    }
}

/// Pi: `sdk-tools.test.ts` and `agent-session` allowlist cases, as
/// `main.ts` maps `--tools`, `--exclude-tools`, `--no-tools`, and
/// `--no-builtin-tools`.
#[test]
fn tool_selection_follows_pi_session_options() {
    let available: Vec<String> = ["read", "bash", "edit", "write", "grep", "find", "ls"]
        .map(String::from)
        .to_vec();
    let select = |options: ToolSelectionOptions| {
        select_tools(&options, None, &available).expect("selection")
    };
    let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();

    let default = select(ToolSelectionOptions::default());
    assert_eq!(default.active, DEFAULT_TOOL_NAMES);
    assert_eq!(default.registered, available);

    let allow = select(ToolSelectionOptions {
        tools: Some(names(&["read", "grep"])),
        ..Default::default()
    });
    assert_eq!(allow.active, ["read", "grep"]);
    assert_eq!(allow.registered, ["read", "grep"]);

    // Matching a pattern activates a tool the selection does not name.
    let pattern = select(ToolSelectionOptions {
        tools: Some(names(&["gr*", "read"])),
        ..Default::default()
    });
    assert_eq!(pattern.active, ["read", "grep"]);
    assert_eq!(pattern.registered, ["read", "grep"]);

    let modify = select(ToolSelectionOptions {
        tools: Some(names(&["+grep", "-bash"])),
        ..Default::default()
    });
    assert_eq!(modify.active, ["read", "edit", "write", "grep"]);
    assert_eq!(modify.registered, available);

    let exclude = select(ToolSelectionOptions {
        exclude_tools: Some(names(&["e*"])),
        ..Default::default()
    });
    assert_eq!(exclude.active, ["read", "bash", "write"]);
    assert!(!exclude.registered.contains(&"edit".to_owned()));

    let none = select(ToolSelectionOptions {
        no_tools: Some(NoTools::All),
        ..Default::default()
    });
    assert!(none.active.is_empty() && none.registered.is_empty());

    let builtin = select(ToolSelectionOptions {
        no_tools: Some(NoTools::Builtin),
        ..Default::default()
    });
    assert!(builtin.active.is_empty());
    assert_eq!(builtin.registered, available);

    let only = select(ToolSelectionOptions {
        no_tools: Some(NoTools::All),
        tools: Some(names(&["ls"])),
        ..Default::default()
    });
    assert_eq!(only.active, ["ls"]);

    let error = select_tools(
        &ToolSelectionOptions {
            tools: Some(names(&["read", "+grep"])),
            ..Default::default()
        },
        None,
        &available,
    );
    assert_eq!(
        error.unwrap_err(),
        "Invalid tools option: tool names cannot be mixed with +name or -name entries"
    );

    let from_setting = select_tools(
        &ToolSelectionOptions::default(),
        Some(&names(&["grep", "ls"])),
        &available,
    )
    .expect("selection");
    assert_eq!(from_setting.active, ["grep", "ls"]);
}

fn text_model() -> Model {
    serde_json::from_value(json!({
        "id": "text-only", "name": "text-only", "api": "openai-completions", "provider": "test",
        "baseUrl": "http://127.0.0.1:9", "input": ["text"], "reasoning": false,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 1000, "maxTokens": 100,
    }))
    .expect("model")
}

fn vision_model(resize: Value) -> Model {
    let mut model = text_model();
    model.input = serde_json::from_value(json!(["text", "image"])).expect("input");
    model.input_limits = Some(json!({ "images": { "resize": resize } }));
    model
}

/// Pi: `image-resize-callers.test.ts`: the read tool resizes with the
/// session model's `inputLimits.images.resize`, and returns text only when
/// no encoding fits.
#[tokio::test]
async fn read_uses_the_model_resize_profile() {
    let dir = support::TempDir::new("resize-profile");
    std::fs::write(
        dir.path().join("wide.png"),
        base64::engine::general_purpose::STANDARD
            .decode(png(40, 10))
            .expect("png"),
    )
    .expect("write");
    let read = |resize: Value| {
        let model = vision_model(resize);
        let options = ToolsOptions {
            read: ReadToolOptions {
                model: Some(Arc::new(move || Some(model.clone()))),
                ..ReadToolOptions::default()
            },
            ..ToolsOptions::default()
        };
        create_tool_definition("read", dir.path(), &options)
            .expect("read")
            .tool
    };
    let text = |result: &AgentToolResult| match &result.content[0] {
        UserContentBlock::Text(text) => text.text.clone(),
        other => panic!("unexpected {other:?}"),
    };
    let tool = read(json!({ "maxWidth": 20, "maxHeight": 20 }));
    let result = (tool.execute)(
        "a".into(),
        json!({"path": "wide.png"}),
        None,
        AgentToolUpdateCallback::ignore(),
    )
    .await
    .expect("read");
    assert_eq!(
        text(&result),
        "Read image file [image/png]\n[Image: original 40x10, displayed at 20x5. Multiply coordinates by 2.00 to map to original image.]"
    );
    assert!(
        matches!(&result.content[1], UserContentBlock::Image(image) if image.mime_type == "image/png")
    );

    let tool = read(json!({ "maxBytes": 1 }));
    let result = (tool.execute)(
        "b".into(),
        json!({"path": "wide.png"}),
        None,
        AgentToolUpdateCallback::ignore(),
    )
    .await
    .expect("read");
    assert_eq!(result.content.len(), 1);
    assert_eq!(
        text(&result),
        "Read image file [image/png]\n[Image omitted: could not be resized below the inline image size limit.]"
    );
}

fn result_json(result: &AgentToolResult, structured: bool) -> Value {
    let mut value = json!({
        "content": result.content,
        "details": result.details.clone().unwrap_or(Value::Null),
    });
    if structured {
        value["structuredContent"] = result.structured_content.clone().unwrap_or(Value::Null);
    }
    value
}

/// Replaces the image data the `image` crate encodes differently from
/// Photon, after checking that it is an image of its MIME type.
fn mask_image_data(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("type") == Some(&json!("image")) {
                // Long data was compacted; its head still shows the type.
                let data = map["data"]
                    .as_str()
                    .or_else(|| map["data"]["head"].as_str())
                    .expect("image data");
                let magic = match map["mimeType"].as_str() {
                    Some("image/png") => "iVBORw0KGgo",
                    Some("image/jpeg") => "/9j/",
                    other => panic!("unexpected mime {other:?}"),
                };
                assert!(
                    data.starts_with(magic),
                    "image data is a {:?}",
                    map["mimeType"]
                );
                map.insert("data".into(), json!("<image>"));
            }
            for item in map.values_mut() {
                mask_image_data(item);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_image_data),
        _ => {}
    }
}

async fn run_case(case: &Value) -> Result<(), String> {
    let name = case["name"].as_str().expect("name");
    let tool_name = case["tool"].as_str().expect("tool");
    let dir = setup(&case["files"], name);
    let dir_text = dir.text();
    let mut options = ToolsOptions::default();
    if case.get("model").is_some() {
        let model = text_model();
        options.read = ReadToolOptions {
            model: Some(Arc::new(move || Some(model.clone()))),
            ..ReadToolOptions::default()
        };
    }
    let definition = create_tool_definition(tool_name, dir.path(), &options).expect("tool");
    let args = substitute(&case["args"], &dir_text);
    let outcome = (definition.tool.execute)(
        format!("call-{name}"),
        args,
        None,
        AgentToolUpdateCallback::ignore(),
    )
    .await;
    let mut mine = json!({});
    match outcome {
        Ok(result) => mine["result"] = scrub(&result_json(&result, true), &dir_text),
        Err(error) => mine["error"] = scrub(&json!(error), &dir_text),
    }
    let mut expected = json!({});
    for key in ["result", "error"] {
        if let Some(value) = case.get(key) {
            expected[key] = value.clone();
        }
    }
    if case.get("imageDataExact") == Some(&json!(false)) {
        mask_image_data(&mut mine);
        mask_image_data(&mut expected);
    }
    if mine != expected {
        return Err(format!(
            "{tool_name}/{name}:\n  bake: {}\n  pi:   {}",
            serde_json::to_string(&mine).unwrap_or_default(),
            serde_json::to_string(&expected).unwrap_or_default()
        ));
    }
    if tool_name == "write" || tool_name == "edit" {
        let path = case["args"]["path"]
            .as_str()
            .expect("path")
            .replace(support::DIR, &dir_text);
        let path = if Path::new(&path).is_absolute() {
            path.into()
        } else {
            dir.path().join(path)
        };
        let after = std::fs::read(path)
            .ok()
            .map(|bytes| {
                support::compact(&json!(
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ))
            })
            .unwrap_or(Value::Null);
        if after != case["after"] {
            return Err(format!(
                "{tool_name}/{name}: file after is {after}, Pi left {}",
                case["after"]
            ));
        }
    }
    Ok(())
}

/// Pi: `tools.test.ts` (read, write, edit, ls; not bash), the edit fuzzy
/// matching, line ending, and BOM tests, `read-tool-*` and `path-utils`
/// resolution cases, recorded by running Pi's tools on the same files.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_cases_match_pi() {
    let skip_modes = support::ignores_file_modes();
    let mut failures = Vec::new();
    let mut ran = 0;
    for case in golden()["cases"].as_array().expect("cases") {
        if skip_modes
            && case["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|f| f.get("mode").is_some()))
        {
            eprintln!(
                "note: skipping {} (this user ignores file modes)",
                case["name"]
            );
            continue;
        }
        ran += 1;
        if let Err(failure) = run_case(case).await {
            failures.push(failure);
        }
    }
    assert!(ran >= 98, "ran {ran} cases");
    assert!(
        failures.is_empty(),
        "{} of {ran} cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn png(width: u32, height: u32) -> String {
    // The generator's `createPng`: 8-bit grey rows of value `row % 256`.
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        pixels.extend(std::iter::repeat_n((row % 256) as u8, width as usize));
    }
    let image = image::GrayImage::from_raw(width, height, pixels).expect("pixels");
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("png");
    base64::engine::general_purpose::STANDARD.encode(out.into_inner())
}

/// Pi: `tool-result-images.test.ts`.
#[test]
fn tool_result_images_match_pi() {
    for case in golden()["toolResultImages"].as_array().expect("cases") {
        let content: Vec<UserContentBlock> = case["content"]
            .as_array()
            .expect("content")
            .iter()
            .map(|block| {
                if block["type"] == "image" {
                    let data = match &block["data"] {
                        Value::String(data) => data.clone(),
                        size => png(
                            size["$png"][0].as_u64().expect("w") as u32,
                            size["$png"][1].as_u64().expect("h") as u32,
                        ),
                    };
                    UserContentBlock::Image(ImageContent {
                        data,
                        mime_type: block["mimeType"].as_str().expect("mime").into(),
                    })
                } else {
                    serde_json::from_value(block.clone()).expect("text block")
                }
            })
            .collect();
        let auto_resize = case["options"]["autoResizeImages"]
            .as_bool()
            .unwrap_or(true);
        let normalized =
            normalize_tool_result_images(&content, auto_resize, ImageResizeOptions::default());
        assert_eq!(
            normalized.is_none(),
            case["unchanged"].as_bool().expect("unchanged"),
            "{}",
            case["name"]
        );
        let output: Vec<Value> = normalized
            .unwrap_or(content)
            .iter()
            .map(|block| match block {
                UserContentBlock::Image(image) => {
                    json!({"type": "image", "mimeType": image.mime_type})
                }
                text => serde_json::to_value(text).expect("json"),
            })
            .collect();
        assert_eq!(json!(output), case["output"], "{}", case["name"]);
    }
}

/// Pi: `file-mutation-queue.test.ts`'s parallel-edit case through the edit
/// tool: concurrent edits of one file all land.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_edits_of_one_file_all_apply() {
    let dir = support::TempDir::new("parallel-edit");
    let file = dir.path().join("f.txt");
    let count = 24;
    let original: String = (0..count).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &original).expect("write");
    let tool = create_tool_definition("edit", dir.path(), &ToolsOptions::default())
        .expect("edit")
        .tool;
    let mut tasks = Vec::new();
    for i in 0..count {
        let execute = Arc::clone(&tool.execute);
        tasks.push(tokio::spawn(async move {
            let args = json!({"path": "f.txt", "edits": [{"oldText": format!("line {i}\n"), "newText": format!("LINE {i}\n")}]});
            execute(format!("call-{i}"), args, None, AgentToolUpdateCallback::ignore()).await
        }));
    }
    for task in tasks {
        let result = task.await.expect("task").expect("edit succeeds");
        assert!(
            matches!(&result.content[0], UserContentBlock::Text(text) if text.text.starts_with("Successfully replaced 1 block(s)"))
        );
    }
    let expected: String = (0..count).map(|i| format!("LINE {i}\n")).collect();
    assert_eq!(std::fs::read_to_string(&file).expect("read"), expected);
}

/// Pi: the abort cases of `tools.test.ts`: an aborted signal rejects with
/// "Operation aborted" and leaves files alone.
#[tokio::test]
async fn aborted_calls_reject_and_change_nothing() {
    let dir = support::TempDir::new("abort");
    std::fs::write(dir.path().join("f.txt"), "hello\n").expect("write");
    let controller = AbortController::new();
    controller.abort();
    for (name, args) in [
        ("read", json!({"path": "f.txt"})),
        ("write", json!({"path": "f.txt", "content": "new"})),
        (
            "edit",
            json!({"path": "f.txt", "edits": [{"oldText": "hello", "newText": "bye"}]}),
        ),
        ("ls", json!({})),
        ("grep", json!({"pattern": "hello"})),
        ("find", json!({"pattern": "*"})),
    ] {
        let tool = create_tool_definition(name, dir.path(), &ToolsOptions::default())
            .expect("tool")
            .tool;
        let outcome = (tool.execute)(
            "call".into(),
            args,
            Some(controller.signal()),
            AgentToolUpdateCallback::ignore(),
        )
        .await;
        assert_eq!(
            outcome.err().as_deref(),
            Some("Operation aborted"),
            "{name}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).expect("read"),
        "hello\n"
    );
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

/// Sorted lines of a result's text, for searches whose file order varies.
fn sorted_text(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(text) = value["result"]["content"][0]["text"].as_str() {
        let (body, notice) = text
            .split_once("\n\n[")
            .map_or((text, None), |(b, n)| (b, Some(n)));
        let mut lines: Vec<&str> = body.lines().collect();
        lines.sort_unstable();
        let mut sorted = lines.join("\n");
        if let Some(notice) = notice {
            sorted.push_str("\n\n[");
            sorted.push_str(notice);
        }
        value["result"]["content"][0]["text"] = json!(sorted);
    }
    value
}

async fn run_external(kind: &str) {
    let binary = if kind == "grep" { "rg" } else { "fd" };
    if !on_path(binary) {
        eprintln!("note: skipping the live {kind} cases: `{binary}` is not on PATH");
        return;
    }
    let mut failures = Vec::new();
    for case in golden()[kind].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let dir = setup(&case["files"], name);
        let dir_text = dir.text();
        let tool = create_tool_definition(kind, dir.path(), &ToolsOptions::default())
            .expect("tool")
            .tool;
        let args = substitute(&case["args"], &dir_text);
        let mut mine = json!({});
        match (tool.execute)(
            format!("call-{name}"),
            args,
            None,
            AgentToolUpdateCallback::ignore(),
        )
        .await
        {
            Ok(result) => mine["result"] = scrub(&result_json(&result, false), &dir_text),
            Err(error) => mine["error"] = scrub(&json!(error), &dir_text),
        }
        let mut expected = json!({});
        for key in ["result", "error"] {
            if let Some(value) = case.get(key) {
                expected[key] = value.clone();
            }
        }
        if sorted_text(&mine) != sorted_text(&expected) {
            failures.push(format!(
                "{kind}/{name}:\n  bake: {mine}\n  pi:   {expected}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Pi: `tools.test.ts`'s grep cases, run with the real `rg`; skipped with a
/// note when `rg` is not on `PATH`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_grep_matches_pi() {
    run_external("grep").await;
}

/// Pi: `tools.test.ts`'s find cases, run with the real `fd`; skipped with a
/// note when `fd` is not on `PATH`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_find_matches_pi() {
    run_external("find").await;
}
