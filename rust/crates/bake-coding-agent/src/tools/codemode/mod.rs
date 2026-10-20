//! The `codemode` tool: the model writes JavaScript that calls other tools.
//!
//! Ported from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT;
//! see the crate's `NOTICE`): `src/extensions/codemode/{tool,execute,index}.ts`
//! and the `codemode` settings of `src/core/settings-schema.ts`, with their
//! tests. The engine is [`bake_codemode`]; nested calls go through
//! [`bake_agent::ToolContext::nested`], so a script's calls meet the
//! session's pipeline, permissions, approvals, and sandbox as direct calls
//! do. Only the script's output reaches the model.
//!
//! Scripts use `tools`, `ALL_TOOLS`, `text()`, `image()`, `exit()`,
//! `store()`/`load()`, `console.*`, `return <value>`, and the discovery
//! functions `searchTools()`, `describeTool()`, and `describeNamespace()`,
//! and may start with a `// @options:` line. Results start with a
//! "Script completed" or "Script failed" header. Nested results are handed to
//! the script as Pi hands them:
//!
//! - A tool that declares an output schema resolves to its structured
//!   content, also for error results that carry one (`bash`, `read`, the web
//!   tools, and MCP tools, whose result is the `CallToolResult`).
//! - Any other tool resolves to its text content as one string.
//! - A failed, blocked, or invalid call rejects with an `Error` carrying the
//!   tool's error text.
//!
//! The CLI starts the tool active, a Bake default
//! ([`crate::tools::index::BAKE_DEFAULT_TOOL_NAMES`]) where Pi registers
//! it inactive; a `--tools` list without it, `--no-tools`,
//! `--exclude-tools codemode`, or a `defaultTools` setting that omits it
//! turns it off. While it is active, [`prepare_codemode_loadout`]
//! presents the other tools per the `codemode.mode` setting
//! ([`CodemodeMode`]).
//!
//! Differences from Pi:
//!
//! - Code mode is a default tool, by the owner's decision of 2026-10-20.
//! - No `models` namespace and no grammar-constrained input (D42): the
//!   description is Pi's with `models` off, and the tool has no constrained
//!   sampling.
//! - Bake has no tool exposures: every tool a session declares is `direct`,
//!   and none is `deferred` or reachable only from scripts. With `on` the
//!   `codemode` description therefore lists no tools; with `only` it lists
//!   every tool scripts can call, and their declarations are left out of
//!   requests. A script can call exactly the session's declared tools except
//!   `codemode` (the nested executor's tools, which leave out tools whose
//!   [`AgentTool::nested_callable`] is cleared).
//! - MCP servers are namespaces named `mcp__<server>` with the server's name
//!   as alias, its `description`, and its bounded `instructions`, as Pi's
//!   `registerTools` builds them. Bake keeps `-` (`mcp__my-server`), where
//!   Pi maps it to `_` (`mcp__my_server`).
//! - The output budget counts characters, where Pi counts UTF-16 units; the
//!   catalog's token estimate counts UTF-16 units as Pi does. Namespaces
//!   sort by code point, where Pi uses `localeCompare`.
//! - At most [`MAX_CONCURRENT_NESTED_CALLS`] of a script's tool calls run at
//!   once; the rest wait in the order the script made them. Pi starts every
//!   call at once. Every call is recorded when the script makes it, so one
//!   still waiting when the script ends is listed as cancelled, with the id
//!   `<codemode call id>/?`.
//! - An Esc abort reports `Script aborted: This operation was aborted`, the
//!   message of the default reason of Pi's aborted signal.

mod execute;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bake_agent::{AgentTool, BoxFuture, ToolReplay};
use bake_codemode::{
    ScriptFunction, StoreValues, ToolNamespace, render_tool_output_type, render_tool_sample,
    to_codemode_identifier,
};
use serde_json::{Map, Value, json};

use crate::agent_session::ToolPrompt;
use crate::owned_work::WorkGate;
use crate::session::SessionEntry;

