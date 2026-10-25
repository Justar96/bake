//! JSON-RPC 2.0 messages as MCP uses them.
//!
//! Ported from Pi's `packages/mcp/src/protocol/jsonrpc.ts` (v1.3.0). Pi checks
//! message shapes with type guards; here one parser classifies every incoming
//! message and reports why a message is malformed instead of throwing. Batches
//! (JSON arrays) are reported as unsupported: MCP 2025-06-18 removed them, and
//! Pi does not accept them either.

use std::fmt;

use serde_json::{Map, Number, Value};

/// The JSON-RPC error codes MCP uses.
pub mod error_codes {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist or is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// The id of a request the server sent the client.
///
/// MCP ids are strings or integers; Pi also accepts any finite number, so a
/// fractional id is kept as the server wrote it and echoed back unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestId {
    /// A numeric id.
    Number(Number),
    /// A string id.
    String(String),
}

impl RequestId {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => Some(Self::Number(number.clone())),
            Value::String(text) => Some(Self::String(text.clone())),
            _ => None,
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Number(number) => Value::Number(number.clone()),
            Self::String(text) => Value::String(text.clone()),
        }
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::String(text) => write!(f, "{:?}", preview(text, 64)),
        }
    }
}

/// The error object of a JSON-RPC error response.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonRpcError {
    /// The error code; see [`error_codes`].
    pub code: i64,
    /// The server's message.
    pub message: String,
    /// The server's additional data, if any.
    pub data: Option<Value>,
}

impl fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", preview(&self.message, 500), self.code)?;
        if let Some(data) = &self.data {
            write!(f, "; data: {}", preview(&data.to_string(), 300))?;
        }
        Ok(())
    }
}

/// One classified incoming message.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// A response to a request the client sent.
    Response { id: Value, outcome: Outcome },
    /// A JSON-RPC error with a null id: the server could not read a message.
    ErrorWithoutId(JsonRpcError),
    /// A request from the server.
    Request { id: RequestId, method: String },
    /// A notification from the server.
    Notification {
        method: String,
        params: Option<Value>,
    },
}

/// What a response carries.
#[derive(Debug)]
pub(crate) enum Outcome {
    Result(Value),
    Error(JsonRpcError),
    /// A response whose shape is wrong: both or neither of `result` and
    /// `error`, or an error object without an integer code and a string
    /// message. It still names an id, so the request it answers can fail.
    Invalid(&'static str),
}

/// Why an incoming message was not a JSON-RPC message.
#[derive(Debug)]
pub(crate) enum Malformed {
    Json(serde_json::Error),
    Batch,
    NotAnObject,
    Version,
    Shape(&'static str),
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "not JSON: {error}"),
            Self::Batch => f.write_str("a JSON-RPC batch, which MCP does not use"),
            Self::NotAnObject => f.write_str("not a JSON object"),
            Self::Version => f.write_str("\"jsonrpc\" is not \"2.0\""),
            Self::Shape(detail) => f.write_str(detail),
        }
    }
}

/// Classify one message. serde_json's recursion limit (128) bounds the
/// nesting, so a hostile message cannot exhaust the stack.
pub(crate) fn parse(bytes: &[u8]) -> Result<Incoming, Malformed> {
    let value: Value = serde_json::from_slice(bytes).map_err(Malformed::Json)?;
    let mut object = match value {
        Value::Object(object) => object,
        Value::Array(_) => return Err(Malformed::Batch),
        _ => return Err(Malformed::NotAnObject),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Malformed::Version);
    }
    if let Some(method) = object.get("method") {
        let Some(method) = method.as_str().map(str::to_owned) else {
            return Err(Malformed::Shape("\"method\" is not a string"));
        };
        return match object.get("id") {
            None => Ok(Incoming::Notification {
                method,
                params: object.remove("params"),
            }),
            Some(id) => match RequestId::from_value(id) {
                Some(id) => Ok(Incoming::Request { id, method }),
                None => Err(Malformed::Shape("request id is not a string or number")),
            },
        };
    }
    let Some(id) = object.remove("id") else {
        return Err(Malformed::Shape("neither \"method\" nor \"id\""));
    };
    let outcome = match (object.remove("result"), object.remove("error")) {
        (Some(_), Some(_)) => Outcome::Invalid("the response has both \"result\" and \"error\""),
        (Some(result), None) => Outcome::Result(result),
        (None, Some(error)) => match error_object(error) {
            Some(error) => Outcome::Error(error),
            None => Outcome::Invalid(
                "the response's error has no integer \"code\" and string \"message\"",
            ),
        },
        (None, None) => Outcome::Invalid("the response has neither \"result\" nor \"error\""),
    };
    match (id, outcome) {
        (Value::Null, Outcome::Error(error)) => Ok(Incoming::ErrorWithoutId(error)),
        (id @ (Value::Number(_) | Value::String(_)), outcome) => {
            Ok(Incoming::Response { id, outcome })
        }
        _ => Err(Malformed::Shape("response id is not a string or number")),
    }
}

fn error_object(value: Value) -> Option<JsonRpcError> {
    let Value::Object(mut object) = value else {
        return None;
    };
    let code = object.get("code").and_then(Value::as_i64)?;
    let Value::String(message) = object.remove("message")? else {
        return None;
    };
    Some(JsonRpcError {
        code,
        message,
        data: object.remove("data"),
    })
}

