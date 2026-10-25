//! A hostile MCP server through `bake-mcp`'s newline framing and client:
//! everything a server writes is untrusted, from its framing to its
//! JSON-RPC envelopes and result shapes.
//!
//! Input: the server's output. Framing check: `LineReader` over the bytes
//! returns exactly their lines, a `\r` before each `\n` stripped, and stops
//! with the error the remainder calls for. Client run: a scripted server
//! answers each message the client writes with the next line of the input,
//! over in-memory pipes on a paused clock (so a request nobody answers times
//! out at once), while the client initializes, lists and calls tools, lists
//! and reads resources, pings, and shuts down.

#![no_main]

use std::io::Cursor;
use std::num::NonZeroUsize;
use std::time::Duration;

use bake_mcp::{
    ClientOptions, Implementation, LineReader, LineWriter, McpClient, MessageReceiver, ReceiveError,
};
use libfuzzer_sys::fuzz_target;
use serde_json::json;
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

const FRAME_LIMIT: usize = 256;
const MESSAGE_LIMIT: usize = 64 * 1024;

fn limit(bytes: usize) -> NonZeroUsize {
    NonZeroUsize::new(bytes).unwrap_or(NonZeroUsize::MIN)
}

async fn check_framing(script: &[u8]) {
    let mut reader = LineReader::new(Cursor::new(script.to_vec()), limit(FRAME_LIMIT));
    let mut consumed = 0;
    loop {
        let rest = &script[consumed..];
        let newline = rest.iter().position(|byte| *byte == b'\n');
        match reader.receive().await {
            Ok(Some(line)) => {
                let end = newline.expect("a line is returned only at its newline");
                let raw = &rest[..end];
                assert!(raw.len() <= FRAME_LIMIT, "an oversized line was returned");
                assert_eq!(line, raw.strip_suffix(b"\r").unwrap_or(raw));
                consumed += end + 1;
            }
            Ok(None) => {
                assert!(rest.is_empty(), "the stream ended with bytes unread");
                return;
            }
            Err(ReceiveError::Incomplete { bytes }) => {
                assert!(newline.is_none() && bytes == rest.len() && bytes <= FRAME_LIMIT);
                return;
            }
            Err(ReceiveError::TooLarge { .. }) => {
                assert!(newline.unwrap_or(rest.len()) > FRAME_LIMIT);
                return;
            }
            Err(error) => panic!("reading from memory failed: {error}"),
        }
    }
}

/// Answer each message the client writes with the next script line, and
/// hang up when the script runs out or the client closes.
async fn serve(script: Vec<u8>, read: ReadHalf<DuplexStream>, mut write: WriteHalf<DuplexStream>) {
    let mut reader = LineReader::new(read, limit(MESSAGE_LIMIT));
    let mut lines = script.split_inclusive(|byte| *byte == b'\n');
    while let Ok(Some(_)) = reader.receive().await {
        let Some(line) = lines.next() else {
            return;
        };
        if write.write_all(line).await.is_err() {
            return;
        }
    }
}

async fn exchange(script: &[u8]) {
    let (client_io, server_io) = tokio::io::duplex(MESSAGE_LIMIT);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server = tokio::spawn(serve(script.to_vec(), server_read, server_write));
    let mut options = ClientOptions::new(Implementation::new("bake-fuzz", "0.0.0"));
    options.request_timeout = Some(Duration::from_secs(1));
    options.shutdown_timeout = Duration::from_secs(1);
    options.limits.max_message_bytes = limit(MESSAGE_LIMIT);
    options.limits.max_list_pages = limit(8);
    let Ok(mut connecting) = McpClient::start(
        LineReader::new(client_read, limit(MESSAGE_LIMIT)),
        LineWriter::new(client_write),
        options,
    ) else {
        return;
    };
    match connecting.initialize().await {
        Ok((client, mut events)) => {
            let _ = client.server().capabilities.tools();
            let _ = client.server().capabilities.resources();
            let _ = client.list_tools().await;
            let _ = client
                .call_tool("echo", json!({"text": "hi"}).as_object().cloned())
                .await;
            let _ = client.list_resources().await;
            let _ = client.list_resource_templates().await;
            let _ = client.read_resource("file:///fuzz").await;
            let _ = client.ping().await;
            let _ = client.shutdown().await;
            while events.try_recv().is_some() {}
        }
        Err(_) => {
            let _ = connecting.shutdown().await;
        }
    }
    let _ = server.await;
}

fuzz_target!(|data: &[u8]| {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
    else {
        return;
    };
    runtime.block_on(async {
        check_framing(data).await;
        exchange(data).await;
    });
});
