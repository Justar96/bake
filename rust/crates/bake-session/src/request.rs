//! Development-only request derivation over a closed subset of current-format
//! events.
//!
//! [`RequestFold`] folds one event at a time into the state a model request
//! needs: the derived messages, the request header, and the Session id. It
//! reads no codec types or diagnostics; [`crate::replay`] admits events and
//! converts them to [`Fact`]s first. The fold owns its messages so each
//! request snapshot outlives the parsed rows it came from.
//!
//! Every fold transition matches `packages/core/session` for the subset the
//! import side admits: append-only surfaces and at most one `request/header`.
//! Replacements, message projections, header changes, and tool updates are
//! refused before they reach the fold.

use serde_json::{Map, Value};

/// A surface event type, which fixes how its message derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceKind {
    System,
    User,
    Assistant,
    ToolResult,
}

/// One admitted event's effect on request derivation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fact {
    /// A surface append carrying its whole message object.
    Surface {
        kind: SurfaceKind,
        message: Map<String, Value>,
    },
    /// A `request/header` in canonical form: `config` is copied verbatim and
    /// `tools` is absent or non-empty.
    Header {
        config: Map<String, Value>,
        tools: Option<Vec<Value>>,
    },
    /// An event derivation does not read.
    LogOnly,
}

/// Why the fold refused a fact. The fold is unchanged after a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FoldRefusal {
    /// A second `request/header`. Tool history after a header change depends
    /// on order-sensitive schema comparisons this fold does not implement.
    HeaderChange,
}

#[derive(Debug, Clone, PartialEq)]
struct Header {
    config: Map<String, Value>,
    tools: Option<Vec<Value>>,
}

/// The derivation state of one append-only Session prefix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestFold {
    session_id: String,
    messages: Vec<Map<String, Value>>,
    header: Option<Header>,
}

impl RequestFold {
    pub(crate) const fn new(session_id: String) -> Self {
        Self {
            session_id,
            messages: Vec::new(),
            header: None,
        }
    }

    /// Apply one fact, or refuse it without changing the fold.
    pub(crate) fn append(&mut self, fact: Fact) -> Result<(), FoldRefusal> {
        match fact {
            Fact::Surface { kind, message } => {
                // `deriveEventMessage`: an empty system message records "no
                // system prompt", and an empty assistant message only hosts
                // usage; neither reaches the model.
                let empty = message
                    .get("content")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if !(empty && matches!(kind, SurfaceKind::System | SurfaceKind::Assistant)) {
                    self.messages.push(message);
                }
            }
            Fact::Header { config, tools } => {
                if self.header.is_some() {
                    return Err(FoldRefusal::HeaderChange);
                }
                self.header = Some(Header { config, tools });
            }
            Fact::LogOnly => {}
        }
        Ok(())
    }

    /// The request a step would send after this prefix, or `None` before the
    /// first `request/header`.
    pub(crate) fn request(&self) -> Option<Request> {
        let header = self.header.as_ref()?;
        Some(Request {
            config: header.config.clone(),
            messages: self.messages.clone(),
            tools: header.tools.clone(),
            session_id: self.session_id.clone(),
        })
    }
}

/// One provider-neutral model request derived from a Session prefix.
///
/// Messages are the logged message objects, unchanged: original IDs, sources,
/// unknown members, and `null` members are kept, and an omitted member stays
/// omitted.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    config: Map<String, Value>,
    messages: Vec<Map<String, Value>>,
    tools: Option<Vec<Value>>,
    session_id: String,
}

impl Request {
    /// The request as the JSON value `replayRequests` returns.
    ///
    /// The members are the header's `config` members, then `messages`,
    /// `toolHistory`, `tools` when the header declares tools, and `sessionId`.
    /// With one request header, the tool history is that header's tools and
    /// no updates. Equal values do not imply equal provider wire bytes or
    /// member order.
    pub fn to_json(&self) -> Value {
        let tools = self.tools.clone();
        let mut request = self.config.clone();
        request.insert(
            "messages".to_owned(),
            Value::Array(self.messages.iter().cloned().map(Value::Object).collect()),
        );
        let mut history = Map::new();
        history.insert(
            "tools".to_owned(),
            Value::Array(tools.clone().unwrap_or_default()),
        );
        history.insert("updates".to_owned(), Value::Array(Vec::new()));
        request.insert("toolHistory".to_owned(), Value::Object(history));
        if let Some(tools) = tools {
            request.insert("tools".to_owned(), Value::Array(tools));
        }
        request.insert(
            "sessionId".to_owned(),
            Value::String(self.session_id.clone()),
        );
        Value::Object(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(fields) => fields,
            _ => panic!("object literal"),
        }
    }

    fn header() -> Fact {
        Fact::Header {
            config: object(json!({"provider": "p", "model": "m"})),
            tools: None,
        }
    }

    #[test]
    fn a_refused_header_change_leaves_the_fold_unchanged() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(header()).expect("first header");
        fold.append(Fact::Surface {
            kind: SurfaceKind::User,
            message: object(
                json!({"id": "u", "role": "user", "content": [], "source": {"kind": "user"}}),
            ),
        })
        .expect("user message");
        let before = fold.clone();
        assert_eq!(fold.append(header()), Err(FoldRefusal::HeaderChange));
        assert_eq!(fold, before);
    }

    #[test]
    fn empty_system_and_assistant_messages_derive_nothing() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(header()).expect("header");
        for kind in [
            SurfaceKind::System,
            SurfaceKind::User,
            SurfaceKind::Assistant,
            SurfaceKind::ToolResult,
        ] {
            fold.append(Fact::Surface {
                kind,
                message: object(json!({"id": format!("{kind:?}"), "content": []})),
            })
            .expect("surface");
        }
        let request = fold.request().expect("request");
        let ids: Vec<&Value> = request
            .messages
            .iter()
            .map(|message| &message["id"])
            .collect();
        assert_eq!(ids, [&json!("User"), &json!("ToolResult")]);
    }

    #[test]
    fn no_request_before_a_header() {
        let mut fold = RequestFold::new("s".to_owned());
        fold.append(Fact::LogOnly).expect("log-only");
        assert_eq!(fold.request(), None);
    }
}
