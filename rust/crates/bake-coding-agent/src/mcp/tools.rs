//! MCP tools as agent tools, and their results as model content.
//!
//! Adapted from Pi v1.3.0 `src/extensions/mcp/tools.ts`
//! (`createMcpToolDefinition`, `convertMcpResult`, `toModelContent`,
//! `limitMcpContent`, `toParameters`) and `packages/mcp/src/protocol/content.ts`
//! (`toLlmContent`), with Bake's public names ([`super::names`]) and Bake's
//! all-or-nothing tool list (`syncTools` in `packages/mcp/mcp-client`): a
//! listing with a repeated name, a non-object input schema, or a definition
//! over the bounds below is refused whole, so a server's tools are always one
//! coherent generation.
//!
//! Results follow Pi: text and images pass through, embedded text resources
//! become text, resource links and other blocks become short notes, binary
//! resources other than images are saved to a private file the note names,
//! and model-facing text over 20 KB keeps its start and end with the full
//! text saved beside it. `isError` results are error results whose text the
//! model sees. A resource link of a server whose resources
//! [`super::resources`] can read names `read_mcp_resource` and that server,
//! as Pi's `readableResources` does.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use bake_agent::{AgentTool, AgentToolResult, ToolReplay};
use bake_ai::{ImageContent, TextContent, UserContentBlock};
use bake_mcp::{CallToolResult, McpClient, Tool};
use serde_json::{Map, Value, json};

use super::config::{McpExposure, McpServerSettings};
use super::names::public_tool_name;
use crate::owned_work::WorkGate;
use crate::tools::output_files::create_with_extension;
use crate::tools::truncate::{format_size, truncate_middle};

/// Model-facing text beyond this is cut in the middle, Pi's
/// `MCP_OUTPUT_MAX_BYTES`.
pub const MCP_OUTPUT_MAX_BYTES: usize = 20 * 1024;

/// The most tools one server may declare at once. Bake's bound on what one
/// server adds to every request.
pub const MAX_TOOLS_PER_SERVER: usize = 512;

/// The largest declaration (name, description, and input schema as JSON)
/// one tool may have. Bake's bound.
pub const MAX_DECLARATION_BYTES: usize = 64 * 1024;

/// The prefix of the private files results are saved to.
const OUTPUT_PREFIX: &str = "bake-mcp";

/// What a generation's tools call through: the server's client, and the
/// session's gate, which owns the blocking file writes of a result.
pub(crate) struct Caller {
    pub(crate) server: String,
    pub(crate) client: Arc<McpClient>,
    pub(crate) work: WorkGate,
    /// Whether `read_mcp_resource` reaches this server, so its resource
    /// links name it. Pi reads `readableResources()` per call; here a
    /// server's `resources` capability and its exposure are both fixed for
    /// the connection's life, so the answer is too.
    pub(crate) readable_resources: bool,
}

/// A listed tool as the adapter reads it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListedTool<'a> {
    /// The name on the server, which calls send.
    pub(crate) name: &'a str,
    /// `inputSchema`.
    pub(crate) input_schema: &'a Map<String, Value>,
    /// The rest of the definition (`description`, `title`, `annotations`).
    pub(crate) definition: &'a Map<String, Value>,
}

impl<'a> From<&'a Tool> for ListedTool<'a> {
    fn from(tool: &'a Tool) -> Self {
        Self {
            name: tool.name(),
            input_schema: tool.input_schema(),
            definition: tool.definition(),
        }
    }
}

/// One tool the model will see, before it is bound to a client.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedTool {
    /// The name on the server.
    pub(crate) tool: String,
    /// The public name.
    pub(crate) name: String,
    /// The description the model reads.
    pub(crate) description: String,
    /// The input schema the model reads and the agent validates against.
    pub(crate) parameters: Value,
    /// What scripts receive ([`mcp_result_schema`]); never sent to the
    /// model.
    pub(crate) output_schema: Value,
}

