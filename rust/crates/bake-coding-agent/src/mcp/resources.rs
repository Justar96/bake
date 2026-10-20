//! MCP resources, through the three tools Codex and opencode use, so a model
//! trained on them uses them unchanged:
//! [`LIST_MCP_RESOURCES_TOOL`], [`LIST_MCP_RESOURCE_TEMPLATES_TOOL`], and
//! [`READ_MCP_RESOURCE_TOOL`].
//!
//! Adapted from Pi v1.3.0 `src/extensions/mcp/resources.ts`
//! (`createMcpResourceToolDefinitions`, `isMcpAppResource`, `listed`,
//! `jsonResult`, and its schemas) and the resource registration of
//! `src/extensions/mcp/index.ts` (`serversWithResources`,
//! `syncResourceTools`); MIT, see the crate `NOTICE`. The names, parameter
//! and output schemas, descriptions, payload shapes, and messages are Pi's.
//!
//! The three tools are one shared set that reaches every connected server
//! that declared the `resources` capability and whose exposure is not
//! `hidden` ([`super::McpServers`] publishes and withdraws them with the
//! servers' generations, so their appearance and disappearance are logged
//! like any MCP tool's). A session whose servers declare no resources
//! declares none of them.
//!
//! Listings are JSON, as in Codex: with a `server`, one page, which `cursor`
//! continues; without one, every page of every server with its listing
//! errors. MCP App resources (`ui://` URIs and `profile=mcp-app` media
//! types) and icons are left out: they are user interfaces for hosts that
//! render them, and so is a `server` key an entry of the server's own
//! carries, which is Bake's attribution, not the server's data. A read becomes
//! model-facing text and images through
//! [`super::tools`]'s conversion, so a binary resource is saved to a private
//! file the note names and text over [`super::MCP_OUTPUT_MAX_BYTES`] keeps
//! its start and end beside the file holding all of it. Scripts receive the
//! JSON payload as the tool's structured content.
//!
//! Deviations from Pi, both because a `bake-rs` session owns its work
//! ([`WorkGate`]) rather than spawning unowned promises:
//!
//! - a listing without a `server` lists the servers one after another, where
//!   Pi runs them under `Promise.allSettled`; the order of the listing and
//!   its errors is Pi's, by server name;
//! - the call's abort signal is checked between servers and ends the call,
//!   where Pi passes it into each request.
//!
//! Replay class: all three are [`ToolReplay::Safe`], and all three declare
//! `read_only`. Unlike an `mcp__<server>__<tool>` call, whose effect is
//! whatever the server's own `readOnlyHint` claims it is, each of these
//! sends one MCP read method that Bake, not the server, chose
//! (`resources/list`, `resources/templates/list`, `resources/read`), which
//! the protocol defines as a read and which Pi annotates `readOnlyHint` in
//! its own code. A selected-run rerun therefore repeats a read, not an
//! effect; a server that acts on a read anyway is outside what any client
//! can check.

use std::future::Future;
use std::sync::Arc;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::{AbortSignal, UserContentBlock};
use bake_mcp::{ReadResourceResult, Resource, ResourcesPage};
use serde_json::{Map, Value, json};

use super::tools::{Caller, limit_content, text, to_model_content};
use crate::owned_work::WorkGate;

/// Codex's tool that lists a server's resources.
pub const LIST_MCP_RESOURCES_TOOL: &str = "list_mcp_resources";
/// Codex's tool that lists a server's resource templates.
pub const LIST_MCP_RESOURCE_TEMPLATES_TOOL: &str = "list_mcp_resource_templates";
/// Codex's tool that reads one resource.
pub const READ_MCP_RESOURCE_TOOL: &str = "read_mcp_resource";

/// The three tool names, in the order they are published.
pub const RESOURCE_TOOL_NAMES: [&str; 3] = [
    LIST_MCP_RESOURCES_TOOL,
    LIST_MCP_RESOURCE_TEMPLATES_TOOL,
    READ_MCP_RESOURCE_TOOL,
];

/// The connected servers the tools reach, read at call time, as Pi's
/// `servers: () => readonly McpResourceServer[]`.
pub(crate) type ResourceServers = Arc<dyn Fn() -> Vec<Arc<Caller>> + Send + Sync>;

/// Pi's `LIST_PARAMETERS`.
fn list_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name. Omit to list every server with resources.",
            },
            "cursor": {
                "type": "string",
                "description": "Opaque cursor from a previous call with the same server; omit for the first page.",
            },
        },
        "additionalProperties": false,
    })
}

