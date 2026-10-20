//! The MCP shapes this client reads and writes: protocol versions, the
//! initialize result, tools, tool results, and resources.
//!
//! Ported from Pi's `packages/mcp/src/protocol/types.ts` and the result checks
//! of `src/client.ts` (v1.3.0). Each type keeps the server's whole JSON object
//! next to the fields this client checks, so later layers (tool adapters,
//! content conversion) read what the server sent rather than a lossy copy.

use std::fmt;

use serde_json::{Map, Value};

use crate::jsonrpc::preview;

/// A protocol revision this client negotiates with `initialize`.
///
/// These are the handshake-based ("legacy") revisions Pi v1.3.0 accepts.
/// The stateless 2026-07-28 revision, which replaces `initialize` with
/// per-request metadata and `server/discover`, is not implemented; a server
/// that speaks only that revision rejects `initialize`, and the client reports
/// the server's error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolVersion {
    /// `2025-11-25`, the latest handshake-based revision.
    V2025_11_25,
    /// `2025-06-18`.
    V2025_06_18,
    /// `2025-03-26`.
    V2025_03_26,
    /// `2024-11-05`.
    V2024_11_05,
}

impl ProtocolVersion {
    /// The version the client requests by default.
    pub const LATEST: Self = Self::V2025_11_25;

    /// Every version the client accepts from a server, newest first. Servers
    /// that do not support the requested version answer with their own, so
    /// older ones stay accepted (Pi's `SUPPORTED_PROTOCOL_VERSIONS`).
    pub const SUPPORTED: [Self; 4] = [
        Self::V2025_11_25,
        Self::V2025_06_18,
        Self::V2025_03_26,
        Self::V2024_11_05,
    ];

    /// The version's wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V2025_11_25 => "2025-11-25",
            Self::V2025_06_18 => "2025-06-18",
            Self::V2025_03_26 => "2025-03-26",
            Self::V2024_11_05 => "2024-11-05",
        }
    }

    /// The supported version with this wire string.
    pub fn parse(text: &str) -> Option<Self> {
        Self::SUPPORTED
            .into_iter()
            .find(|version| version.as_str() == text)
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A client or server name and version (`Implementation` in the schema).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Implementation {
    /// The program name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// A display name, if any.
    pub title: Option<String>,
}

impl Implementation {
    /// A name and version without a title.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            title: None,
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("name".into(), self.name.clone().into());
        object.insert("version".into(), self.version.clone().into());
        if let Some(title) = &self.title {
            object.insert("title".into(), title.clone().into());
        }
        Value::Object(object)
    }
}

/// The capabilities a server declared in its initialize result.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerCapabilities {
    raw: Map<String, Value>,
}

impl ServerCapabilities {
    /// The server's `tools` capability, if it declared one. Pi's coding agent
    /// lists tools only when it is present: servers without it (prompts or
    /// resources only) need not answer `tools/list`.
    pub fn tools(&self) -> Option<ToolsCapability> {
        let tools = self.raw.get("tools")?.as_object()?;
        Some(ToolsCapability {
            list_changed: tools.get("listChanged").and_then(Value::as_bool) == Some(true),
        })
    }

    /// The server's `resources` capability, if it declared one. Pi's coding
    /// agent offers its resource tools only for servers that declared it
    /// (`McpServerConnection.hasResources`); a server without it need not
    /// answer `resources/list`.
    pub fn resources(&self) -> Option<ResourcesCapability> {
        let resources = self.raw.get("resources")?.as_object()?;
        Some(ResourcesCapability {
            list_changed: resources.get("listChanged").and_then(Value::as_bool) == Some(true),
            subscribe: resources.get("subscribe").and_then(Value::as_bool) == Some(true),
        })
    }

    /// The capabilities object as the server sent it.
    pub fn raw(&self) -> &Map<String, Value> {
        &self.raw
    }
}

/// The server's `tools` capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolsCapability {
    /// Whether the server said it sends `notifications/tools/list_changed`.
    pub list_changed: bool,
}

