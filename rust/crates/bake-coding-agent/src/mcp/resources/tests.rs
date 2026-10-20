//! Pi v1.3.0 `test/suite/agent-session-mcp.test.ts`, "lists and reads
//! resources with Codex's resource tools", over a fake MCP server this
//! module drives in process, with Bake's own bound, blob, and argument
//! cases.

use std::sync::{Arc, Mutex, PoisonError};

use bake_mcp::{ClientOptions, Implementation, LineReader, LineWriter, McpClient};
use serde_json::json;

use super::*;
use crate::mcp::tools::MCP_OUTPUT_MAX_BYTES;

/// A one-pixel PNG, as Pi's `TINY_PNG_BASE64`.
const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// What one fake server was asked.
struct Fake {
    /// The methods the server received, in order.
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

/// A caller over a fake server that answers from `respond`, each direction
/// its own pipe. The returned task is the server; aborting it ends the
/// connection.
async fn client(
    respond: impl Fn(&str, &Value) -> Value + Send + 'static,
) -> (Arc<Caller>, Fake, tokio::task::JoinHandle<()>) {
    let (to_server, server_reads) = tokio::io::duplex(1 << 20);
    let (server_writes, from_server) = tokio::io::duplex(1 << 20);
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let server_seen = Arc::clone(&seen);
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut lines = BufReader::new(server_reads).lines();
        let mut write = server_writes;
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            server_seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((method.to_owned(), params.clone()));
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            let answer = match method {
                "initialize" => json!({
                    "protocolVersion": params["protocolVersion"],
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "docs", "version": "1" },
                }),
                other => respond(other, &params),
            };
            let reply = match answer.get("error") {
                Some(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
                None => json!({ "jsonrpc": "2.0", "id": id, "result": answer }),
            };
            if write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
    });
    let options = ClientOptions::new(Implementation::new("bake", "test"));
    let limits = options.limits;
    let (connected, _events) = McpClient::connect(
        LineReader::new(from_server, limits.max_message_bytes),
        LineWriter::new(to_server),
        options,
    )
    .await
    .expect("a client over the fake server");
    (
        Arc::new(Caller {
            server: "docs".into(),
            client: Arc::new(connected),
            work: WorkGate::default(),
            readable_resources: true,
        }),
        Fake { seen },
        server,
    )
}

impl Fake {
    fn methods(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }
}

fn servers(callers: Vec<Arc<Caller>>) -> ResourceServers {
    Arc::new(move || callers.clone())
}

fn texts(result: &AgentToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(image) => format!("image {} {}", image.mime_type, image.data),
        })
        .collect()
}

async fn run(tool: &AgentTool, params: Value) -> Result<AgentToolResult, String> {
    (tool.execute)(
        "call-1".into(),
        params,
        None,
        bake_agent::AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await
}

/// Pi's fake server for the resource cases.
fn docs_server(method: &str, params: &Value) -> Value {
    match method {
        "resources/list" => json!({
            "resources": [
                { "uri": "docs://intro", "name": "intro", "mimeType": "text/markdown", "_meta": { "x": 1 } },
                // MCP App user interfaces are left out.
                { "uri": "ui://docs/viewer", "name": "viewer", "mimeType": "text/html" },
                { "uri": "docs://app", "name": "app", "mimeType": "text/html;profile=mcp-app" },
            ],
        }),
        "resources/templates/list" => json!({
            "resourceTemplates": [
                { "uriTemplate": "docs://pages/{slug}", "name": "page", "icons": [{ "src": "data:image/png;base64,AAAA" }] },
            ],
        }),
        "resources/read" => {
            let uri = params["uri"].as_str().unwrap_or_default();
            match uri {
                "docs://logo" => json!({
                    "contents": [{ "uri": uri, "mimeType": "image/png", "blob": TINY_PNG }],
                }),
                "docs://huge" => json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "text/plain",
                        "text": "h".repeat(MCP_OUTPUT_MAX_BYTES + 1_000),
                    }],
                }),
                "docs://empty" => json!({ "contents": [] }),
                "docs://pair" => json!({
                    "contents": [
                        { "uri": "docs://pair/a", "text": "first" },
                        { "uri": "docs://pair/b", "text": "second" },
                    ],
                }),
                _ => json!({
                    "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": format!("# {uri}"), "_meta": { "y": 2 } }],
                }),
            }
        }
        _ => json!({}),
    }
}