/// The tools of one listing the model will see, in the server's order,
/// without those whose exposure is hidden; or why the listing is refused.
pub(crate) fn plan_tools(
    server: &str,
    settings: &McpServerSettings,
    tools: &[ListedTool<'_>],
) -> Result<Vec<PlannedTool>, String> {
    let mut planned: Vec<PlannedTool> = Vec::with_capacity(tools.len());
    let mut seen = std::collections::HashSet::new();
    for tool in tools {
        let name = tool.name;
        if !seen.insert(name) {
            return Err(format!(
                "server listed tool \"{name}\" more than once — invalid tool list"
            ));
        }
        let parameters = to_parameters(name, tool.input_schema)?;
        if settings.tool_exposure(name) == McpExposure::Hidden {
            continue;
        }
        let public = public_tool_name(server, name);
        if let Some(other) = planned.iter().find(|other| other.name == public) {
            return Err(format!(
                "tools \"{}\" and \"{name}\" share the name {public}",
                other.tool
            ));
        }
        let description = describe(server, name, tool.definition);
        let declared = public.len()
            + description.len()
            + serde_json::to_string(&parameters).map_or(usize::MAX, |text| text.len());
        if declared > MAX_DECLARATION_BYTES {
            return Err(format!(
                "tool \"{name}\" has a {declared}-byte definition, over the {MAX_DECLARATION_BYTES}-byte limit"
            ));
        }
        planned.push(PlannedTool {
            tool: name.to_owned(),
            name: public,
            description,
            parameters,
            output_schema: mcp_result_schema(
                tool.definition
                    .get("outputSchema")
                    .and_then(Value::as_object),
            ),
        });
    }
    if planned.len() > MAX_TOOLS_PER_SERVER {
        return Err(format!(
            "server offers {} tools, over the limit of {MAX_TOOLS_PER_SERVER}",
            planned.len()
        ));
    }
    Ok(planned)
}

/// The planned tools bound to `caller`: one generation.
pub(crate) fn bind_tools(caller: &Arc<Caller>, planned: Vec<PlannedTool>) -> Vec<Arc<AgentTool>> {
    planned
        .into_iter()
        .map(|tool| Arc::new(agent_tool(Arc::clone(caller), tool)))
        .collect()
}

/// Pi's description: the trimmed description, else the title, else a
/// sentence naming the tool.
fn describe(server: &str, name: &str, definition: &Map<String, Value>) -> String {
    let title = definition.get("title").and_then(Value::as_str).or_else(|| {
        definition
            .get("annotations")
            .and_then(|annotations| annotations.get("title"))
            .and_then(Value::as_str)
    });
    let description = definition.get("description").and_then(Value::as_str);
    match description.map(str::trim) {
        Some(description) if !description.is_empty() => description.to_owned(),
        _ => match title {
            Some(title) if !title.is_empty() => title.to_owned(),
            _ => format!("MCP tool {name} from server {server}"),
        },
    }
}

/// Pi's `toParameters`: an object schema, with `type` and `properties`
/// filled in when a server omits them. A schema of another type is refused,
/// as MCP requires `"type": "object"`.
fn to_parameters(name: &str, schema: &Map<String, Value>) -> Result<Value, String> {
    let mut parameters = schema.clone();
    match parameters.get("type") {
        None => {
            parameters.insert("type".into(), "object".into());
        }
        Some(Value::String(kind)) if kind == "object" => {}
        Some(other) => {
            return Err(format!(
                "tool \"{name}\" has an input schema of type {other}, not \"object\""
            ));
        }
    }
    if !parameters.contains_key("properties") {
        parameters.insert("properties".into(), Value::Object(Map::new()));
    }
    Ok(Value::Object(parameters))
}

fn agent_tool(caller: Arc<Caller>, planned: PlannedTool) -> AgentTool {
    let PlannedTool {
        tool,
        name,
        description,
        parameters,
        output_schema,
    } = planned;
    let label = format!("{}/{tool}", caller.server);
    let mut agent_tool = AgentTool::new(
        name,
        label,
        description,
        parameters,
        move |_id, params, signal, _update, _ctx| {
            let caller = Arc::clone(&caller);
            let tool = tool.clone();
            Box::pin(async move {
                if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                    return Err("Operation aborted".to_owned());
                }
                // Pi sends `params ?? {}`; the agent validated an object.
                let arguments = match params {
                    Value::Object(arguments) => arguments,
                    _ => Map::new(),
                };
                // Dropping the call withdraws the request or sends
                // `notifications/cancelled`: an abort ends this call only.
                let call = caller.client.call_tool(&tool, Some(arguments));
                let result = match &signal {
                    Some(signal) => tokio::select! {
                        biased;
                        () = signal.cancelled() => return Err("Operation aborted".to_owned()),
                        result = call => result,
                    },
                    None => call.await,
                };
                let result = result.map_err(|error| error.to_string())?;
                Ok(convert_result(
                    &caller.server,
                    &tool,
                    result,
                    caller.readable_resources,
                    &caller.work,
                )
                .await)
            })
        },
    );
    // A server's `readOnlyHint` is its own claim, not a promise Bake can
    // check, so no MCP call is rerun.
    agent_tool.replay = Some(ToolReplay::Never);
    agent_tool.output_schema = Some(output_schema);
    agent_tool
}