/// The server's `resources` capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourcesCapability {
    /// Whether the server said it sends
    /// `notifications/resources/list_changed`.
    pub list_changed: bool,
    /// Whether the server said it accepts `resources/subscribe`, which this
    /// client does not send.
    pub subscribe: bool,
}

/// The server's answer to `initialize`, checked as Pi checks it.
#[derive(Clone, Debug, PartialEq)]
pub struct InitializeResult {
    /// The version the server selected; always one of
    /// [`ProtocolVersion::SUPPORTED`].
    pub protocol_version: ProtocolVersion,
    /// The server's capabilities.
    pub capabilities: ServerCapabilities,
    /// The server's name and version.
    pub server_info: Implementation,
    /// The server's instructions for the model, if any. Their size is bounded
    /// only by the message limit; a consumer that shows them to the model
    /// applies its own bound.
    pub instructions: Option<String>,
}

/// Why an initialize result was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InitializeRejection {
    Invalid(&'static str),
    UnsupportedVersion(String),
}

pub(crate) fn initialize_result(value: Value) -> Result<InitializeResult, InitializeRejection> {
    let Value::Object(mut object) = value else {
        return Err(InitializeRejection::Invalid("the result is not an object"));
    };
    let Some(Value::String(version)) = object.remove("protocolVersion") else {
        return Err(InitializeRejection::Invalid(
            "\"protocolVersion\" is not a string",
        ));
    };
    let Some(Value::Object(capabilities)) = object.remove("capabilities") else {
        return Err(InitializeRejection::Invalid(
            "\"capabilities\" is not an object",
        ));
    };
    let Some(Value::Object(mut info)) = object.remove("serverInfo") else {
        return Err(InitializeRejection::Invalid(
            "\"serverInfo\" is not an object",
        ));
    };
    let (Some(Value::String(name)), Some(Value::String(server_version))) =
        (info.remove("name"), info.remove("version"))
    else {
        return Err(InitializeRejection::Invalid(
            "\"serverInfo\" lacks a string \"name\" and \"version\"",
        ));
    };
    let title = match info.remove("title") {
        Some(Value::String(title)) => Some(title),
        _ => None,
    };
    let instructions = match object.remove("instructions") {
        None => None,
        Some(Value::String(text)) => Some(text),
        Some(_) => {
            return Err(InitializeRejection::Invalid(
                "\"instructions\" is not a string",
            ));
        }
    };
    // The version is checked after the shape, as Pi does.
    let Some(protocol_version) = ProtocolVersion::parse(&version) else {
        return Err(InitializeRejection::UnsupportedVersion(preview(
            &version, 64,
        )));
    };
    Ok(InitializeResult {
        protocol_version,
        capabilities: ServerCapabilities { raw: capabilities },
        server_info: Implementation {
            name,
            version: server_version,
            title,
        },
        instructions,
    })
}

/// A tool from `tools/list`.
///
/// Only `name` (a string) and `inputSchema` (an object) are checked, as in
/// Pi; the schema is not validated as JSON Schema here. Names are not
/// deduplicated or sanitized: the tool adapter owns the model-visible names.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    name: String,
    input_schema: Map<String, Value>,
    definition: Map<String, Value>,
}

impl Tool {
    /// The tool's name on the server.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tool's `inputSchema` object.
    pub fn input_schema(&self) -> &Map<String, Value> {
        &self.input_schema
    }

    /// The tool's `description`, when it is a string.
    pub fn description(&self) -> Option<&str> {
        self.definition.get("description").and_then(Value::as_str)
    }

    /// The rest of the definition as the server sent it: every field except
    /// `inputSchema`, including `name` and the fields this client does not
    /// read (`title`, `outputSchema`, `annotations`, `execution`, `_meta`).
    pub fn definition(&self) -> &Map<String, Value> {
        &self.definition
    }
}

/// One page of `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolsPage {
    /// The page's tools in the server's order.
    pub tools: Vec<Tool>,
    /// The cursor of the next page. A `null` or empty cursor ends the list,
    /// as some servers write it (Pi 1.0.0's fix).
    pub next_cursor: Option<String>,
}