// Pi "lists and reads resources with Codex's resource tools": the tool
// names, the JSON payloads, the `_meta` and icon fields left out, and the
// message for a server without resources.
#[tokio::test]
async fn lists_and_reads_resources_with_codex_tools() {
    let (caller, fake, server) = client(docs_server).await;
    let tools = resource_tools(&servers(vec![Arc::clone(&caller)]), &WorkGate::default());
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        RESOURCE_TOOL_NAMES
    );
    for tool in &tools {
        assert_eq!(tool.replay, Some(ToolReplay::Safe));
        assert!(tool.read_only);
    }

    let listed = run(&tools[0], json!({})).await.unwrap();
    assert_eq!(
        listed.structured_content,
        Some(json!({
            "resources": [
                { "server": "docs", "uri": "docs://intro", "name": "intro", "mimeType": "text/markdown" },
            ],
        }))
    );
    // The model reads the payload as compact JSON, as Codex sends it.
    assert_eq!(
        texts(&listed),
        [serde_json::to_string(&listed.structured_content).unwrap()]
    );
    assert_eq!(
        listed.details,
        Some(json!({ "server": "", "tool": LIST_MCP_RESOURCES_TOOL }))
    );

    let templates = run(&tools[1], json!({ "server": "docs" })).await.unwrap();
    assert_eq!(
        templates.structured_content,
        Some(json!({
            "server": "docs",
            "resourceTemplates": [
                { "server": "docs", "uriTemplate": "docs://pages/{slug}", "name": "page" },
            ],
        }))
    );

    let read = run(
        &tools[2],
        json!({ "server": "docs", "uri": "docs://pages/setup" }),
    )
    .await
    .unwrap();
    assert_eq!(texts(&read), ["# docs://pages/setup"]);
    assert_eq!(
        read.structured_content,
        Some(json!({
            "server": "docs",
            "uri": "docs://pages/setup",
            "contents": [
                { "uri": "docs://pages/setup", "mimeType": "text/markdown", "text": "# docs://pages/setup" },
            ],
        }))
    );

    // Pi: a server without resources is named, with the servers that have
    // them.
    let missing = run(&tools[2], json!({ "server": "nope", "uri": "docs://x" })).await;
    assert_eq!(
        missing.unwrap_err(),
        "MCP server \"nope\" has no resources. Servers with resources: docs"
    );
    assert_eq!(
        fake.methods(),
        [
            "initialize",
            "notifications/initialized",
            "resources/list",
            "resources/templates/list",
            "resources/read",
        ]
    );
    caller.client.shutdown().await;
    server.abort();
}

// Bake: an oversized read keeps its start and end and names the file with
// all of it, through the same spill the MCP tool results use.
#[tokio::test]
async fn an_oversized_read_is_bounded_and_spilled() {
    let (caller, _fake, server) = client(docs_server).await;
    let tools = resource_tools(&servers(vec![Arc::clone(&caller)]), &WorkGate::default());
    let read = run(&tools[2], json!({ "server": "docs", "uri": "docs://huge" }))
        .await
        .unwrap();
    let text = texts(&read).join("\n");
    assert!(text.len() < MCP_OUTPUT_MAX_BYTES + 1_024, "{}", text.len());
    assert!(text.starts_with("Warning: truncated output"), "{text}");
    let path = read.details.as_ref().unwrap()["fullOutputPath"]
        .as_str()
        .expect("the full output was saved")
        .to_owned();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().len(),
        MCP_OUTPUT_MAX_BYTES + 1_000
    );
    // The structured content scripts receive is never truncated.
    assert_eq!(
        read.structured_content.as_ref().unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .len(),
        MCP_OUTPUT_MAX_BYTES + 1_000
    );
    std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    caller.client.shutdown().await;
    server.abort();
}

