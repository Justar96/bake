//! The self-contained HTML export `/share` uploads, adapted from Pi v1.3.0
//! `packages/coding-agent/src/core/export-html/index.ts` (`generateHtml`,
//! with v1.4.0's CJK emphasis script, `generateThemeVars`,
//! `deriveExportColors`, and the session data of `exportSessionToHtml`).
//!
//! The page is Pi's: `template.html`, `template.css`, and `template.js` are
//! Pi v1.5.2's files (system messages rendered where they occur, trees
//! walked without recursion, and recorded cost totals), and
//! `vendor/marked.min.js`, `vendor/marked-cjk-friendly.min.js` (both MIT),
//! and `vendor/highlight.min.js` (BSD-3-Clause) are the copies Pi vendors,
//! all embedded verbatim; the crate's `NOTICE` reproduces their licenses. The
//! session travels base64-encoded as JSON, and the page's own script renders
//! it in the browser, so nothing here interprets message content.
//!
//! Bake has one theme for the export: Pi's `dark` theme, its colors resolved
//! through Pi's `theme.ts` and `packages/tui/src/colors.ts` once, when this
//! port was made, into [`DARK_THEME_COLORS`] and [`DARK_EXPORT_COLORS`].
//! Placeholders are replaced literally: Pi's `String.replace` reads `$&` in
//! the inserted text as a pattern, which rewrites one of highlight.js's
//! regular expressions in Pi's pages; here the vendored text is inserted as
//! it is. Pi's pre-rendered custom tool HTML (`renderedTools`) is not
//! produced: Bake's tools are rendered by the template.

use base64::Engine;
use serde_json::{Map, Value};

const TEMPLATE_HTML: &str = include_str!("template.html");
const TEMPLATE_CSS: &str = include_str!("template.css");
const TEMPLATE_JS: &str = include_str!("template.js");
const MARKED_JS: &str = include_str!("vendor/marked.min.js");
const MARKED_CJK_FRIENDLY_JS: &str = include_str!("vendor/marked-cjk-friendly.min.js");
const HIGHLIGHT_JS: &str = include_str!("vendor/highlight.min.js");

/// Pi's `dark` theme tokens as `getResolvedThemeColors("dark")` gives them:
/// foreground tokens, then background tokens, as hex.
pub const DARK_THEME_COLORS: &[(&str, &str)] = &[
    ("accent", "#a798d7"),
    ("border", "#5fa8cc"),
    ("borderAccent", "#a08ed5"),
    ("borderMuted", "#768186"),
    ("success", "#68b78d"),
    ("error", "#ea7f81"),
    ("warning", "#cd9a22"),
    ("muted", "#9da5a9"),
    ("dim", "#7e888e"),
    ("text", "#dee0e1"),
    ("thinkingText", "#96a0a4"),
    ("scrollbarTrack", "#484e52"),
    ("scrollbarThumb", "#97a0a5"),
    ("searchMatchText", "#9da5a9"),
    ("userMessageText", "#dee0e1"),
    ("customMessageText", "#9da5a9"),
    ("customMessageLabel", "#a798d7"),
    ("toolTitle", "#dee0e1"),
    ("toolOutput", "#9da5a9"),
    ("mdHeading", "#cd9a22"),
    ("mdLink", "#69add0"),
    ("mdLinkUrl", "#9da5a9"),
    ("mdCode", "#a798d7"),
    ("mdCodeBlock", "#68b78d"),
    ("mdCodeBlockBorder", "#9da5a9"),
    ("mdQuote", "#9da5a9"),
    ("mdQuoteBorder", "#9da5a9"),
    ("mdHr", "#9da5a9"),
    ("mdListBullet", "#a798d7"),
    ("toolDiffAdded", "#68b78d"),
    ("toolDiffRemoved", "#ea7f81"),
    ("toolDiffContext", "#9da5a9"),
    ("syntaxComment", "#9da5a9"),
    ("syntaxKeyword", "#69add0"),
    ("syntaxFunction", "#cd9a22"),
    ("syntaxVariable", "#5db3ba"),
    ("syntaxString", "#de8d5a"),
    ("syntaxNumber", "#68b78d"),
    ("syntaxType", "#a798d7"),
    ("syntaxOperator", "#9da5a9"),
    ("syntaxPunctuation", "#9da5a9"),
    ("thinkingOff", "#6c767b"),
    ("thinkingMinimal", "#68808d"),
    ("thinkingLow", "#5489a4"),
    ("thinkingMedium", "#6185cc"),
    ("thinkingHigh", "#9776e5"),
    ("thinkingXhigh", "#de54c1"),
    ("thinkingMax", "#fe5462"),
    ("bashMode", "#5eb286"),
    ("selectedBg", "#213b49"),
    ("searchMatchBg", "#4e2f1b"),
    ("userMessageBg", "#213b49"),
    ("customMessageBg", "#3a3055"),
    ("toolPendingBg", "#34383a"),
    ("toolSuccessBg", "#254131"),
    ("toolErrorBg", "#5b282a"),
];