/// A request's bytes. `params` keeps its key order (serde_json's
/// `preserve_order`), so the wire form matches Pi's `JSON.stringify`.
pub(crate) fn encode_request(id: u64, method: &str, params: Option<Map<String, Value>>) -> Vec<u8> {
    let mut message = Map::new();
    message.insert("jsonrpc".into(), "2.0".into());
    message.insert("id".into(), id.into());
    message.insert("method".into(), method.into());
    if let Some(params) = params {
        message.insert("params".into(), Value::Object(params));
    }
    // `Value`'s `Display` cannot fail, unlike `to_vec`'s signature.
    Value::Object(message).to_string().into_bytes()
}

pub(crate) fn encode_notification(method: &str, params: Option<Map<String, Value>>) -> Vec<u8> {
    let mut message = Map::new();
    message.insert("jsonrpc".into(), "2.0".into());
    message.insert("method".into(), method.into());
    if let Some(params) = params {
        message.insert("params".into(), Value::Object(params));
    }
    Value::Object(message).to_string().into_bytes()
}

pub(crate) fn encode_result(id: &RequestId, result: Value) -> Vec<u8> {
    let mut message = Map::new();
    message.insert("jsonrpc".into(), "2.0".into());
    message.insert("id".into(), id.to_value());
    message.insert("result".into(), result);
    Value::Object(message).to_string().into_bytes()
}

pub(crate) fn encode_error(id: &RequestId, code: i64, text: String) -> Vec<u8> {
    let mut error = Map::new();
    error.insert("code".into(), code.into());
    error.insert("message".into(), text.into());
    let mut message = Map::new();
    message.insert("jsonrpc".into(), "2.0".into());
    message.insert("id".into(), id.to_value());
    message.insert("error".into(), Value::Object(error));
    Value::Object(message).to_string().into_bytes()
}

/// At most `max` characters of `text`, marked when cut. Server text reaches
/// errors and diagnostics only through this, so one cannot carry a whole
/// message.
pub(crate) fn preview(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((end, _)) => format!("{}…", &text[..end]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Result<Incoming, Malformed> {
        parse(text.as_bytes())
    }

    #[test]
    fn classifies_each_message_kind() {
        assert!(matches!(
            parsed(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Ok(Incoming::Response {
                outcome: Outcome::Result(_),
                ..
            })
        ));
        assert!(matches!(
            parsed(r#"{"jsonrpc":"2.0","id":"a","error":{"code":-1,"message":"m","data":[1]}}"#),
            Ok(Incoming::Response {
                outcome: Outcome::Error(JsonRpcError { code: -1, .. }),
                ..
            })
        ));
        assert!(matches!(
            parsed(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse"}}"#),
            Ok(Incoming::ErrorWithoutId(_))
        ));
        assert!(matches!(
            parsed(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#),
            Ok(Incoming::Request {
                id: RequestId::Number(_),
                ..
            })
        ));
        assert!(matches!(
            parsed(r#"{"jsonrpc":"2.0","method":"notifications/x","params":{"a":1}}"#),
            Ok(Incoming::Notification {
                params: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn reports_why_a_message_is_malformed() {
        let cases = [
            ("{", "not JSON"),
            ("[]", "batch"),
            ("3", "not a JSON object"),
            (r#"{"id":1,"result":{}}"#, "\"jsonrpc\""),
            (r#"{"jsonrpc":"1.0","id":1,"result":{}}"#, "\"jsonrpc\""),
            (
                r#"{"jsonrpc":"2.0","method":7}"#,
                "\"method\" is not a string",
            ),
            (r#"{"jsonrpc":"2.0","id":{},"method":"x"}"#, "request id"),
            (r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#, "request id"),
            (r#"{"jsonrpc":"2.0","result":{}}"#, "neither"),
            (r#"{"jsonrpc":"2.0","id":true,"result":{}}"#, "response id"),
            (r#"{"jsonrpc":"2.0","id":null,"result":{}}"#, "response id"),
        ];
        for (text, expected) in cases {
            let error = parsed(text).expect_err(text).to_string();
            assert!(error.contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn invalid_responses_keep_their_id() {
        for text in [
            r#"{"jsonrpc":"2.0","id":4,"result":{},"error":{"code":1,"message":"m"}}"#,
            r#"{"jsonrpc":"2.0","id":4}"#,
            r#"{"jsonrpc":"2.0","id":4,"error":{"code":1.5,"message":"m"}}"#,
            r#"{"jsonrpc":"2.0","id":4,"error":{"code":1}}"#,
            r#"{"jsonrpc":"2.0","id":4,"error":"boom"}"#,
        ] {
            assert!(
                matches!(
                    parsed(text),
                    Ok(Incoming::Response {
                        outcome: Outcome::Invalid(_),
                        ..
                    })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing_the_stack() {
        let depth = 100_000;
        let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(matches!(parse(text.as_bytes()), Err(Malformed::Json(_))));
    }

    #[test]
    fn encodes_messages_in_pi_key_order() {
        let mut params = Map::new();
        params.insert("name".into(), "echo".into());
        assert_eq!(
            String::from_utf8(encode_request(2, "tools/call", Some(params))).unwrap(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo"}}"#
        );
        assert_eq!(
            String::from_utf8(encode_notification("notifications/initialized", None)).unwrap(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        );
        let id = RequestId::String("r".into());
        assert_eq!(
            String::from_utf8(encode_error(&id, -32601, "Method not found: x".into())).unwrap(),
            r#"{"jsonrpc":"2.0","id":"r","error":{"code":-32601,"message":"Method not found: x"}}"#
        );
    }

    #[test]
    fn previews_cut_on_character_boundaries() {
        assert_eq!(preview("héllo", 2), "hé…");
        assert_eq!(preview("hé", 2), "hé");
        assert_eq!(preview("", 0), "");
    }
}