// Pi's `blockToContent` through a read: an image blob becomes an image
// block, several parts are labeled with their URIs, and an empty resource
// says so.
#[tokio::test]
async fn an_image_blob_is_projected_and_parts_are_labeled() {
    let (caller, _fake, server) = client(docs_server).await;
    let tools = resource_tools(&servers(vec![Arc::clone(&caller)]), &WorkGate::default());
    let image = run(&tools[2], json!({ "server": "docs", "uri": "docs://logo" }))
        .await
        .unwrap();
    assert_eq!(texts(&image), [format!("image image/png {TINY_PNG}")]);

    let pair = run(&tools[2], json!({ "server": "docs", "uri": "docs://pair" }))
        .await
        .unwrap();
    assert_eq!(
        texts(&pair),
        ["docs://pair/a:", "first", "docs://pair/b:", "second"]
    );

    let empty = run(
        &tools[2],
        json!({ "server": "docs", "uri": "docs://empty" }),
    )
    .await
    .unwrap();
    assert_eq!(texts(&empty), ["Resource docs://empty is empty."]);
    caller.client.shutdown().await;
    server.abort();
}

// Pi's `stringArgument` and the cursor rule, and a listing error of one
// server in an every-server listing.
#[tokio::test]
async fn arguments_and_listing_errors_follow_pi() {
    let (caller, _fake, server) = client(|method, _| match method {
        "resources/list" => json!({ "error": { "code": -32603, "message": "list failed" } }),
        "resources/templates/list" => {
            json!({ "error": { "code": -32601, "message": "Method not found" } })
        }
        _ => json!({}),
    })
    .await;
    let tools = resource_tools(&servers(vec![Arc::clone(&caller)]), &WorkGate::default());

    // A listing of every server records the failure instead of failing.
    let listed = run(&tools[0], json!({})).await.unwrap();
    let errors = &listed.structured_content.as_ref().unwrap()["errors"];
    assert_eq!(errors[0]["server"], "docs");
    assert!(
        errors[0]["error"].as_str().unwrap().contains("list failed"),
        "{errors}"
    );
    // A named server's failure is the call's error.
    let one = run(&tools[0], json!({ "server": "docs" })).await;
    assert!(one.unwrap_err().contains("list failed"));
    // Pi's `withoutTemplates`: a server without the method has none.
    let templates = run(&tools[1], json!({ "server": "docs" })).await.unwrap();
    assert_eq!(
        templates.structured_content,
        Some(json!({ "server": "docs", "resourceTemplates": [] }))
    );

    assert_eq!(
        run(&tools[0], json!({ "server": 1 })).await.unwrap_err(),
        "server must be a string"
    );
    assert_eq!(
        run(&tools[0], json!({ "cursor": "c" })).await.unwrap_err(),
        "cursor can only be used when a server is specified"
    );
    assert_eq!(
        run(&tools[2], json!({ "server": "  ", "uri": "x" }))
            .await
            .unwrap_err(),
        "server must be provided"
    );
    assert_eq!(
        run(&tools[2], json!({ "server": "docs", "uri": "" }))
            .await
            .unwrap_err(),
        "uri must be provided"
    );
    caller.client.shutdown().await;
    server.abort();
}

// Pi's `isMcpAppResource` and `listed`.
#[test]
fn mcp_app_resources_and_icons_are_left_out() {
    let entry = |value: Value| value.as_object().cloned().unwrap_or_default();
    assert!(is_mcp_app_resource(&entry(json!({ "uri": "ui://x" }))));
    assert!(is_mcp_app_resource(&entry(
        json!({ "uriTemplate": "ui://x/{y}" })
    )));
    assert!(is_mcp_app_resource(&entry(json!({
        "uri": "docs://x",
        "mimeType": "text/html; PROFILE = \"mcp-app\"",
    }))));
    assert!(!is_mcp_app_resource(&entry(json!({
        "uri": "docs://x",
        "mimeType": "text/html;profile=other",
    }))));
    assert!(!is_mcp_app_resource(&entry(json!({
        "uri": "docs://x",
        "mimeType": "profile=mcp-app",
    }))));
    assert!(!is_mcp_app_resource(&entry(json!({ "uri": "docs://x" }))));
}