pub use execute::{
    CODEMODE_ABORT_MESSAGE, DEFAULT_MAX_OUTPUT_TOKENS, MAX_CONCURRENT_CODEMODE_SCRIPTS,
    MAX_CONCURRENT_NESTED_CALLS,
};

/// The tool's name.
pub const CODEMODE_TOOL_NAME: &str = "codemode";

/// The custom entry type holding one script's `store()` writes:
/// [`CodemodeStoreEntryData`].
pub const CODEMODE_STORE_ENTRY_TYPE: &str = "codemode-store";

/// The default of `codemode.inlineBudget`, in estimated tokens.
pub const DEFAULT_CODEMODE_INLINE_BUDGET: f64 = 3000.0;

/// Characters per token when estimating the cost of a tool section.
const CHARS_PER_TOKEN: usize = 4;

/// How the active `codemode` tool presents the tools scripts can call, the
/// `codemode.mode` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodemodeMode {
    /// Declared tools stay declared, and their descriptions say how scripts
    /// call them. The default.
    #[default]
    On,
    /// Declared tools scripts can call are listed in the `codemode`
    /// description and left out of requests.
    Only,
}

/// One script's `store()` writes, the data of a `codemode-store` entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodemodeStoreEntryData {
    /// Keys stored with a value.
    pub set: Map<String, Value>,
    /// Keys stored as `undefined`.
    pub delete: Vec<String>,
}

impl CodemodeStoreEntryData {
    /// The entry's JSON, `{ set, delete }`.
    pub fn to_value(&self) -> Value {
        json!({ "set": self.set, "delete": self.delete })
    }
}

/// A group of tools, such as an MCP server's: the engine's namespace and
/// the other names scripts may look it up by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodemodeNamespace {
    /// The name, description, and instructions.
    pub namespace: ToolNamespace,
    /// Other names `describeNamespace()` and `searchTools()`'s `namespace`
    /// option accept, such as an MCP server's own name.
    pub aliases: Vec<String>,
}

/// What the tool needs from its session, Pi's `CodemodeToolOptions` and the
/// session parts of its `ExtensionToolContext`. Without a host, scripts
/// start with an empty store and their writes are dropped.
pub trait CodemodeHost: Send + Sync {
    /// The namespace of a tool, for `searchTools()`, `describeNamespace()`,
    /// and the description's groups.
    fn tool_namespace(&self, name: &str) -> Option<CodemodeNamespace>;
    /// A tool's prompt guidelines, shown with its declaration.
    fn tool_guidelines(&self, name: &str) -> Vec<String>;
    /// A tool's own description when the request declares it with another,
    /// such as the `Codemode:` paragraph [`CodemodeMode::On`] adds; `None`
    /// when the declared description is its own. Scripts see this one in
    /// `ALL_TOOLS`, `describeTool()`, and `searchTools()`.
    fn tool_description(&self, name: &str) -> Option<String>;
    /// What `load()` reads: the `codemode-store` entries on the session's
    /// current branch ([`read_codemode_store`]).
    fn load_store(&self) -> BoxFuture<'static, Result<StoreValues, String>>;
    /// Appends a successful script's writes as one `codemode-store` entry.
    /// An error is reported after the script's output as
    /// `[The codemode store could not be written: <error>]`.
    fn append_store(&self, data: CodemodeStoreEntryData) -> BoxFuture<'static, Result<(), String>>;
    /// The session's gate, which owns the blocking work of a script: the
    /// engine's first compile and the files output is saved to.
    fn work(&self) -> Option<WorkGate>;
}

/// The status of a nested call in a result's details ([`codemode_details`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodemodeNestedCallStatus {
    /// Still running.
    Running,
    /// Succeeded.
    Ok,
    /// Failed.
    Error,
    /// Cut off by the script ending, a timeout, or an abort.
    Cancelled,
}

impl CodemodeNestedCallStatus {
    /// Pi's name of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One nested call of a script, Pi's `CodemodeNestedCall`, for the
/// renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct CodemodeNestedCall {
    /// The nested call's id, `<codemode call id>/<n>`; `<id>/?` until the
    /// call has one.
    pub id: String,
    /// The tool's name.
    pub name: String,
    /// Compact JSON of the arguments, cut at 200 characters.
    pub args: String,
    /// Its status.
    pub status: CodemodeNestedCallStatus,
    /// How long it ran, in milliseconds, once it settled.
    pub duration_ms: Option<f64>,
    /// Its error text, cut at 500 characters.
    pub error: Option<String>,
}