/// Pi's `READ_PARAMETERS`.
fn read_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources.",
            },
            "uri": {
                "type": "string",
                "description": "Resource URI to read. Must be one of the URIs returned by list_mcp_resources.",
            },
        },
        "required": ["server", "uri"],
        "additionalProperties": false,
    })
}

/// Pi's `LISTING_ERRORS`.
fn listing_errors() -> Value {
    json!({
        "type": "array",
        "description": "Servers that could not be listed",
        "items": {
            "type": "object",
            "properties": { "server": { "type": "string" }, "error": { "type": "string" } },
            "required": ["server", "error"],
        },
    })
}

/// Pi's `LIST_OUTPUT_SCHEMA`.
fn list_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resources": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string" },
                        "uri": { "type": "string" },
                        "name": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "mimeType": { "type": "string" },
                        "size": { "type": "number" },
                    },
                    "required": ["server", "uri", "name"],
                },
            },
            "nextCursor": { "type": "string" },
            "errors": listing_errors(),
        },
        "required": ["resources"],
    })
}

/// Pi's `LIST_TEMPLATES_OUTPUT_SCHEMA`.
fn list_templates_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resourceTemplates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string" },
                        "uriTemplate": { "type": "string", "description": "RFC 6570 URI template" },
                        "name": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "mimeType": { "type": "string" },
                    },
                    "required": ["server", "uriTemplate", "name"],
                },
            },
            "nextCursor": { "type": "string" },
            "errors": listing_errors(),
        },
        "required": ["resourceTemplates"],
    })
}

/// Pi's `READ_OUTPUT_SCHEMA`.
fn read_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "uri": { "type": "string" },
            "contents": {
                "type": "array",
                "items": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "uri": { "type": "string" },
                                "mimeType": { "type": "string" },
                                "text": { "type": "string" },
                            },
                            "required": ["uri", "text"],
                        },
                        {
                            "type": "object",
                            "properties": {
                                "uri": { "type": "string" },
                                "mimeType": { "type": "string" },
                                "blob": { "type": "string", "description": "base64" },
                            },
                            "required": ["uri", "blob"],
                        },
                    ],
                },
            },
        },
        "required": ["server", "uri", "contents"],
    })
}

const LIST_RESOURCES_DESCRIPTION: &str = "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.";
const LIST_TEMPLATES_DESCRIPTION: &str = "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.";
const READ_RESOURCE_DESCRIPTION: &str =
    "Read a specific resource from an MCP server given the server name and resource URI.";

/// Pi's `isMcpAppResource`: a user interface only a host that renders it can
/// use.
fn is_mcp_app_resource(entry: &Map<String, Value>) -> bool {
    let field = |name: &str| entry.get(name).and_then(Value::as_str).unwrap_or_default();
    let address = {
        let uri = field("uri");
        if uri.is_empty() {
            field("uriTemplate")
        } else {
            uri
        }
    };
    address.starts_with("ui://") || is_mcp_app_media_type(field("mimeType"))
}

/// Pi's `/;\s*profile\s*=\s*"?mcp-app"?/i` over a media type: any `;`
/// parameter named `profile` whose value starts with `mcp-app`.
fn is_mcp_app_media_type(mime_type: &str) -> bool {
    let lowered = mime_type.to_ascii_lowercase();
    lowered.split(';').skip(1).any(|parameter| {
        let Some(value) = parameter.trim_start().strip_prefix("profile") else {
            return false;
        };
        let Some(value) = value.trim_start().strip_prefix('=') else {
            return false;
        };
        let value = value.trim_start();
        value
            .strip_prefix('"')
            .unwrap_or(value)
            .starts_with("mcp-app")
    })
}

/// Pi's `listed`: the entry without `_meta` and icons, tagged with its
/// server.
///
/// Deviation from Pi, whose `listed` spreads the entry over the `server` it
/// set: a `server` key of the server's own is left out. The key is Bake's
/// attribution and the one the model passes back to
/// [`READ_MCP_RESOURCE_TOOL`], so a server that sent its own would steer a
/// read at another configured server.
fn listed(server: &str, resource: &Resource) -> Value {
    let mut entry = Map::new();
    entry.insert("server".into(), server.into());
    for (key, value) in resource.entry() {
        if key != "_meta" && key != "icons" && key != "server" {
            entry.insert(key.clone(), value.clone());
        }
    }
    Value::Object(entry)
}