// Bake, where Pi's `listed` spreads the entry over the `server` it set: a
// `server` key the listing server sent is left out, so every entry names
// the server Bake reached and no server can steer a read at another one.
#[tokio::test]
async fn a_listed_entry_keeps_bakes_server_attribution() {
    let (caller, _fake, server) = client(|method, _| match method {
        "resources/list" => json!({
            "resources": [
                { "uri": "docs://x", "name": "x", "server": "billing", "title": "X" },
            ],
        }),
        _ => json!({}),
    })
    .await;
    let tools = resource_tools(&servers(vec![Arc::clone(&caller)]), &WorkGate::default());
    let listed = run(&tools[0], json!({ "server": "docs" })).await.unwrap();
    assert_eq!(
        listed.structured_content,
        Some(json!({
            "server": "docs",
            "resources": [
                { "server": "docs", "uri": "docs://x", "name": "x", "title": "X" },
            ],
        }))
    );
    caller.client.shutdown().await;
    server.abort();
}

// Pi's parameter and output schemas, which every request carries.
#[test]
fn the_schemas_are_pis() {
    assert_eq!(
        list_parameters()["properties"]["server"]["description"],
        "MCP server name. Omit to list every server with resources."
    );
    assert_eq!(list_parameters()["additionalProperties"], json!(false));
    assert_eq!(read_parameters()["required"], json!(["server", "uri"]));
    assert_eq!(list_output_schema()["required"], json!(["resources"]));
    assert_eq!(
        list_templates_output_schema()["required"],
        json!(["resourceTemplates"])
    );
    assert_eq!(
        read_output_schema()["required"],
        json!(["server", "uri", "contents"])
    );
    assert!(LIST_RESOURCES_DESCRIPTION.starts_with("Lists resources provided by MCP servers."));
    assert!(
        LIST_TEMPLATES_DESCRIPTION.starts_with("Lists resource templates provided by MCP servers.")
    );
    assert_eq!(
        READ_RESOURCE_DESCRIPTION,
        "Read a specific resource from an MCP server given the server name and resource URI."
    );
}

// Bake: the tools find no server once the registry that published them is
// gone, and say so instead of panicking.
#[tokio::test]
async fn a_withdrawn_registry_leaves_no_server_to_reach() {
    let tools = resource_tools(&servers(Vec::new()), &WorkGate::default());
    assert_eq!(
        run(&tools[2], json!({ "server": "docs", "uri": "docs://x" }))
            .await
            .unwrap_err(),
        "MCP server \"docs\" has no resources"
    );
    let listed = run(&tools[0], json!({})).await.unwrap();
    assert_eq!(listed.structured_content, Some(json!({ "resources": [] })));
}

