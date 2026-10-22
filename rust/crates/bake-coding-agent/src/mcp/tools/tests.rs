//! Pi v1.3.0 `test/mcp-extension.test.ts`, "MCP tools", and the
//! all-or-nothing listing of Bake's `packages/mcp/mcp-client`
//! (`tests/mcp-client.spec.ts`, "syncTools").

use serde_json::json;

use super::*;
use crate::mcp::config::McpServerSettings;

fn map(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn plan(server: &str, tools: &[(&str, Value, Value)]) -> Result<Vec<PlannedTool>, String> {
    plan_settings(server, &McpServerSettings::default(), tools)
}

fn plan_settings(
    server: &str,
    settings: &McpServerSettings,
    tools: &[(&str, Value, Value)],
) -> Result<Vec<PlannedTool>, String> {
    type Owned = (String, Map<String, Value>, Map<String, Value>);
    let owned: Vec<Owned> = tools
        .iter()
        .map(|(name, schema, definition)| {
            (
                name.to_string(),
                map(schema.clone()),
                map(definition.clone()),
            )
        })
        .collect();
    let listed: Vec<ListedTool<'_>> = owned
        .iter()
        .map(|(name, input_schema, definition)| ListedTool {
            name,
            input_schema,
            definition,
        })
        .collect();
    plan_tools(server, settings, &listed)
}

fn texts(content: &[UserContentBlock]) -> Vec<String> {
    content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(image) => format!("image {} {}", image.mime_type, image.data),
        })
        .collect()
}

fn result(value: Value) -> CallToolResult {
    let mut object = map(value);
    let content = match object.shift_remove("content") {
        Some(Value::Array(content)) => content,
        _ => Vec::new(),
    };
    CallToolResult {
        content,
        structured_content: object.get("structuredContent").cloned(),
        is_error: object
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw: object,
    }
}

async fn convert(value: Value) -> AgentToolResult {
    convert_result("docs", "t", result(value), false, &WorkGate::default()).await
}

#[test]
fn plans_object_schemas_with_pi_descriptions() {
    let planned = plan(
        "docs",
        &[
            (
                "search",
                json!({"properties": {"q": {"type": "string"}}}),
                json!({"description": "  Search docs  "}),
            ),
            (
                "titled",
                json!({"type": "object"}),
                json!({"title": "Titled tool"}),
            ),
            (
                "annotated",
                json!({}),
                json!({"annotations": {"title": "From annotations"}}),
            ),
            ("plain", json!({}), json!({})),
            ("get.item", json!({}), json!({})),
        ],
    )
    .unwrap();
    let names: Vec<_> = planned.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "mcp__docs__search",
            "mcp__docs__titled",
            "mcp__docs__annotated",
            "mcp__docs__plain",
            &public_tool_name("docs", "get.item"),
        ]
    );
    assert_eq!(planned[0].description, "Search docs");
    assert_eq!(planned[1].description, "Titled tool");
    assert_eq!(planned[2].description, "From annotations");
    assert_eq!(planned[3].description, "MCP tool plain from server docs");
    assert_eq!(
        planned[0].parameters,
        json!({"properties": {"q": {"type": "string"}}, "type": "object"})
    );
    assert_eq!(
        planned[3].parameters,
        json!({"type": "object", "properties": {}})
    );
    assert_eq!(planned[4].tool, "get.item");
}

#[test]
fn refuses_a_listing_whole() {
    let duplicate = plan(
        "s",
        &[("a", json!({}), json!({})), ("a", json!({}), json!({}))],
    );
    assert!(duplicate.unwrap_err().contains("more than once"));
    let not_object = plan(
        "s",
        &[
            ("ok", json!({}), json!({})),
            ("bad", json!({"type": "string"}), json!({})),
        ],
    );
    assert!(
        not_object
            .unwrap_err()
            .contains("type \"string\", not \"object\"")
    );
    let huge = "x".repeat(MAX_DECLARATION_BYTES);
    let oversized = plan("s", &[("big", json!({}), json!({"description": huge}))]);
    assert!(oversized.unwrap_err().contains("-byte definition"));
    let many: Vec<(String, Value, Value)> = (0..=MAX_TOOLS_PER_SERVER)
        .map(|index| (format!("t{index}"), json!({}), json!({})))
        .collect();
    let many: Vec<(&str, Value, Value)> = many
        .iter()
        .map(|(name, schema, definition)| (name.as_str(), schema.clone(), definition.clone()))
        .collect();
    assert!(plan("s", &many).unwrap_err().contains("over the limit"));
    // A control at the limits.
    assert_eq!(
        plan("s", &many[..MAX_TOOLS_PER_SERVER]).unwrap().len(),
        MAX_TOOLS_PER_SERVER
    );
}