/// The `dark` theme's explicit `export` colors, Pi's
/// `getThemeExportColors("dark")`: page, card, and info backgrounds.
pub const DARK_EXPORT_COLORS: ExportColors = ExportColors {
    page_bg: "#21252c",
    card_bg: "#282c34",
    info_bg: "#4e2f1b",
};

/// The three backgrounds of an exported page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportColors {
    pub page_bg: &'static str,
    pub card_bg: &'static str,
    pub info_bg: &'static str,
}

/// Pi's `generateThemeVars`: one CSS custom property per theme token, then
/// the export backgrounds, joined as Pi joins them.
fn theme_vars() -> String {
    let mut lines: Vec<String> = DARK_THEME_COLORS
        .iter()
        .map(|(token, color)| format!("--{token}: {color};"))
        .collect();
    lines.push(format!("--exportPageBg: {};", DARK_EXPORT_COLORS.page_bg));
    lines.push(format!("--exportCardBg: {};", DARK_EXPORT_COLORS.card_bg));
    lines.push(format!("--exportInfoBg: {};", DARK_EXPORT_COLORS.info_bg));
    lines.join("\n      ")
}

/// What an export shows, Pi's `SessionData`: the header, every entry in
/// file order, the leaf, and, when the session has them, its system prompt
/// and tool definitions.
pub struct SessionData {
    pub header: Option<Value>,
    pub entries: Vec<Value>,
    pub leaf_id: Option<String>,
    pub system_prompt: Option<String>,
    /// Each tool's `name`, `description`, and `parameters`.
    pub tools: Option<Vec<Value>>,
}

impl SessionData {
    /// The JSON the page decodes, with Pi's member order; absent members
    /// are omitted as `JSON.stringify` omits `undefined`.
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("header".into(), self.header.clone().unwrap_or(Value::Null));
        object.insert("entries".into(), Value::Array(self.entries.clone()));
        object.insert(
            "leafId".into(),
            self.leaf_id.clone().map_or(Value::Null, Value::String),
        );
        if let Some(prompt) = &self.system_prompt {
            object.insert("systemPrompt".into(), Value::String(prompt.clone()));
        }
        if let Some(tools) = &self.tools {
            object.insert("tools".into(), Value::Array(tools.clone()));
        }
        Value::Object(object)
    }
}

/// A tool's definition as an export lists it, Pi's
/// `{ name, description, parameters }`.
pub fn tool_definition(name: &str, description: &str, parameters: &Value) -> Value {
    let mut object = Map::new();
    object.insert("name".into(), name.into());
    object.insert("description".into(), description.into());
    object.insert("parameters".into(), parameters.clone());
    Value::Object(object)
}