pub(crate) fn tools_page(value: Value) -> Result<ToolsPage, String> {
    let Value::Object(mut object) = value else {
        return Err("the result is not an object".into());
    };
    let Some(Value::Array(items)) = object.remove("tools") else {
        return Err("\"tools\" is not an array".into());
    };
    let mut tools = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let Value::Object(mut definition) = item else {
            return Err(format!("tool {index} is not an object"));
        };
        let Some(Value::String(name)) = definition.get("name") else {
            return Err(format!("tool {index} has no string \"name\""));
        };
        let name = name.clone();
        let Some(Value::Object(input_schema)) = definition.shift_remove("inputSchema") else {
            return Err(format!(
                "tool {index} ({:?}) has no object \"inputSchema\"",
                preview(&name, 64)
            ));
        };
        tools.push(Tool {
            name,
            input_schema,
            definition,
        });
    }
    let next_cursor = match object.remove("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if cursor.is_empty() => None,
        Some(Value::String(cursor)) => Some(cursor),
        Some(_) => return Err("\"nextCursor\" is not a string".into()),
    };
    Ok(ToolsPage { tools, next_cursor })
}

/// A resource from `resources/list`, or a template from
/// `resources/templates/list`.
///
/// Only the addressing field (`uri` or `uriTemplate`) and, when present,
/// `name` are checked, as in Pi's `isResource` and `isResourceTemplate`; the
/// rest of the entry is kept as the server sent it. Pi's `toResource` and
/// `toResourceTemplate` fill a missing `name` with the address, which some
/// servers omit although the schema requires it.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    entry: Map<String, Value>,
}

impl Resource {
    /// `uri` for a resource, `uriTemplate` for a template.
    pub fn address(&self) -> &str {
        self.entry
            .get("uri")
            .or_else(|| self.entry.get("uriTemplate"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// `name`, which Pi fills with [`Self::address`] when the server omits it.
    pub fn name(&self) -> &str {
        self.entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The entry as the server sent it, with `name` filled in.
    pub fn entry(&self) -> &Map<String, Value> {
        &self.entry
    }
}

/// One page of `resources/list` or `resources/templates/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourcesPage {
    /// The page's entries in the server's order.
    pub resources: Vec<Resource>,
    /// The cursor of the next page. A `null` or empty cursor ends the list,
    /// as for [`ToolsPage`].
    pub next_cursor: Option<String>,
}

/// Which list a page came from: the result key and the address each entry
/// must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    /// `resources/list`: `resources` entries with a `uri`.
    Resource,
    /// `resources/templates/list`: `resourceTemplates` entries with a
    /// `uriTemplate`.
    Template,
}

impl ResourceKind {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Resource => "resources",
            Self::Template => "resourceTemplates",
        }
    }

    fn address(self) -> &'static str {
        match self {
            Self::Resource => "uri",
            Self::Template => "uriTemplate",
        }
    }
}

pub(crate) fn resources_page(kind: ResourceKind, value: Value) -> Result<ResourcesPage, String> {
    let key = kind.key();
    let address = kind.address();
    let Value::Object(mut object) = value else {
        return Err("the result is not an object".into());
    };
    let Some(Value::Array(items)) = object.remove(key) else {
        return Err(format!("\"{key}\" is not an array"));
    };
    let mut resources = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let Value::Object(mut entry) = item else {
            return Err(format!("entry {index} is not an object"));
        };
        let Some(Value::String(found)) = entry.get(address) else {
            return Err(format!("entry {index} has no string \"{address}\""));
        };
        let found = found.clone();
        match entry.get("name") {
            None | Some(Value::Null) => {
                entry.insert("name".into(), found.into());
            }
            Some(Value::String(_)) => {}
            Some(_) => return Err(format!("entry {index} has a non-string \"name\"")),
        }
        resources.push(Resource { entry });
    }
    let next_cursor = match object.remove("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if cursor.is_empty() => None,
        Some(Value::String(cursor)) => Some(cursor),
        Some(_) => return Err("\"nextCursor\" is not a string".into()),
    };
    Ok(ResourcesPage {
        resources,
        next_cursor,
    })
}