#[test]
fn hidden_tools_are_left_out_but_still_checked() {
    let settings = McpServerSettings {
        tool_exposure: vec![("secret".into(), McpExposure::Hidden)],
        ..McpServerSettings::default()
    };
    let planned = plan_settings(
        "s",
        &settings,
        &[
            ("open", json!({}), json!({})),
            ("secret", json!({}), json!({})),
        ],
    )
    .unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].tool, "open");
    let refused = plan_settings(
        "s",
        &settings,
        &[("secret", json!({"type": "array"}), json!({}))],
    );
    assert!(refused.is_err());
}

// Pi: "converts results, passing the CallToolResult to scripts and
// flagging errors".
#[tokio::test]
async fn converts_results_and_flags_errors() {
    let blocks = json!([
        { "type": "resource_link", "uri": "file:///a", "name": "a" },
        { "type": "resource", "resource": { "uri": "file:///b", "text": "b text" } },
        { "type": "audio", "data": "", "mimeType": "audio/wav" },
    ]);
    let converted = convert(json!({
        "content": blocks,
        "structuredContent": { "ok": true },
        "_meta": { "trace": "x" },
    }))
    .await;
    assert_eq!(
        texts(&converted.content),
        [
            "[Resource file:///a \"a\"]",
            "b text",
            "[audio audio/wav omitted]"
        ]
    );
    assert_eq!(
        converted.details,
        Some(json!({"server": "docs", "tool": "t"}))
    );
    assert_eq!(
        converted.structured_content,
        Some(json!({ "structuredContent": { "ok": true }, "content": blocks }))
    );
    assert!(!converted.is_error);

    let structured = convert(json!({ "content": [], "structuredContent": { "n": 1 } })).await;
    assert_eq!(texts(&structured.content), ["{\n  \"n\": 1\n}"]);
    // #10416: array structured content reaches the model as JSON.
    let array = convert(json!({ "content": [], "structuredContent": [{ "id": 1 }] })).await;
    assert_eq!(texts(&array.content), ["[\n  {\n    \"id\": 1\n  }\n]"]);

    let failed =
        convert(json!({ "content": [{ "type": "text", "text": "nope" }], "isError": true })).await;
    assert_eq!(texts(&failed.content), ["nope"]);
    assert!(failed.is_error);
    let silent = convert(json!({ "content": [], "isError": true })).await;
    assert_eq!(
        texts(&silent.content),
        ["MCP tool docs/t returned an error"]
    );
    assert!(silent.is_error);
}

// Pi: "points resource links to read_mcp_resource and saves binary
// resources", without `read_mcp_resource`, which Bake does not have.
#[tokio::test]
async fn describes_resource_links_and_saves_binary_resources() {
    let converted = convert(json!({
        "content": [
            {
                "type": "resource_link", "uri": "docs://guide", "name": "guide", "title": "The Guide",
                "mimeType": "text/markdown", "size": 2048, "description": "How to use it"
            },
            { "type": "resource", "resource": { "uri": "file:///r/report.pdf", "mimeType": "application/pdf", "blob": "JVBERg==" } },
            { "type": "resource", "resource": { "uri": "docs://logo", "mimeType": "image/png", "blob": "AAAA" } },
            { "type": "resource", "resource": { "uri": "docs://data", "mimeType": "application/json; charset=utf-8", "blob": "eyJhIjoxfQ==" } },
        ]
    }))
    .await;
    let texts = texts(&converted.content);
    assert_eq!(
        texts[0],
        "[Resource docs://guide \"The Guide\" (text/markdown, 2.0KB): How to use it]"
    );
    let saved = texts[1]
        .strip_prefix("[Binary resource file:///r/report.pdf (application/pdf, 4B) saved to ")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or_else(|| panic!("{}", texts[1]));
    let saved = std::path::Path::new(saved);
    assert!(saved.to_string_lossy().ends_with("bake-mcp.pdf"));
    assert_eq!(std::fs::read(saved).unwrap(), b"%PDF");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(saved).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_dir_all(saved.parent().unwrap()).unwrap();
    assert_eq!(texts[2], "image image/png AAAA");
    assert_eq!(texts[3], "{\"a\":1}");
}

// Pi's `readableResources`: a link of a server whose resources
// `read_mcp_resource` reaches names that tool and the server.
#[tokio::test]
async fn a_resource_link_names_the_read_tool_when_resources_are_readable() {
    let link = json!({
        "content": [{ "type": "resource_link", "uri": "docs://guide", "name": "guide" }]
    });
    let readable = convert_result(
        "docs",
        "t",
        result(link.clone()),
        true,
        &WorkGate::default(),
    )
    .await;
    assert_eq!(
        texts(&readable.content),
        ["[Resource docs://guide \"guide\". Read it with read_mcp_resource (server \"docs\")]"]
    );
    let plain = convert_result("docs", "t", result(link), false, &WorkGate::default()).await;
    assert_eq!(texts(&plain.content), ["[Resource docs://guide \"guide\"]"]);
}

#[tokio::test]
async fn malformed_blocks_become_notes() {
    let converted = convert(json!({
        "content": [
            1,
            { "type": "text" },
            { "type": "image", "data": "AAAA" },
            { "type": "video" },
            { "type": "resource", "resource": { "uri": "x" } },
        ]
    }))
    .await;
    assert_eq!(
        texts(&converted.content),
        [
            "[unsupported MCP content ]",
            "[unsupported MCP content text]",
            "[unsupported MCP content image]",
            "[unsupported MCP content video]",
            "[unsupported MCP content resource]",
        ]
    );
}