/// Pi v1.3.0's `CONTENT_BLOCK_SCHEMA`.
fn content_block_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": { "enum": ["text", "image", "audio", "resource_link", "resource"] },
            "text": { "type": "string" },
            "data": { "type": "string" },
            "mimeType": { "type": "string" },
        },
        "required": ["type"],
        "additionalProperties": true,
    })
}

/// Keywords whose values are data, so a `$ref` key in them is not a
/// reference.
const SCHEMA_DATA_KEYWORDS: [&str; 4] = ["const", "enum", "default", "examples"];
/// Keywords whose values map names to schemas, so their keys are names.
const SCHEMA_MAP_KEYWORDS: [&str; 6] = [
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "$defs",
    "definitions",
];

/// Pi v1.3.0's `relocateSchema`: `schema` as the subschema at `pointer` of
/// another, its local references (`#`, `#/$defs/Item`) rewritten to point
/// below `pointer`.
fn relocate_schema(schema: &Value, pointer: &str) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| relocate_schema(item, pointer))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let relocated = match value {
                        Value::String(reference)
                            if key == "$ref"
                                && (reference == "#" || reference.starts_with("#/")) =>
                        {
                            Value::String(format!("{pointer}{}", &reference[1..]))
                        }
                        _ if SCHEMA_DATA_KEYWORDS.contains(&key.as_str()) => value.clone(),
                        Value::Object(map) if SCHEMA_MAP_KEYWORDS.contains(&key.as_str()) => {
                            Value::Object(
                                map.iter()
                                    .map(|(name, subschema)| {
                                        (name.clone(), relocate_schema(subschema, pointer))
                                    })
                                    .collect(),
                            )
                        }
                        _ => relocate_schema(value, pointer),
                    };
                    (key.clone(), relocated)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Pi v1.3.0's `createMcpResultSchema`: the output schema of every MCP tool,
/// the `CallToolResult` a codemode script receives (its structured content
/// without `_meta`), with the tool's own `outputSchema` as
/// `structuredContent`.
pub(crate) fn mcp_result_schema(structured_content: Option<&Map<String, Value>>) -> Value {
    json!({
        "type": "object",
        "properties": {
            "content": { "type": "array", "items": content_block_schema() },
            // Servers may send structured content without declaring a
            // schema for it, and it may be any JSON value.
            "structuredContent": structured_content.map_or_else(
                || json!({}),
                |schema| relocate_schema(
                    &Value::Object(schema.clone()),
                    "#/properties/structuredContent",
                ),
            ),
            "isError": { "type": "boolean" },
        },
        "required": ["content"],
        // Fields newer protocol versions add.
        "additionalProperties": true,
    })
}