/// One `contents` entry of `resources/read`: text or a base64 blob, with
/// the rest of the entry as the server sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceContents {
    entry: Map<String, Value>,
}

impl ResourceContents {
    /// `uri`, which the result checks require.
    pub fn uri(&self) -> &str {
        self.entry
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The entry as the server sent it.
    pub fn entry(&self) -> &Map<String, Value> {
        &self.entry
    }
}

/// The result of `resources/read`, checked as Pi's
/// `validateReadResourceResult` checks it: every entry has a `uri` and
/// either a string `text` or a string `blob`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadResourceResult {
    /// The resource's parts, in the server's order.
    pub contents: Vec<ResourceContents>,
}

pub(crate) fn read_resource_result(value: Value) -> Result<ReadResourceResult, String> {
    let Value::Object(mut object) = value else {
        return Err("the result is not an object".into());
    };
    let Some(Value::Array(items)) = object.remove("contents") else {
        return Err("\"contents\" is not an array".into());
    };
    let mut contents = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let Value::Object(entry) = item else {
            return Err(format!("contents {index} is not an object"));
        };
        if !matches!(entry.get("uri"), Some(Value::String(_))) {
            return Err(format!("contents {index} has no string \"uri\""));
        }
        let text = matches!(entry.get("text"), Some(Value::String(_)));
        let blob = matches!(entry.get("blob"), Some(Value::String(_)));
        if !text && !blob {
            return Err(format!(
                "contents {index} has neither a string \"text\" nor a string \"blob\""
            ));
        }
        contents.push(ResourceContents { entry });
    }
    Ok(ReadResourceResult { contents })
}

/// The result of `tools/call`.
///
/// A tool that failed reports `is_error` with a successful JSON-RPC response;
/// a JSON-RPC error response is a protocol error instead. Content items are
/// kept as JSON: converting them for the model belongs to the tool adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct CallToolResult {
    /// The `content` items; empty when the server omitted `content`, as Pi
    /// and the official SDK default it.
    pub content: Vec<Value>,
    /// `structuredContent` when present. MCP 2026-07-28 allows any JSON value
    /// here, and an explicit `null` stays `Some(Value::Null)` (Pi #10416).
    pub structured_content: Option<Value>,
    /// `isError`; absent means `false`.
    pub is_error: bool,
    /// The whole result as the server sent it, without `content`.
    pub raw: Map<String, Value>,
}