// Pi: "cuts the middle of model-facing text over 20KB and keeps the full
// result for scripts".
#[tokio::test]
async fn cuts_long_text_and_saves_it_whole() {
    let lines: Vec<String> = (1..=3000).map(|index| format!("line {index}")).collect();
    let full = lines.join("\n");
    let value = json!({
        "content": [
            { "type": "text", "text": full },
            { "type": "image", "data": "AAAA", "mimeType": "image/png" },
        ]
    });
    let converted = convert(value.clone()).await;
    assert_eq!(converted.content.len(), 2);
    let texts = texts(&converted.content);
    let header = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: 3000\n\nline 1\nline 2\n",
        full.len().div_ceil(4)
    );
    assert!(texts[0].starts_with(&header), "{}", &texts[0][..200]);
    assert!(texts[0].contains(" chars truncated…"));
    let path = converted.details.as_ref().unwrap()["fullOutputPath"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(texts[0].ends_with(&format!(
        "line 3000\n\n[Full output: {path} (read it with offset/limit)]"
    )));
    assert!(texts[0].len() < 21 * 1024);
    assert_eq!(texts[1], "image image/png AAAA");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), full);
    std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    assert_eq!(converted.structured_content, Some(value));

    let small = convert(json!({ "content": [{ "type": "text", "text": "ok" }] })).await;
    assert_eq!(small.details, Some(json!({"server": "docs", "tool": "t"})));
}

#[tokio::test]
async fn a_closed_session_saves_nothing_and_says_so() {
    let gate = WorkGate::default();
    gate.close().await;
    let converted = convert_result(
        "docs",
        "t",
        result(json!({ "content": [{ "type": "text", "text": "x".repeat(MCP_OUTPUT_MAX_BYTES + 1) }] })),
        false,
        &gate,
    )
    .await;
    let texts = texts(&converted.content);
    assert!(
        texts[0].ends_with("[Could not save the full output: the session is shut down]"),
        "{}",
        texts[0]
    );
    assert!(converted.details.unwrap().get("fullOutputPath").is_none());
}

#[test]
fn extensions_and_text_types_follow_pi() {
    assert_eq!(extension_of("file:///r/report.pdf"), ".pdf");
    assert_eq!(extension_of("docs://logo"), ".bin");
    assert_eq!(extension_of("file:///a/b.verylongext"), ".bin");
    assert_eq!(extension_of("plain/name.tar"), ".tar");
    assert!(is_text_mime_type(Some("TEXT/plain")));
    assert!(is_text_mime_type(Some("application/ld+json")));
    assert!(is_text_mime_type(Some("image/svg+xml")));
    assert!(!is_text_mime_type(Some("application/pdf")));
    assert!(!is_text_mime_type(None));
}

// Pi v1.3.0 "declares the CallToolResult scripts receive, keeping
// references in the server's output schema".
#[test]
fn declares_the_call_tool_result_scripts_receive_keeping_references() {
    let content = r#"content: Array<{ data?: string; mimeType?: string; text?: string; type: "text" | "image" | "audio" | "resource_link" | "resource"; [key: string]: unknown; }>; isError?: boolean;"#;
    assert_eq!(
        bake_codemode::render_tool_output_type(Some(&mcp_result_schema(None))),
        format!("{{ {content} structuredContent?: unknown; [key: string]: unknown; }}")
    );
    let hit = json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] });
    let mut hit_v1 = hit.clone();
    hit_v1["default"] = json!({ "$ref": "#/stays" });
    let schema = mcp_result_schema(Some(&map(json!({
        "type": "object",
        "properties": {
            "hits": { "type": "array", "items": { "$ref": "#/$defs/Hit~1v1" } },
            "again": { "$ref": "#/properties/hits" },
            "children": { "type": "array", "items": { "$ref": "#" } },
            "default": { "$ref": "#/definitions/Hit" },
            "remote": { "$ref": "https://example.com/schema.json" },
        },
        "required": ["hits"],
        "$defs": { "Hit/v1": hit_v1 },
        "definitions": { "Hit": hit },
    }))));
    assert_eq!(
        schema["properties"]["structuredContent"]["$defs"]["Hit/v1"]["default"],
        json!({ "$ref": "#/stays" })
    );
    assert_eq!(
        bake_codemode::render_tool_output_type(Some(&schema)),
        format!(
            "{{ {content} structuredContent?: {{ again?: Array<{{ id: string; }}>; children?: Array<{{ again?: Array<{{ id: string; }}>; children?: Array<unknown>; default?: {{ id: string; }}; hits: Array<{{ id: string; }}>; remote?: unknown; }}>; default?: {{ id: string; }}; hits: Array<{{ id: string; }}>; remote?: unknown; }}; [key: string]: unknown; }}"
        )
    );
}