/// Pi's `convertMcpResult`.
pub(crate) async fn convert_result(
    server: &str,
    tool: &str,
    result: CallToolResult,
    readable_resources: bool,
    work: &WorkGate,
) -> AgentToolResult {
    let mut content = if result.content.is_empty() {
        structured_text(result.structured_content.as_ref())
    } else {
        to_model_content(server, &result.content, readable_resources, work).await
    };
    if result.is_error && text_of(&content).is_empty() {
        content.push(text(format!("MCP tool {server}/{tool} returned an error")));
    }
    let (content, full_output_path) = limit_content(content, work).await;
    let mut details = json!({ "server": server, "tool": tool });
    if let Some(path) = &full_output_path {
        details["fullOutputPath"] = path.to_string_lossy().into_owned().into();
    }
    // What programmatic callers get: the result as sent, without `_meta`.
    let mut structured = result.raw;
    structured.shift_remove("_meta");
    structured.insert("content".into(), Value::Array(result.content));
    AgentToolResult {
        content,
        details: Some(details),
        structured_content: Some(Value::Object(structured)),
        is_error: result.is_error,
        ..AgentToolResult::default()
    }
}

pub(crate) fn text(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent::new(text))
}

fn text_of(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `toLlmContent`'s fallback: structured content as indented JSON when the
/// server sent no blocks.
fn structured_text(structured: Option<&Value>) -> Vec<UserContentBlock> {
    structured
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .map(text)
        .into_iter()
        .collect()
}

/// Pi's `toModelContent`: the model-facing content of `blocks`.
pub(crate) async fn to_model_content(
    server: &str,
    blocks: &[Value],
    readable_resources: bool,
    work: &WorkGate,
) -> Vec<UserContentBlock> {
    let mut content = Vec::with_capacity(blocks.len());
    for block in blocks {
        content.extend(block_to_content(server, block, readable_resources, work).await);
    }
    content
}

/// Pi's `blockToContent`, then `toLlmContent`'s `blockToLlmContent`.
async fn block_to_content(
    server: &str,
    block: &Value,
    readable_resources: bool,
    work: &WorkGate,
) -> Vec<UserContentBlock> {
    let field = |name: &str| block.get(name).and_then(Value::as_str);
    let kind = field("type").unwrap_or_default();
    let unsupported = || vec![text(format!("[unsupported MCP content {kind}]"))];
    match kind {
        "text" => field("text").map_or_else(unsupported, |value| vec![text(value)]),
        "image" => match (field("data"), field("mimeType")) {
            (Some(data), Some(mime_type)) => vec![UserContentBlock::Image(ImageContent {
                data: data.to_owned(),
                mime_type: mime_type.to_owned(),
            })],
            _ => unsupported(),
        },
        "audio" => vec![text(format!(
            "[audio {} omitted]",
            field("mimeType").unwrap_or("unknown type")
        ))],
        "resource_link" => match (field("uri"), field("name")) {
            (Some(uri), Some(name)) => vec![text(resource_link(
                block,
                uri,
                name,
                readable_resources.then_some(server),
            ))],
            _ => unsupported(),
        },
        "resource" => match block.get("resource") {
            Some(resource) => embedded_resource(resource, work).await,
            None => unsupported(),
        },
        _ => unsupported(),
    }
}

/// `[Resource <uri> "<title>" (<type>, <size>): <description>]`, with Pi's
/// sentence naming `read_mcp_resource` when `readable` gives the server
/// whose resources that tool reaches.
fn resource_link(block: &Value, uri: &str, name: &str, readable: Option<&str>) -> String {
    let title = block.get("title").and_then(Value::as_str).unwrap_or(name);
    let mut details = Vec::new();
    if let Some(mime_type) = block.get("mimeType").and_then(Value::as_str)
        && !mime_type.is_empty()
    {
        details.push(mime_type.to_owned());
    }
    if let Some(size) = block.get("size").and_then(Value::as_u64) {
        details.push(format_size(usize::try_from(size).unwrap_or(usize::MAX)));
    }
    let details = if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    };
    let description = match block.get("description").and_then(Value::as_str) {
        Some(description) if !description.is_empty() => format!(": {description}"),
        _ => String::new(),
    };
    let read = readable.map_or_else(String::new, |server| {
        format!(
            ". Read it with {} (server \"{server}\")",
            super::resources::READ_MCP_RESOURCE_TOOL
        )
    });
    format!("[Resource {uri} \"{title}\"{details}{description}{read}]")
}