pub(crate) fn call_tool_result(value: Value) -> Result<CallToolResult, String> {
    let Value::Object(mut object) = value else {
        return Err("the result is not an object".into());
    };
    let content = match object.shift_remove("content") {
        None => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("\"content\" is not an array".into()),
    };
    // Stricter than Pi, which passes `isError` through untyped: reading a
    // non-boolean as success could show a failed call to the model as one
    // that succeeded.
    let is_error = match object.get("isError") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err("\"isError\" is not a boolean".into()),
    };
    Ok(CallToolResult {
        content,
        structured_content: object.get("structuredContent").cloned(),
        is_error,
        raw: object,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn versions_round_trip_and_unknown_ones_are_refused() {
        for version in ProtocolVersion::SUPPORTED {
            assert_eq!(ProtocolVersion::parse(version.as_str()), Some(version));
        }
        assert_eq!(ProtocolVersion::parse("2026-07-28"), None);
        assert_eq!(ProtocolVersion::parse("1999-01-01"), None);
        assert_eq!(ProtocolVersion::LATEST.as_str(), "2025-11-25");
    }

    #[test]
    fn initialize_results_are_checked_like_pi() {
        let good = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "s", "version": "1", "title": "S"},
            "instructions": "Use it.",
        });
        let result = initialize_result(good).unwrap();
        assert_eq!(result.protocol_version, ProtocolVersion::V2025_06_18);
        assert_eq!(
            result.capabilities.tools(),
            Some(ToolsCapability { list_changed: true })
        );
        assert_eq!(result.server_info.title.as_deref(), Some("S"));
        assert_eq!(result.instructions.as_deref(), Some("Use it."));

        let bad = [
            json!([]),
            json!({"capabilities": {}, "serverInfo": {"name": "s", "version": "1"}}),
            json!({"protocolVersion": "2025-06-18", "capabilities": [], "serverInfo": {"name": "s", "version": "1"}}),
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "s"}}),
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "s", "version": "1"}, "instructions": 3}),
        ];
        for value in bad {
            assert!(
                matches!(
                    initialize_result(value.clone()),
                    Err(InitializeRejection::Invalid(_))
                ),
                "{value}"
            );
        }
        assert_eq!(
            initialize_result(json!({
                "protocolVersion": "1999-01-01",
                "capabilities": {},
                "serverInfo": {"name": "s", "version": "1"},
            })),
            Err(InitializeRejection::UnsupportedVersion("1999-01-01".into()))
        );
    }

    #[test]
    fn a_tools_capability_must_be_an_object() {
        let caps = |value: Value| ServerCapabilities {
            raw: value.as_object().cloned().unwrap(),
        };
        assert_eq!(caps(json!({})).tools(), None);
        assert_eq!(caps(json!({"tools": true})).tools(), None);
        assert_eq!(
            caps(json!({"tools": {}})).tools(),
            Some(ToolsCapability {
                list_changed: false
            })
        );
    }

    #[test]
    fn tool_pages_check_names_schemas_and_cursors() {
        let page = tools_page(json!({
            "tools": [{"name": "read", "inputSchema": {"type": "object"}, "description": "Read", "annotations": {"readOnlyHint": true}}],
            "nextCursor": "",
        }))
        .unwrap();
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.tools[0].name(), "read");
        assert_eq!(page.tools[0].description(), Some("Read"));
        assert_eq!(
            page.tools[0].input_schema(),
            json!({"type": "object"}).as_object().unwrap()
        );
        assert_eq!(
            page.tools[0].definition()["annotations"],
            json!({"readOnlyHint": true})
        );
        assert_eq!(
            tools_page(json!({"tools": [], "nextCursor": null}))
                .unwrap()
                .next_cursor,
            None
        );
        for (value, expected) in [
            (json!({}), "\"tools\" is not an array"),
            (json!({"tools": [1]}), "tool 0 is not an object"),
            (
                json!({"tools": [{"inputSchema": {}}]}),
                "tool 0 has no string",
            ),
            (
                json!({"tools": [{"name": "x", "inputSchema": "object"}]}),
                "no object \"inputSchema\"",
            ),
            (json!({"tools": [], "nextCursor": 2}), "\"nextCursor\""),
        ] {
            let error = tools_page(value).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn a_resources_capability_must_be_an_object() {
        let caps = |value: Value| ServerCapabilities {
            raw: value.as_object().cloned().unwrap(),
        };
        assert_eq!(caps(json!({})).resources(), None);
        assert_eq!(caps(json!({"resources": true})).resources(), None);
        assert_eq!(
            caps(json!({"resources": {}})).resources(),
            Some(ResourcesCapability {
                list_changed: false,
                subscribe: false
            })
        );
        assert_eq!(
            caps(json!({"resources": {"listChanged": true, "subscribe": true}})).resources(),
            Some(ResourcesCapability {
                list_changed: true,
                subscribe: true
            })
        );
        // A server with resources only need not declare tools.
        assert_eq!(caps(json!({"resources": {}})).tools(), None);
    }

    #[test]
    fn resource_pages_check_addresses_names_and_cursors() {
        let page = resources_page(
            ResourceKind::Resource,
            json!({
                "resources": [
                    {"uri": "docs://intro", "name": "intro", "_meta": {"x": 1}},
                    {"uri": "docs://plain"},
                ],
                "nextCursor": null,
            }),
        )
        .unwrap();
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.resources[0].name(), "intro");
        // Pi's `toResource`: the address stands in for a missing name.
        assert_eq!(page.resources[1].name(), "docs://plain");
        assert_eq!(page.resources[0].entry()["_meta"], json!({"x": 1}));

        let templates = resources_page(
            ResourceKind::Template,
            json!({"resourceTemplates": [{"uriTemplate": "docs://p/{s}"}], "nextCursor": "next"}),
        )
        .unwrap();
        assert_eq!(templates.next_cursor.as_deref(), Some("next"));
        assert_eq!(templates.resources[0].address(), "docs://p/{s}");

        for (kind, value, expected) in [
            (
                ResourceKind::Resource,
                json!({}),
                "\"resources\" is not an array",
            ),
            (
                ResourceKind::Template,
                json!({"resources": []}),
                "\"resourceTemplates\" is not an array",
            ),
            (
                ResourceKind::Resource,
                json!({"resources": [1]}),
                "entry 0 is not an object",
            ),
            (
                ResourceKind::Resource,
                json!({"resources": [{"name": "x"}]}),
                "entry 0 has no string \"uri\"",
            ),
            (
                ResourceKind::Template,
                json!({"resourceTemplates": [{"uri": "x"}]}),
                "entry 0 has no string \"uriTemplate\"",
            ),
            (
                ResourceKind::Resource,
                json!({"resources": [{"uri": "x", "name": 1}]}),
                "entry 0 has a non-string \"name\"",
            ),
            (
                ResourceKind::Resource,
                json!({"resources": [], "nextCursor": 2}),
                "\"nextCursor\" is not a string",
            ),
            (ResourceKind::Resource, json!([]), "not an object"),
        ] {
            let error = resources_page(kind, value.clone()).unwrap_err();
            assert!(error.contains(expected), "{value}: {error}");
        }
    }

    #[test]
    fn read_results_need_a_uri_and_a_body() {
        let read = read_resource_result(json!({"contents": [
            {"uri": "docs://a", "text": "x"},
            {"uri": "docs://b", "blob": "AAAA", "_meta": {"y": 1}},
        ]}))
        .unwrap();
        assert_eq!(read.contents[0].uri(), "docs://a");
        assert_eq!(read.contents[1].entry()["_meta"], json!({"y": 1}));
        assert_eq!(
            read_resource_result(json!({"contents": []}))
                .unwrap()
                .contents,
            Vec::new()
        );
        for (value, expected) in [
            (json!({}), "\"contents\" is not an array"),
            (json!({"contents": [1]}), "contents 0 is not an object"),
            (
                json!({"contents": [{"text": "x"}]}),
                "contents 0 has no string \"uri\"",
            ),
            (
                json!({"contents": [{"uri": "x"}]}),
                "neither a string \"text\" nor a string \"blob\"",
            ),
            (
                json!({"contents": [{"uri": "x", "text": 1}]}),
                "neither a string \"text\" nor a string \"blob\"",
            ),
            (json!(null), "the result is not an object"),
        ] {
            let error = read_resource_result(value.clone()).unwrap_err();
            assert!(error.contains(expected), "{value}: {error}");
        }
    }

    #[test]
    fn call_results_default_content_and_check_is_error() {
        let result = call_tool_result(json!({"structuredContent": {"ok": true}})).unwrap();
        assert!(result.content.is_empty());
        assert_eq!(result.structured_content, Some(json!({"ok": true})));
        assert!(!result.is_error);
        let result =
            call_tool_result(json!({"content": [], "structuredContent": null, "isError": true}))
                .unwrap();
        assert_eq!(result.structured_content, Some(Value::Null));
        assert!(result.is_error);
        assert_eq!(
            call_tool_result(json!({"content": "x"})).unwrap_err(),
            "\"content\" is not an array"
        );
        assert_eq!(
            call_tool_result(json!({"isError": "true"})).unwrap_err(),
            "\"isError\" is not a boolean"
        );
        assert_eq!(
            call_tool_result(json!(null)).unwrap_err(),
            "the result is not an object"
        );
    }
}