impl CodemodeNestedCall {
    /// Pi's JSON: `{ id, name, args, status, durationMs?, error? }`.
    pub fn to_value(&self) -> Value {
        let mut call = json!({
            "id": self.id,
            "name": self.name,
            "args": self.args,
            "status": self.status.as_str(),
        });
        if let Some(duration) = self.duration_ms {
            call["durationMs"] = json!(duration);
        }
        if let Some(error) = &self.error {
            call["error"] = json!(error);
        }
        call
    }
}

/// The `details` of a codemode result, Pi's `CodemodeToolDetails`:
/// `{ calls, fullOutputPath? }`.
pub fn codemode_details(calls: &[CodemodeNestedCall], full_output_path: Option<&str>) -> Value {
    let mut details = json!({
        "calls": calls.iter().map(CodemodeNestedCall::to_value).collect::<Vec<_>>(),
    });
    if let Some(path) = full_output_path {
        details["fullOutputPath"] = json!(path);
    }
    details
}

/// Pi's `codemodeToolSystemPromptContribution`.
pub fn codemode_tool_prompt() -> ToolPrompt {
    ToolPrompt {
        name: CODEMODE_TOOL_NAME.to_owned(),
        snippet: Some("Run JavaScript that calls other tools".to_owned()),
        guidelines: vec![
            "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.".to_owned(),
        ],
    }
}

/// Pi's `codemodeSchema`.
pub fn codemode_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["code"],
        "properties": {
            "code": { "type": "string", "description": "Raw JavaScript source." },
        },
    })
}