async fn embedded_resource(resource: &Value, work: &WorkGate) -> Vec<UserContentBlock> {
    let field = |name: &str| resource.get(name).and_then(Value::as_str);
    let uri = field("uri").unwrap_or_default();
    let mime_type = field("mimeType");
    if let Some(value) = field("text") {
        return vec![text(value)];
    }
    let Some(blob) = field("blob") else {
        return vec![text("[unsupported MCP content resource]")];
    };
    if mime_type.is_some_and(|mime| mime.starts_with("image/")) {
        return vec![UserContentBlock::Image(ImageContent {
            data: blob.to_owned(),
            mime_type: mime_type.unwrap_or_default().to_owned(),
        })];
    }
    let data = decode_base64(blob);
    if is_text_mime_type(mime_type) {
        return vec![text(String::from_utf8_lossy(&data).into_owned())];
    }
    let kind = format!(
        "{}, {}",
        mime_type.unwrap_or("unknown type"),
        format_size(data.len())
    );
    let note = match save(data, extension_of(uri), work).await {
        Ok(path) => format!(
            "[Binary resource {uri} ({kind}) saved to {}]",
            path.display()
        ),
        Err(reason) => format!("[Binary resource {uri} ({kind}) could not be saved: {reason}]"),
    };
    vec![text(note)]
}

/// Node's lenient base64 decoder, as `tools::image` reads tool images.
pub(crate) fn decode_base64(data: &str) -> Vec<u8> {
    use base64::Engine;
    let cleaned: String = data
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
    .decode(cleaned)
    .unwrap_or_default()
}

/// Pi's `isTextMimeType`.
fn is_text_mime_type(mime_type: Option<&str>) -> bool {
    let Some(mime_type) = mime_type else {
        return false;
    };
    let kind = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    kind.starts_with("text/")
        || kind == "application/json"
        || kind.ends_with("+json")
        || kind.ends_with("+xml")
}

/// Pi's `extensionOf`: the URI path's extension of up to eight letters or
/// digits, else `.bin`.
fn extension_of(uri: &str) -> String {
    let parsed = reqwest::Url::parse(uri).ok();
    let path = parsed.as_ref().map_or(uri, |url| url.path());
    let Some((_, extension)) = path.rsplit_once('.') else {
        return ".bin".into();
    };
    if (1..=8).contains(&extension.len()) && extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        format!(".{extension}")
    } else {
        ".bin".into()
    }
}

/// Writes `data` to a new private file on the session's blocking pool.
async fn save(data: Vec<u8>, extension: String, work: &WorkGate) -> Result<PathBuf, String> {
    let task = work
        .spawn_blocking(move || {
            let (path, mut file) =
                create_with_extension(&std::env::temp_dir(), OUTPUT_PREFIX, &extension)?;
            file.write_all(&data)?;
            file.sync_all()?;
            Ok::<PathBuf, std::io::Error>(path)
        })
        .map_err(|refused| refused.to_string())?;
    match task.await {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Pi's `limitMcpContent`: text within [`MCP_OUTPUT_MAX_BYTES`] as it is;
/// longer text becomes one block in Codex's truncation format, naming the
/// file that holds all of it, followed by the images.
pub(crate) async fn limit_content(
    content: Vec<UserContentBlock>,
    work: &WorkGate,
) -> (Vec<UserContentBlock>, Option<PathBuf>) {
    let combined = text_of(&content);
    let truncation = truncate_middle(&combined, MCP_OUTPUT_MAX_BYTES);
    if !truncation.truncated {
        return (content, None);
    }
    let (full_output_path, place) = match save(combined.into_bytes(), ".txt".into(), work).await {
        Ok(path) => {
            let place = format!(
                "[Full output: {} (read it with offset/limit)]",
                path.display()
            );
            (Some(path), place)
        }
        Err(reason) => (None, format!("[Could not save the full output: {reason}]")),
    };
    let tokens = truncation.total_bytes.div_ceil(4);
    let mut limited = vec![text(format!(
        "Warning: truncated output (original token count: {tokens})\nTotal output lines: {}\n\n{}\n\n{place}",
        truncation.total_lines, truncation.content
    ))];
    limited.extend(
        content
            .into_iter()
            .filter(|block| matches!(block, UserContentBlock::Image(_))),
    );
    (limited, full_output_path)
}

#[cfg(test)]
mod tests;