/// Pi's `stringArgument`: a trimmed non-empty string, or nothing; a value
/// that is not a string is refused, as Pi throws.
fn string_argument(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            Ok((!value.is_empty()).then(|| value.to_owned()))
        }
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// Pi's `findServer`.
fn find_server(servers: &[Arc<Caller>], name: &str) -> Result<Arc<Caller>, String> {
    if let Some(server) = servers.iter().find(|server| server.server == name) {
        return Ok(Arc::clone(server));
    }
    let available: Vec<&str> = servers
        .iter()
        .map(|server| server.server.as_str())
        .collect();
    let available = if available.is_empty() {
        String::new()
    } else {
        format!(". Servers with resources: {}", available.join(", "))
    };
    Err(format!("MCP server \"{name}\" has no resources{available}"))
}

/// Which listing a call makes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    /// `resources/list`.
    Resources,
    /// `resources/templates/list`.
    Templates,
}

impl Listing {
    /// The payload key, Pi's `key`.
    fn key(self) -> &'static str {
        match self {
            Self::Resources => "resources",
            Self::Templates => "resourceTemplates",
        }
    }

    fn tool(self) -> &'static str {
        match self {
            Self::Resources => LIST_MCP_RESOURCES_TOOL,
            Self::Templates => LIST_MCP_RESOURCE_TEMPLATES_TOOL,
        }
    }

    async fn page(
        self,
        caller: &Caller,
        cursor: Option<&str>,
    ) -> Result<ResourcesPage, bake_mcp::RequestError> {
        match self {
            Self::Resources => caller.client.list_resources_page(cursor).await,
            Self::Templates => caller.client.list_resource_templates_page(cursor).await,
        }
    }

    async fn all(self, caller: &Caller) -> Result<Vec<Resource>, bake_mcp::RequestError> {
        match self {
            Self::Resources => caller.client.list_resources().await,
            Self::Templates => caller.client.list_resource_templates().await,
        }
    }
}

/// `future`, unless `signal` aborts first.
async fn unless_aborted<T>(
    signal: Option<&AbortSignal>,
    future: impl Future<Output = T>,
) -> Result<T, String> {
    match signal {
        None => Ok(future.await),
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => Err("Operation aborted".to_owned()),
            value = future => Ok(value),
        },
    }
}

/// Pi's `list`: one page of one server, or every page of every server with
/// the errors of those that could not be listed.
async fn list(
    servers: &[Arc<Caller>],
    listing: Listing,
    params: &Value,
    signal: Option<&AbortSignal>,
) -> Result<(Option<String>, Value), String> {
    let name = string_argument(params, "server")?;
    let cursor = string_argument(params, "cursor")?;
    let mut payload = Map::new();
    if let Some(name) = name {
        let caller = find_server(servers, &name)?;
        let page = unless_aborted(signal, listing.page(&caller, cursor.as_deref()))
            .await?
            .map_err(|error| error.to_string())?;
        payload.insert("server".into(), caller.server.clone().into());
        payload.insert(
            listing.key().into(),
            Value::Array(
                page.resources
                    .iter()
                    .filter(|resource| !is_mcp_app_resource(resource.entry()))
                    .map(|resource| listed(&caller.server, resource))
                    .collect(),
            ),
        );
        if let Some(next) = page.next_cursor {
            payload.insert("nextCursor".into(), next.into());
        }
        return Ok((Some(caller.server.clone()), Value::Object(payload)));
    }
    if cursor.is_some() {
        return Err("cursor can only be used when a server is specified".to_owned());
    }
    let mut ordered: Vec<Arc<Caller>> = servers.to_vec();
    ordered.sort_by(|left, right| left.server.cmp(&right.server));
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for caller in &ordered {
        match unless_aborted(signal, listing.all(caller)).await? {
            Ok(resources) => items.extend(
                resources
                    .iter()
                    .filter(|resource| !is_mcp_app_resource(resource.entry()))
                    .map(|resource| listed(&caller.server, resource)),
            ),
            Err(error) => errors.push(json!({
                "server": caller.server.clone(),
                "error": error.to_string(),
            })),
        }
    }
    payload.insert(listing.key().into(), Value::Array(items));
    if !errors.is_empty() {
        payload.insert("errors".into(), Value::Array(errors));
    }
    Ok((None, Value::Object(payload)))
}

/// Pi's `jsonResult`: the payload as one bounded JSON text block, with the
/// payload itself as the structured content scripts receive.
async fn json_result(
    tool: &str,
    server: Option<String>,
    payload: Value,
    work: &WorkGate,
) -> AgentToolResult {
    let encoded = serde_json::to_string(&payload).unwrap_or_else(|error| error.to_string());
    let (content, full_output_path) = limit_content(vec![text(encoded)], work).await;
    let mut details = json!({ "server": server.unwrap_or_default(), "tool": tool });
    if let Some(path) = &full_output_path {
        details["fullOutputPath"] = path.to_string_lossy().into_owned().into();
    }
    AgentToolResult {
        content,
        details: Some(details),
        structured_content: Some(payload),
        ..AgentToolResult::default()
    }
}