/// Pi's `generateHtml`: the page with its styles, scripts, and the session
/// data filled in.
pub fn generate_html(data: &SessionData) -> String {
    let json = data.to_json().to_string();
    let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    let css = TEMPLATE_CSS
        .replacen("{{THEME_VARS}}", &theme_vars(), 1)
        .replacen("{{BODY_BG}}", DARK_EXPORT_COLORS.page_bg, 1)
        .replacen("{{CONTAINER_BG}}", DARK_EXPORT_COLORS.card_bg, 1)
        .replacen("{{INFO_BG}}", DARK_EXPORT_COLORS.info_bg, 1);
    TEMPLATE_HTML
        .replacen("{{CSS}}", &css, 1)
        .replacen("{{JS}}", TEMPLATE_JS, 1)
        .replacen("{{SESSION_DATA}}", &encoded, 1)
        .replacen("{{MARKED_JS}}", MARKED_JS, 1)
        .replacen("{{MARKED_CJK_FRIENDLY_JS}}", MARKED_CJK_FRIENDLY_JS, 1)
        .replacen("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decoded(html: &str) -> Value {
        let start = html
            .find(r#"<script id="session-data" type="application/json">"#)
            .expect("data script")
            + r#"<script id="session-data" type="application/json">"#.len();
        let end = start + html[start..].find("</script>").expect("end");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[test]
    fn the_page_carries_the_session_and_every_template_part() {
        let data = SessionData {
            header: Some(json!({"type": "session", "id": "s1"})),
            entries: vec![json!({"type": "message", "id": "e1",
                "message": {"role": "user", "content": "</script><script>alert(1)</script>"}})],
            leaf_id: Some("e1".into()),
            system_prompt: Some("You are Bake.".into()),
            tools: Some(vec![tool_definition("read", "Read a file", &json!({}))]),
        };
        let html = generate_html(&data);
        for placeholder in [
            "{{CSS}}",
            "{{JS}}",
            "{{SESSION_DATA}}",
            "{{MARKED_JS}}",
            "{{MARKED_CJK_FRIENDLY_JS}}",
            "{{HIGHLIGHT_JS}}",
            "{{THEME_VARS}}",
            "{{BODY_BG}}",
        ] {
            assert!(
                !html.contains(placeholder),
                "{placeholder} left in the page"
            );
        }
        // The message text is inside the base64 data, never raw markup.
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("--userMessageBg: #213b49;"));
        assert!(html.contains("--body-bg: #21252c;"));
        assert!(html.contains("marked v18.0.11"));
        assert!(html.contains("marked-cjk-friendly v0.1.2"));
        assert!(html.contains("Highlight.js v11.9.0"));
        // highlight.js's `$&` stays as written (Pi's `replace` rewrites it).
        assert!(html.contains("[-+*\\/?!$&|:<=>@^~]"));
        let value = decoded(&html);
        assert_eq!(value["leafId"], "e1");
        assert_eq!(value["systemPrompt"], "You are Bake.");
        assert_eq!(value["tools"][0]["name"], "read");
        assert_eq!(
            value["entries"][0]["message"]["content"],
            "</script><script>alert(1)</script>"
        );
        let keys: Vec<&String> = value.as_object().expect("object").keys().collect();
        assert_eq!(
            keys,
            ["header", "entries", "leafId", "systemPrompt", "tools"]
        );
    }

    #[test]
    fn absent_prompt_and_tools_are_omitted() {
        let data = SessionData {
            header: None,
            entries: Vec::new(),
            leaf_id: None,
            system_prompt: None,
            tools: None,
        };
        let value = data.to_json();
        assert_eq!(
            value,
            json!({"header": null, "entries": [], "leafId": null})
        );
    }

    // export-html-xss.test.ts: the template's link and image renderers
    // sanitize URLs and escape attributes.
    #[test]
    fn the_template_keeps_pi_s_markdown_url_sanitizing() {
        assert!(TEMPLATE_JS.contains("sanitizeMarkdownUrl(token.href)"));
        assert!(TEMPLATE_JS.contains("replace(/[\\x00-\\x1f\\x7f]/g, '')"));
        assert!(TEMPLATE_JS.contains("escapeHtml(href)"));
        assert!(!TEMPLATE_JS.contains("${img.mimeType}"));
    }

    /// Pi v1.4.0's `test/export-html-cjk.test.ts` (#10154): the page's own
    /// scripts, run by Node as Pi's test runs them, render emphasis next to
    /// CJK punctuation and keep CommonMark's rules for other text. Skipped
    /// without `node`.
    #[test]
    fn renders_emphasis_next_to_cjk_punctuation() {
        let Some(node) = crate::tools::process::find_executable(&["node"]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        let dir = crate::test_support::TempDir::new("export-cjk");
        let page = dir.join("out.html");
        let data = SessionData {
            header: Some(json!({"type": "session", "id": "cjk-test"})),
            entries: Vec::new(),
            leaf_id: None,
            system_prompt: None,
            tools: None,
        };
        std::fs::write(&page, generate_html(&data)).expect("write page");
        let cases = [
            (
                "**这是测试。**后面",
                "<p><strong>这是测试。</strong>后面</p>\n",
            ),
            (
                "**一句话说清：**一台机器",
                "<p><strong>一句话说清：</strong>一台机器</p>\n",
            ),
            (
                "**要点到此为止。**20 小时",
                "<p><strong>要点到此为止。</strong>20 小时</p>\n",
            ),
            ("字**「引用」**字", "<p>字<strong>「引用」</strong>字</p>\n"),
            ("*斜体。*后面", "<p><em>斜体。</em>后面</p>\n"),
            ("**end.**Then next", "<p>**end.**Then next</p>\n"),
        ];
        // Pi's `loadMarkdownParser`: every script but the viewer runs in a
        // bare context, and the viewer returns its parser before it renders.
        let script = r#"
const { readFileSync } = require("node:fs");
const { createContext, runInContext } = require("node:vm");
const html = readFileSync(process.argv[1], "utf8");
const sessionData = html.match(/<script id="session-data" type="application\/json">([^<]+)<\/script>/)?.[1];
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((match) => match[1]);
const viewer = scripts.pop();
const context = createContext({
  atob,
  TextDecoder,
  URLSearchParams,
  document: { getElementById: () => ({ textContent: sessionData }), querySelector: () => null },
  window: { location: { search: "" } },
});
for (const script of scripts) runInContext(script, context);
const parse = runInContext(
  viewer.replace("function safeMarkedParse(", "return safeMarkedParse; function safeMarkedParse("),
  context,
);
process.stdout.write(JSON.stringify(JSON.parse(process.argv[2]).map((text) => parse(text))));
"#;
        let inputs = serde_json::to_string(&cases.map(|(input, _)| input)).expect("inputs");
        let output = std::process::Command::new(node)
            .arg("-e")
            .arg(script)
            .arg(&page)
            .arg(inputs)
            .output()
            .expect("node");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rendered: Vec<String> = serde_json::from_slice(&output.stdout).expect("rendered");
        assert_eq!(rendered, cases.map(|(_, html)| html));
    }

    /// Runs `script` with Node, `args` after it, and parses what it prints
    /// as JSON; `None`, with a note, when `node` is not on the PATH.
    fn node_json(script: &str, args: &[&std::ffi::OsStr]) -> Option<Value> {
        let Some(node) = crate::tools::process::find_executable(&["node"]) else {
            eprintln!("skipped: node is not on PATH");
            return None;
        };
        let output = std::process::Command::new(node)
            .arg("-e")
            .arg(script)
            .args(args)
            .output()
            .expect("node");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(serde_json::from_slice(&output.stdout).expect("JSON from node"))
    }

    /// The template file as embedded, for scripts that read it as Pi's
    /// tests read `template.js`.
    fn template_js_path() -> &'static std::ffi::OsStr {
        std::ffi::OsStr::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/export_html/template.js"
        ))
    }

    /// Pi v1.5.2's `test/export-html-deep-tree.test.ts` (#11225), "builds
    /// and flattens a single-child chain deeper than the call stack": a
    /// page of 20,000 chained entries, whose viewer Node runs as Pi's test
    /// runs it, builds, flattens, and walks the tree without recursion.
    /// Skipped without `node`.
    #[test]
    fn a_session_deeper_than_the_call_stack_renders() {
        const DEPTH: usize = 20_000;
        let iso = |seconds: usize| {
            format!(
                "1970-01-01T{:02}:{:02}:{:02}.000Z",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        };
        let entries = (0..DEPTH)
            .map(|i| {
                json!({
                    "type": "message",
                    "id": format!("e{i}"),
                    "parentId": if i == 0 { Value::Null } else { json!(format!("e{}", i - 1)) },
                    "timestamp": iso(i),
                    "message": { "role": "user", "content": "x", "timestamp": i * 1000 },
                })
            })
            .collect();
        let data = SessionData {
            header: Some(json!({
                "type": "session", "version": 3, "id": "deep-test",
                "timestamp": iso(0), "cwd": "/tmp",
            })),
            entries,
            leaf_id: Some(format!("e{}", DEPTH - 1)),
            system_prompt: None,
            tools: None,
        };
        let dir = crate::test_support::TempDir::new("export-deep");
        let page = dir.join("out.html");
        std::fs::write(&page, generate_html(&data)).expect("write page");
        let script = r#"
const { readFileSync } = require("node:fs");
const { createContext, runInContext } = require("node:vm");
const html = readFileSync(process.argv[1], "utf8");
const depth = Number(process.argv[2]);
const sessionData = html.match(/<script id="session-data" type="application\/json">([^<]+)<\/script>/)?.[1];
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((match) => match[1]);
const viewer = scripts.pop();
if (!sessionData || !viewer) throw new Error("Missing exported session data or viewer script");
const context = createContext({
  atob,
  TextDecoder,
  URLSearchParams,
  document: { getElementById: () => ({ textContent: sessionData }), querySelector: () => null },
  window: { location: { search: "" } },
});
const api = runInContext(
  viewer.replace(
    "let treeNodeMap = null;",
    "let treeNodeMap = null; return { buildTree, buildActivePathIds, flattenTree, findNewestLeaf, getPath };",
  ),
  context,
);
const tree = api.buildTree();
const leafId = `e${depth - 1}`;
const flat = api.flattenTree(tree, api.buildActivePathIds(leafId));
const path = api.getPath(leafId);
process.stdout.write(JSON.stringify({
  roots: tree.length,
  flat: flat.length,
  flatLast: flat[depth - 1].node.entry.id,
  newestLeaf: api.findNewestLeaf("e0"),
  path: path.length,
  pathFirst: path[0].id,
  pathLast: path[depth - 1].id,
}));
"#;
        let depth = DEPTH.to_string();
        let Some(walked) = node_json(script, &[page.as_os_str(), depth.as_ref()]) else {
            return;
        };
        let leaf = format!("e{}", DEPTH - 1);
        assert_eq!(
            walked,
            json!({
                "roots": 1, "flat": DEPTH, "flatLast": leaf, "newestLeaf": leaf,
                "path": DEPTH, "pathFirst": "e0", "pathLast": leaf,
            })
        );
    }

    /// Pi v1.5.2's `test/export-html-system-messages.test.ts` (#11261): the
    /// template's system-message helpers, run by Node on the slice of
    /// `template.js` Pi's test runs, render a system message's text as
    /// `bake-ai` sends it to the model, render the first system entry on a
    /// branch as the prompt and later ones as updates, and count a
    /// compaction's snapshot as earlier system state. Skipped without
    /// `node`.
    #[test]
    fn system_messages_render_as_the_model_receives_them() {
        let initial = json!({
            "role": "system",
            "content": "You are a helpful assistant.",
            "sections": {
                "rules": "<rules>\nbe concise\n</rules>",
                "project_context": "<project_context>\nv1\n</project_context>",
            },
            "toolsAdded": [{ "name": "read", "description": "Read a file", "parameters": { "type": "object", "properties": {} } }],
            "timestamp": 1,
        });
        let update = json!({
            "role": "system",
            "content": [{ "type": "text", "text": "Extra instructions" }],
            "sections": { "project_context": "<project_context>\nv2\n</project_context>", "rules": null },
            "toolsAdded": [{ "name": "bash", "description": "Run a command", "parameters": { "type": "object", "properties": {} } }],
            "toolsRemoved": [{ "name": "read" }],
            "timestamp": 2,
        });
        let input = json!({
            "initial": initial,
            "update": update,
            "entries": [
                { "type": "message", "id": "s1", "parentId": null, "message": initial },
                { "type": "message", "id": "u1", "parentId": "s1", "message": { "role": "user", "content": "hi" } },
                { "type": "message", "id": "s2", "parentId": "u1", "message": update },
            ],
            "snapshotEntries": [
                { "type": "compaction", "id": "c1", "parentId": null, "systemMessage": initial },
                { "type": "message", "id": "s2", "parentId": "c1", "message": update },
            ],
        });
        let script = r#"
const { readFileSync } = require("node:fs");
const { runInNewContext } = require("node:vm");
const templateJs = readFileSync(process.argv[1], "utf8");
const input = JSON.parse(process.argv[2]);
function loadSystemHelpers(entries) {
  const start = templateJs.indexOf("function systemContentText(content)");
  const end = templateJs.indexOf("const hasSystemEntries");
  if (start < 0 || end <= start) throw new Error("the system-message helpers moved");
  const context = { entries };
  runInNewContext(
    `const byId = new Map(entries.map(entry => [entry.id, entry]));
    ${templateJs.slice(start, end)}
    helpers = { getSystemMessageText, renderSystemMessageUpdate, isLeadingSystemEntry, describeSystemUpdate };`,
    context,
  );
  return context.helpers;
}
const helpers = loadSystemHelpers(input.entries);
const snapshot = loadSystemHelpers(input.snapshotEntries);
process.stdout.write(JSON.stringify({
  initialText: helpers.getSystemMessageText(input.initial),
  updateText: helpers.renderSystemMessageUpdate(input.update),
  leading: [helpers.isLeadingSystemEntry(input.entries[0]), helpers.isLeadingSystemEntry(input.entries[2])],
  described: helpers.describeSystemUpdate(input.update),
  afterSnapshot: snapshot.isLeadingSystemEntry({ id: "s2", parentId: "c1" }),
}));
"#;
        let input_text = input.to_string();
        let Some(rendered) = node_json(script, &[template_js_path(), input_text.as_ref()]) else {
            return;
        };
        let message = |value: &Value| -> bake_ai::SystemMessage {
            serde_json::from_value(value.clone()).expect("system message")
        };
        // "renders system message text the same way the model receives it"
        assert_eq!(
            rendered["initialText"],
            bake_ai::utils::text::get_system_message_text(&message(&input["initial"]))
        );
        assert_eq!(
            rendered["updateText"],
            bake_ai::utils::text::render_system_message_update(&message(&input["update"]))
        );
        // "renders the first system entry on a branch as the prompt and
        // later ones as updates"
        assert_eq!(rendered["leading"], json!([true, false]));
        assert_eq!(
            rendered["described"],
            "instructions, project_context, -rules, tools +bash -read"
        );
        // "treats a compaction snapshot as earlier system state"
        assert_eq!(rendered["afterSnapshot"], false);
    }

    // The same file's "renders system entries in the transcript": the
    // template renders system entries where they occur, and falls back to
    // the header's prompt and tools only for sessions without them.
    #[test]
    fn the_template_renders_system_entries_in_the_transcript() {
        let renders = TEMPLATE_JS
            .match_indices("if (msg.role === 'system') {")
            .any(|(at, opening)| {
                TEMPLATE_JS[at + opening.len()..]
                    .trim_start()
                    .starts_with("let html = '';")
            });
        assert!(renders);
        assert!(TEMPLATE_JS.contains("renderSystemPromptBlock('System Prompt', text)"));
        assert!(TEMPLATE_JS.contains("renderToolsList('Available Tools', msg.toolsAdded)"));
        assert!(TEMPLATE_JS.contains("if (!hasSystemEntries) {"));
    }

    /// Pi v1.5.2's `test/export-html-cost.test.ts` (#9980), "uses recorded
    /// totals while falling back to component sums for old sessions": the
    /// header's cost is each message's recorded total, or the sum of its
    /// parts when it has none. Skipped without `node`.
    #[test]
    fn the_cost_uses_recorded_totals() {
        let entries = json!([
            {
                "type": "message",
                "message": {
                    "role": "assistant", "content": [],
                    "usage": { "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0, "cacheWrite": 0, "total": 0.9 } },
                },
            },
            {
                "type": "message",
                "message": {
                    "role": "assistant", "content": [],
                    "usage": { "cost": { "input": 0.4, "output": 0.1, "cacheRead": 0, "cacheWrite": 0 } },
                },
            },
        ]);
        let script = r#"
const { readFileSync } = require("node:fs");
const { runInNewContext } = require("node:vm");
const templateJs = readFileSync(process.argv[1], "utf8");
const start = templateJs.indexOf("function computeStats(entryList)");
const end = templateJs.indexOf("const globalStats = computeStats(entries);");
if (start < 0 || end <= start) throw new Error("computeStats moved");
const context = {};
runInNewContext(`${templateJs.slice(start, end)}
result = computeStats(${process.argv[2]});`, context);
process.stdout.write(JSON.stringify(context.result.cost));
"#;
        let entries_text = entries.to_string();
        let Some(cost) = node_json(script, &[template_js_path(), entries_text.as_ref()]) else {
            return;
        };
        let close = |member: &str, expected: f64| {
            let actual = cost[member].as_f64().expect("number");
            assert!((actual - expected).abs() < 1e-9, "{member}: {actual}");
        };
        close("input", 0.5);
        close("output", 0.3);
        close("total", 1.4);
    }
}