// Pi's `syncResourceTools`: the three tools are published exactly while a
// connected server offers resources, and withdrawn with it; a server
// without the capability adds none, and a hidden one does not reach them.
#[tokio::test]
async fn the_resource_tools_are_published_with_the_servers_that_offer_resources() {
    use crate::mcp::config::{McpExposure, parse_mcp_config};
    use crate::mcp::{McpServerState, McpServers, McpTimings, McpToolFilter};

    let text = json!({
        "mcpServers": {
            "plain": { "command": "x" },
            "docs": { "command": "x" },
            "quiet": { "command": "x", "exposure": "hidden" },
        }
    });
    let mut entries = parse_mcp_config(&text.to_string(), std::path::Path::new("mcp.json")).servers;
    assert_eq!(entries[2].settings.exposure, McpExposure::Hidden);
    entries.truncate(3);
    let registry = McpServers::new(
        &entries,
        Vec::new(),
        McpTimings::default(),
        McpToolFilter::default(),
        WorkGate::default(),
    );
    let connected = |count: usize| McpServerState::Connected {
        tools: count,
        generation: 1,
        refresh_error: None,
    };
    let (caller, _fake, server) = client(docs_server).await;

    // A server that declared no resources publishes none of the tools.
    registry.publish(0, Vec::new(), connected).unwrap();
    registry.set_connection(0, None, None);
    assert!(registry.tool_names().is_empty());

    // A hidden server does not reach them either.
    registry.publish(2, Vec::new(), connected).unwrap();
    registry.set_connection(2, None, Some(Arc::clone(&caller)));
    assert!(registry.tool_names().is_empty());

    let server_tool = {
        let mut tool = AgentTool::new(
            "mcp__docs__search",
            "docs/search",
            "Search",
            json!({ "type": "object", "properties": {} }),
            |_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::default()) }),
        );
        tool.replay = Some(ToolReplay::Never);
        Arc::new(tool)
    };
    registry
        .publish(1, vec![Arc::clone(&server_tool)], connected)
        .unwrap();
    registry.set_connection(1, Some("  Docs search.  "), Some(Arc::clone(&caller)));
    // Pi's `registerTools`: the server's namespace carries its instructions,
    // which `describeNamespace()` returns to scripts.
    let namespace = registry
        .tool_namespace("mcp__docs__search")
        .expect("a namespace");
    assert_eq!(namespace.namespace.name, "mcp__docs");
    assert_eq!(namespace.aliases, ["docs"]);
    assert_eq!(
        namespace.namespace.instructions.as_deref(),
        Some("Docs search.")
    );
    assert_eq!(
        registry.tool_names(),
        ["mcp__docs__search"]
            .into_iter()
            .chain(RESOURCE_TOOL_NAMES)
            .collect::<Vec<_>>()
    );
    // They belong to no namespace, so scripts call them by name.
    assert!(registry.tool_namespace(READ_MCP_RESOURCE_TOOL).is_none());
    // A read through the published tool reaches the server.
    let tools = registry.tools();
    let read = tools
        .iter()
        .find(|tool| tool.name == READ_MCP_RESOURCE_TOOL)
        .expect("the read tool");
    let result = run(read, json!({ "server": "docs", "uri": "docs://intro" }))
        .await
        .unwrap();
    assert_eq!(texts(&result), ["# docs://intro"]);

    // A disconnected server takes them with it.
    registry.withdraw(
        1,
        McpServerState::Disconnected {
            error: "gone".into(),
        },
    );
    assert!(registry.tool_names().is_empty());
    assert_eq!(registry.servers_prompt_section(), None);
    caller.client.shutdown().await;
    server.abort();
}

// The user's restrictions cover the resource tools, as they cover every MCP
// tool.
#[tokio::test]
async fn tool_restrictions_cover_the_resource_tools() {
    use crate::mcp::config::parse_mcp_config;
    use crate::mcp::{McpServerState, McpServers, McpTimings, McpToolFilter};

    let text = json!({ "mcpServers": { "docs": { "command": "x" } } });
    let entries = parse_mcp_config(&text.to_string(), std::path::Path::new("mcp.json")).servers;
    let registry = McpServers::new(
        &entries,
        Vec::new(),
        McpTimings::default(),
        McpToolFilter::new(None, vec![READ_MCP_RESOURCE_TOOL.into()]),
        WorkGate::default(),
    );
    let (caller, _fake, server) = client(docs_server).await;
    registry
        .publish(0, Vec::new(), |count| McpServerState::Connected {
            tools: count,
            generation: 1,
            refresh_error: None,
        })
        .unwrap();
    registry.set_connection(0, None, Some(Arc::clone(&caller)));
    assert_eq!(registry.tool_names(), RESOURCE_TOOL_NAMES);
    assert_eq!(
        registry.declared_tool_names(),
        [LIST_MCP_RESOURCES_TOOL, LIST_MCP_RESOURCE_TEMPLATES_TOOL]
    );
    caller.client.shutdown().await;
    server.abort();
}