/// Pi's `readResource` tool body: the resource's parts as model content,
/// each labeled with its URI when there is more than one.
async fn read(
    servers: &[Arc<Caller>],
    params: &Value,
    signal: Option<&AbortSignal>,
    work: &WorkGate,
) -> Result<AgentToolResult, String> {
    let name = string_argument(params, "server")?.ok_or("server must be provided")?;
    let uri = string_argument(params, "uri")?.ok_or("uri must be provided")?;
    let caller = find_server(servers, &name)?;
    let result: ReadResourceResult = unless_aborted(signal, caller.client.read_resource(&uri))
        .await?
        .map_err(|error| error.to_string())?;
    let labeled = result.contents.len() > 1;
    let mut blocks: Vec<Value> = Vec::new();
    for contents in &result.contents {
        if labeled {
            blocks.push(json!({ "type": "text", "text": format!("{}:", contents.uri()) }));
        }
        blocks.push(json!({ "type": "resource", "resource": contents.entry() }));
    }
    // The server's resources are readable by definition here, but a read's
    // own blocks carry no resource link, so the hint never applies.
    let converted = to_model_content(&caller.server, &blocks, false, work).await;
    let converted: Vec<UserContentBlock> = if converted.is_empty() {
        vec![text(format!("Resource {uri} is empty."))]
    } else {
        converted
    };
    let (content, full_output_path) = limit_content(converted, work).await;
    let contents: Vec<Value> = result
        .contents
        .iter()
        .map(|contents| {
            Value::Object(
                contents
                    .entry()
                    .iter()
                    .filter(|(key, _)| *key != "_meta")
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        })
        .collect();
    let mut details = json!({ "server": caller.server.clone(), "tool": READ_MCP_RESOURCE_TOOL });
    if let Some(path) = &full_output_path {
        details["fullOutputPath"] = path.to_string_lossy().into_owned().into();
    }
    Ok(AgentToolResult {
        content,
        details: Some(details),
        structured_content: Some(json!({
            "server": caller.server.clone(),
            "uri": uri,
            "contents": contents,
        })),
        ..AgentToolResult::default()
    })
}

fn resource_tool(
    name: &'static str,
    description: &'static str,
    parameters: Value,
    output_schema: Value,
    run: impl Fn(
        Vec<Arc<Caller>>,
        Value,
        Option<AbortSignal>,
    ) -> bake_agent::BoxFuture<'static, Result<AgentToolResult, String>>
    + Send
    + Sync
    + 'static,
    servers: ResourceServers,
) -> Arc<AgentTool> {
    let mut tool = AgentTool::new(
        name,
        name,
        description,
        parameters,
        move |_id, params, signal, _update, _ctx| {
            let callers = servers();
            run(callers, params, signal)
        },
    );
    tool.output_schema = Some(output_schema);
    // Each call sends one MCP read method Bake chose; see the module
    // documentation's replay paragraph.
    tool.replay = Some(ToolReplay::Safe);
    tool.read_only = true;
    Arc::new(tool)
}

/// The three resource tools, bound to the servers `servers` reports at call
/// time. `work` owns the blocking writes of a spilled result.
pub(crate) fn resource_tools(servers: &ResourceServers, work: &WorkGate) -> Vec<Arc<AgentTool>> {
    let listing = |listing: Listing, description: &'static str, output_schema: Value| {
        let work = work.clone();
        resource_tool(
            listing.tool(),
            description,
            list_parameters(),
            output_schema,
            move |callers, params, signal| {
                let work = work.clone();
                Box::pin(async move {
                    let (server, payload) =
                        list(&callers, listing, &params, signal.as_ref()).await?;
                    Ok(json_result(listing.tool(), server, payload, &work).await)
                })
            },
            Arc::clone(servers),
        )
    };
    let work_for_read = work.clone();
    vec![
        listing(
            Listing::Resources,
            LIST_RESOURCES_DESCRIPTION,
            list_output_schema(),
        ),
        listing(
            Listing::Templates,
            LIST_TEMPLATES_DESCRIPTION,
            list_templates_output_schema(),
        ),
        resource_tool(
            READ_MCP_RESOURCE_TOOL,
            READ_RESOURCE_DESCRIPTION,
            read_parameters(),
            read_output_schema(),
            move |callers, params, signal| {
                let work = work_for_read.clone();
                Box::pin(async move { read(&callers, &params, signal.as_ref(), &work).await })
            },
            Arc::clone(servers),
        ),
    ]
}

#[cfg(test)]
mod tests;