const DESCRIPTION_INTRO: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.
- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.
- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`";

/// Pi's `describeGlobals(false)`: Bake has no `models`.
const GLOBALS: &str = "Globals:
- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.
- `store(key, value)` and `load(key)` keep JSON values across codemode calls.
- `ALL_TOOLS`, `await searchTools(query, { limit?, namespace? })`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.";

/// The output schema of a tool without one: its text.
fn text_output_schema() -> Value {
    json!({ "type": "string" })
}

/// Pi's `toCodemodeDeclaration`: what a script sees of a tool, its
/// description followed by its prompt guidelines, which the system prompt
/// only has for declared tools.
pub fn to_codemode_declaration(tool: &AgentTool, guidelines: &[String]) -> ScriptFunction {
    let bullets: Vec<String> = guidelines
        .iter()
        .map(|guideline| guideline.trim())
        .filter(|guideline| !guideline.is_empty())
        .map(|guideline| format!("- {guideline}"))
        .collect();
    let description = if bullets.is_empty() {
        tool.description.clone()
    } else {
        format!("{}\n\n{}", tool.description.trim(), bullets.join("\n"))
    };
    ScriptFunction {
        name: tool.name.clone(),
        description: Some(description),
        input_schema: Some(tool.parameters.clone()),
        output_schema: Some(
            tool.output_schema
                .clone()
                .unwrap_or_else(text_output_schema),
        ),
        signature: None,
    }
}

/// Pi's `getCodemodeCallableTools`: every given tool except `codemode`.
pub fn codemode_callable_tools(tools: &[Arc<AgentTool>]) -> Vec<Arc<AgentTool>> {
    tools
        .iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .cloned()
        .collect()
}

/// Pi's `CodemodeDescriptionOptions`, without `models`.
#[derive(Debug, Clone, Default)]
pub struct CodemodeDescriptionOptions {
    /// Namespace of each tool, by tool name. Tools of one namespace are
    /// listed under one heading.
    pub namespaces: HashMap<String, CodemodeNamespace>,
    /// Tools that are callable but never listed. Bake has no `deferred`
    /// exposure, so the session passes none.
    pub deferred: HashSet<String>,
    /// Prompt guidelines of each tool, by tool name, listed after its
    /// description.
    pub guidelines: HashMap<String, Vec<String>>,
    /// Estimated tokens (UTF-16 units / 4) the tool sections may use. Tools
    /// that do not fit are left out, like deferred tools. `None` lists every
    /// tool that is not deferred.
    pub inline_budget: Option<f64>,
}

struct CatalogEntry {
    name: String,
    section: String,
    cost: usize,
}

struct CatalogGroup {
    namespace: Option<CodemodeNamespace>,
    entries: Vec<CatalogEntry>,
}

/// Pi's `renderToolSection`: `` ### `id` (`raw name`) `` followed by the
/// tool's description and declaration.
fn render_tool_section(declaration: &ScriptFunction) -> String {
    let id = to_codemode_identifier(&declaration.name);
    let heading = if id == declaration.name {
        format!("### `{id}`")
    } else {
        format!("### `{id}` (`{}`)", declaration.name)
    };
    format!(
        "{heading}\n{}",
        render_tool_sample(declaration, None).trim()
    )
}

/// Pi's `selectCatalog`: in each round every group (tools without a
/// namespace first, then namespaces by name) places its cheapest remaining
/// tool; a group whose next tool does not fit drops out while the others
/// continue.
fn select_catalog(groups: &[CatalogGroup], budget: Option<f64>) -> HashSet<String> {
    let Some(budget) = budget else {
        return groups
            .iter()
            .flat_map(|group| group.entries.iter().map(|entry| entry.name.clone()))
            .collect();
    };
    let mut queues: Vec<Vec<&CatalogEntry>> = groups
        .iter()
        .map(|group| {
            let mut entries: Vec<&CatalogEntry> = group.entries.iter().collect();
            entries.sort_by_key(|entry| entry.cost);
            entries.reverse();
            entries
        })
        .filter(|queue| !queue.is_empty())
        .collect();
    let mut shown = HashSet::new();
    let mut remaining = budget;
    while !queues.is_empty() {
        queues.retain_mut(|queue| {
            // The cheapest is last: sorted by cost, stable, then reversed.
            let Some(next) = queue.last() else {
                return false;
            };
            if next.cost as f64 > remaining {
                return false;
            }
            remaining -= next.cost as f64;
            shown.insert(next.name.clone());
            queue.pop();
            !queue.is_empty()
        });
    }
    shown
}

/// Pi's `createCodemodeDescription` without `models`: the helper list,
/// guidance for finding tools that are not listed, and one section per
/// listed tool, grouped by namespace and limited to the inline budget.
pub fn create_codemode_description(
    tools: &[Arc<AgentTool>],
    options: &CodemodeDescriptionOptions,
) -> String {
    let declarations: Vec<ScriptFunction> = codemode_callable_tools(tools)
        .iter()
        .filter(|tool| !options.deferred.contains(&tool.name))
        .map(|tool| {
            to_codemode_declaration(
                tool,
                options
                    .guidelines
                    .get(&tool.name)
                    .map_or(&[][..], Vec::as_slice),
            )
        })
        .collect();
    let mut groups: Vec<(String, CatalogGroup)> = vec![(
        String::new(),
        CatalogGroup {
            namespace: None,
            entries: Vec::new(),
        },
    )];
    for declaration in &declarations {
        let namespace = options.namespaces.get(&declaration.name);
        let key = namespace.map_or_else(String::new, |namespace| {
            format!("ns:{}", namespace.namespace.name)
        });
        let index = match groups.iter().position(|(known, _)| known == &key) {
            Some(index) => index,
            None => {
                groups.push((
                    key,
                    CatalogGroup {
                        namespace: namespace.cloned(),
                        entries: Vec::new(),
                    },
                ));
                groups.len() - 1
            }
        };
        let section = render_tool_section(declaration);
        let cost = section.encode_utf16().count().div_ceil(CHARS_PER_TOKEN);
        groups[index].1.entries.push(CatalogEntry {
            name: declaration.name.clone(),
            section,
            cost,
        });
    }
    let mut ordered: Vec<CatalogGroup> = groups.into_iter().map(|(_, group)| group).collect();
    ordered.sort_by(|a, b| match (&a.namespace, &b.namespace) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => a.namespace.name.cmp(&b.namespace.name),
    });
    let shown = select_catalog(&ordered, options.inline_budget);

    let mut sections = vec![DESCRIPTION_INTRO.to_owned(), GLOBALS.to_owned()];
    if declarations.is_empty() {
        return sections.join("\n\n");
    }
    let mut tool_sections = vec!["Nested tools:".to_owned()];
    for group in &ordered {
        let visible: Vec<&CatalogEntry> = group
            .entries
            .iter()
            .filter(|entry| shown.contains(&entry.name))
            .collect();
        if let Some(namespace) = &group.namespace {
            // Only tools that did not fit the budget are counted as not
            // listed here.
            let listing = if visible.len() == group.entries.len() {
                ""
            } else if visible.is_empty() {
                " (tools not listed)"
            } else {
                " (some tools not listed)"
            };
            let description = namespace
                .namespace
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty());
            tool_sections.push(format!(
                "## {}{listing}{}",
                namespace.namespace.name,
                description.map_or_else(String::new, |description| format!("\n{description}"))
            ));
        }
        tool_sections.extend(visible.iter().map(|entry| entry.section.clone()));
    }
    sections.push(tool_sections.join("\n\n"));
    sections.join("\n\n")
}

/// Pi's `describeFields`: the field names of an object schema, with the
/// fields of object-typed fields `depth` levels deep.
fn describe_fields(schema: &Value, depth: usize) -> Option<String> {
    let schema = schema.as_object()?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields: Vec<String> = properties
        .iter()
        .map(|(name, field)| {
            let nested = if depth > 0 {
                describe_fields(field, depth - 1)
            } else {
                None
            };
            format!(
                "{name}{}{}",
                if required.contains(name.as_str()) {
                    ""
                } else {
                    "?"
                },
                nested.map_or_else(String::new, |nested| format!(": {nested}"))
            )
        })
        .collect();
    Some(format!("{{ {} }}", fields.join(", ")))
}

/// Pi's `describeOutput`: what a script call resolves to, in one line.
fn describe_output(schema: &Value) -> String {
    let rendered = render_tool_output_type(Some(schema));
    if rendered == "string" {
        return "a string".to_owned();
    }
    let described = describe_fields(schema, 1)
        .unwrap_or_else(|| rendered.split_whitespace().collect::<Vec<_>>().join(" "));
    format!("`{described}`")
}

/// Pi's `describeScriptCall`: a declared tool's description followed by
/// how scripts call it and what the call resolves to.
pub fn describe_script_call(tool: &AgentTool) -> String {
    let declaration = to_codemode_declaration(tool, &[]);
    let output = declaration.output_schema.unwrap_or_else(text_output_schema);
    format!(
        "{}\n\nCodemode: `tools.{}(args)` resolves to {}.",
        tool.description.trim(),
        to_codemode_identifier(&tool.name),
        describe_output(&output)
    )
}

/// The tools of a session as [`prepare_codemode_loadout`] sees them, Pi's
/// `ToolLoadout` without exposures.
pub struct ToolLoadout<'a> {
    /// The tools a request declares, in order, with their own
    /// descriptions; the callable tools are those whose
    /// [`AgentTool::nested_callable`] is set.
    pub declared: &'a [Arc<AgentTool>],
    /// The namespace of a tool.
    pub namespace: &'a dyn Fn(&str) -> Option<CodemodeNamespace>,
    /// A tool's prompt guidelines.
    pub guidelines: &'a dyn Fn(&str) -> Vec<String>,
}

/// What the active `codemode` tool changes in a request, Pi's
/// `ToolLoadoutChanges`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolLoadoutChanges {
    /// Model-facing descriptions of declared tools, by tool name, in
    /// declaration order; `codemode`'s last.
    pub descriptions: Vec<(String, String)>,
    /// Declared tools whose declarations requests leave out. They stay
    /// active and callable, and the transcript still declares them.
    pub hidden_declarations: Vec<String>,
}

/// Pi's `prepareCodemodeLoadout`: with [`CodemodeMode::On`] the
/// descriptions of declared tools scripts can call say how scripts call
/// them, and the `codemode` description lists the callable tools that are
/// not declared (none in Bake); with [`CodemodeMode::Only`] the `codemode`
/// description lists every callable tool, and requests leave out their
/// declarations. Listed tools carry their prompt guidelines.
pub fn prepare_codemode_loadout(
    loadout: &ToolLoadout<'_>,
    mode: CodemodeMode,
    inline_budget: Option<f64>,
) -> ToolLoadoutChanges {
    let callable: Vec<Arc<AgentTool>> = codemode_callable_tools(loadout.declared)
        .into_iter()
        .filter(|tool| tool.nested_callable)
        .collect();
    let mut descriptions = Vec::new();
    if mode == CodemodeMode::On {
        for tool in &callable {
            descriptions.push((tool.name.clone(), describe_script_call(tool)));
        }
    }
    // Every Bake tool is `direct`: with `on` the listing is empty.
    let listed: Vec<Arc<AgentTool>> = match mode {
        CodemodeMode::Only => callable.clone(),
        CodemodeMode::On => Vec::new(),
    };
    let options = CodemodeDescriptionOptions {
        namespaces: listed
            .iter()
            .filter_map(|tool| {
                (loadout.namespace)(&tool.name).map(|namespace| (tool.name.clone(), namespace))
            })
            .collect(),
        deferred: HashSet::new(),
        guidelines: listed
            .iter()
            .map(|tool| (tool.name.clone(), (loadout.guidelines)(&tool.name)))
            .collect(),
        inline_budget: Some(inline_budget.unwrap_or(DEFAULT_CODEMODE_INLINE_BUDGET)),
    };
    descriptions.push((
        CODEMODE_TOOL_NAME.to_owned(),
        create_codemode_description(&listed, &options),
    ));
    ToolLoadoutChanges {
        descriptions,
        hidden_declarations: match mode {
            CodemodeMode::Only => callable.iter().map(|tool| tool.name.clone()).collect(),
            CodemodeMode::On => Vec::new(),
        },
    }
}

/// Pi's `isStoreEntryData`.
fn store_entry_data(data: &Value) -> Option<(&Map<String, Value>, Vec<&str>)> {
    let set = data.get("set")?.as_object()?;
    let deleted = data
        .get("delete")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<&str>>>()?;
    Some((set, deleted))
}

/// Pi's `readCodemodeStore`: what `load()` reads, the `codemode-store`
/// entries on `branch` applied from the root. Malformed entries are skipped.
pub fn read_codemode_store(branch: &[&SessionEntry]) -> StoreValues {
    let mut store = StoreValues::new();
    for entry in branch {
        if entry.entry_type() != "custom"
            || entry.get("customType").and_then(Value::as_str) != Some(CODEMODE_STORE_ENTRY_TYPE)
        {
            continue;
        }
        let Some((set, deleted)) = entry.get("data").and_then(store_entry_data) else {
            continue;
        };
        for key in deleted {
            store.shift_remove(key);
        }
        for (key, value) in set {
            store.insert(key.clone(), value.clone());
        }
    }
    store
}

/// Pi's `createCodemodeToolDefinition`: the tool, whose description lists
/// no tools until [`prepare_codemode_loadout`] replaces it. It is never
/// replayed, and scripts cannot call it.
pub fn create_codemode_tool(host: Option<Arc<dyn CodemodeHost>>) -> AgentTool {
    let mut tool = AgentTool::new(
        CODEMODE_TOOL_NAME,
        CODEMODE_TOOL_NAME,
        create_codemode_description(&[], &CodemodeDescriptionOptions::default()),
        codemode_parameters(),
        move |id, params, signal, on_update, ctx| {
            let host = host.clone();
            Box::pin(execute::execute_codemode(
                id, params, signal, on_update, ctx, host,
            ))
        },
    );
    // A script's nested calls may write, run commands, or leave the machine.
    tool.replay = Some(ToolReplay::Never);
    // Scripts must not start other scripts.
    tool.nested_callable = false;
    tool
}

#[cfg(test)]
mod tests;
