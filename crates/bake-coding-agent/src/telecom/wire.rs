//! Telecom's wire format, version 1: the presence record a member
//! publishes, and the one frame a sender writes to a member's socket with
//! the one response it reads back.
//!
//! Every record is a JSON object with a `v` member. A reader accepts only
//! the version it knows and validates every member it uses before anything
//! of it reaches a model or a screen: identifiers and callsigns match fixed
//! patterns, texts are capped and stripped of control characters other
//! than line breaks and tabs, and a frame line longer than [`FRAME_CAP`]
//! is refused unread. Unknown members are ignored, so a later version may
//! add members without breaking a reader of this one.

use serde::{Deserialize, Serialize};

/// The version this module reads and writes.
pub const WIRE_VERSION: u32 = 1;

/// The longest text a frame may carry, in UTF-8 bytes.
pub const TEXT_CAP: usize = 32 * 1024;

/// The longest frame or response line, in bytes, its line feed excluded.
pub const FRAME_CAP: usize = 64 * 1024;

/// The most recipients one message may name.
pub const GROUP_CAP: usize = 8;

/// The longest task a member publishes, in characters.
pub const TASK_CAP: usize = 400;

/// Whether `word` is a callsign: a lowercase letter or digit, then up to
/// 31 lowercase letters, digits, `-`, or `_`.
pub fn is_callsign(word: &str) -> bool {
    let bytes = word.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// Whether `id` is a frame, ask, or session id: 1 to 128 ASCII letters,
/// digits, `.`, `_`, or `-`.
pub fn is_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `text` without control characters other than line feeds and tabs,
/// carriage returns folded into line feeds.
pub fn clean_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// `text` on one line, cleaned, and cut to `cap` characters ending in
/// `...`.
pub fn one_line(text: &str, cap: usize) -> String {
    let flat = clean_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= cap {
        return flat;
    }
    let mut cut: String = flat.chars().take(cap.saturating_sub(3)).collect();
    cut.push_str(&"..."[..cap.min(3)]);
    cut
}

/// Whether a member is in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    Idle,
    Busy,
}

/// What a member publishes in its `<callsign>.json`, rewritten only by
/// the member while it holds `<callsign>.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    pub v: u32,
    /// The member's session id.
    pub session: String,
    pub callsign: String,
    pub pid: u32,
    /// The checkout it works in.
    pub checkout: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What it works on, in one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub activity: Activity,
    /// When `activity` last changed, in Unix milliseconds.
    pub since: i64,
    /// The tool call a busy member is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The callsign of the member that spawned this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// `provider/model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// When the member joined, in Unix milliseconds.
    pub started_at: i64,
}

impl Presence {
    /// Reads a record, or `None` when it is not a valid version 1 record
    /// for `callsign`. Texts are normalized as a model may see them.
    pub fn decode(bytes: &[u8], callsign: &str) -> Option<Self> {
        if bytes.len() > FRAME_CAP {
            return None;
        }
        let mut record: Self = serde_json::from_slice(bytes).ok()?;
        let valid = record.v == WIRE_VERSION
            && record.callsign == callsign
            && is_id(&record.session)
            && record.parent.as_deref().is_none_or(is_callsign);
        if !valid {
            return None;
        }
        record.task = record
            .task
            .map(|task| one_line(&task, TASK_CAP))
            .filter(|task| !task.is_empty());
        record.branch = record.branch.map(|branch| one_line(&branch, 120));
        record.tool = record.tool.map(|tool| one_line(&tool, 64));
        record.model = record.model.map(|model| one_line(&model, 120));
        record.checkout = one_line(&record.checkout, 1024);
        Some(record)
    }
}

/// Who sent a frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sender {
    pub session: String,
    pub callsign: String,
}

/// What a frame carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Body {
    /// A message. `group` names the other recipients of the same message;
    /// `human` marks one a person typed rather than a model.
    Message {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        group: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        human: bool,
    },
    /// A question the sender waits on, answered by a `reply` with its id.
    Ask { ask_id: String, text: String },
    /// The answer to an ask the recipient made.
    Reply { ask_id: String, text: String },
    /// A message to the whole team; it never starts a turn.
    Broadcast { text: String },
    /// The sender stopped waiting on its ask.
    Withdraw { ask_id: String },
}

/// One frame: written as one line to a member's socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub v: u32,
    /// Fresh for every frame.
    pub id: String,
    pub from: Sender,
    /// Unix milliseconds.
    pub sent_at: i64,
    pub body: Body,
}

/// Why a frame line was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooLong,
    Malformed(String),
    Version(u32),
    Invalid(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => write!(f, "frame longer than {FRAME_CAP} bytes"),
            Self::Malformed(error) => write!(f, "malformed frame: {error}"),
            Self::Version(v) => write!(f, "unsupported wire version {v}"),
            Self::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl Frame {
    /// A version 1 frame from `from`, with a fresh id.
    pub fn new(from: Sender, body: Body) -> Self {
        Self {
            v: WIRE_VERSION,
            id: crate::session::id::uuid_v4(),
            from,
            sent_at: bake_ai::now_ms(),
            body,
        }
    }

    /// The frame as one line, its line feed included.
    pub fn encode(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }

    /// Reads one frame line (without its line feed), validating every
    /// member a recipient uses. Texts are cleaned of control characters.
    pub fn decode(line: &[u8]) -> Result<Self, DecodeError> {
        if line.len() > FRAME_CAP {
            return Err(DecodeError::TooLong);
        }
        let probe: serde_json::Value = serde_json::from_slice(line)
            .map_err(|error| DecodeError::Malformed(error.to_string()))?;
        let version = probe.get("v").and_then(serde_json::Value::as_u64);
        if version != Some(u64::from(WIRE_VERSION)) {
            return Err(DecodeError::Version(
                version.and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
            ));
        }
        let mut frame: Self = serde_json::from_value(probe)
            .map_err(|error| DecodeError::Malformed(error.to_string()))?;
        if !is_id(&frame.id) {
            return Err(DecodeError::Invalid("frame id"));
        }
        if !is_id(&frame.from.session) {
            return Err(DecodeError::Invalid("sender session"));
        }
        if !is_callsign(&frame.from.callsign) {
            return Err(DecodeError::Invalid("sender callsign"));
        }
        match &mut frame.body {
            Body::Message { text, group, .. } => {
                if group.len() > GROUP_CAP || !group.iter().all(|name| is_callsign(name)) {
                    return Err(DecodeError::Invalid("group"));
                }
                clean_body_text(text)?;
            }
            Body::Ask { ask_id, text } | Body::Reply { ask_id, text } => {
                if !is_id(ask_id) {
                    return Err(DecodeError::Invalid("ask id"));
                }
                clean_body_text(text)?;
            }
            Body::Broadcast { text } => clean_body_text(text)?,
            Body::Withdraw { ask_id } => {
                if !is_id(ask_id) {
                    return Err(DecodeError::Invalid("ask id"));
                }
            }
        }
        Ok(frame)
    }
}

fn clean_body_text(text: &mut String) -> Result<(), DecodeError> {
    if text.len() > TEXT_CAP {
        return Err(DecodeError::Invalid("text: too long"));
    }
    *text = clean_text(text);
    if text.trim().is_empty() {
        return Err(DecodeError::Invalid("text: empty"));
    }
    Ok(())
}

/// What a recipient answers a frame with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    /// The frame was handed to the recipient's session.
    Accepted,
    /// It was not, for `reason`.
    Refused { reason: String },
}

/// The response line to the frame `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }

    /// Reads a response to the frame `id`.
    pub fn decode(line: &[u8], id: &str) -> Result<Outcome, DecodeError> {
        if line.len() > FRAME_CAP {
            return Err(DecodeError::TooLong);
        }
        let response: Self = serde_json::from_slice(line)
            .map_err(|error| DecodeError::Malformed(error.to_string()))?;
        if response.v != WIRE_VERSION {
            return Err(DecodeError::Version(response.v));
        }
        if response.id != id {
            return Err(DecodeError::Invalid("response id"));
        }
        Ok(match response.outcome {
            Outcome::Refused { reason } => Outcome::Refused {
                reason: one_line(&reason, 400),
            },
            accepted => accepted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sender() -> Sender {
        Sender {
            session: "s-1".into(),
            callsign: "alpha".into(),
        }
    }

    #[test]
    fn frames_round_trip_with_their_wire_names() {
        let frame = Frame::new(
            sender(),
            Body::Ask {
                ask_id: "a1".into(),
                text: "Is users.email unique?".into(),
            },
        );
        let line = frame.encode();
        let text = std::str::from_utf8(&line).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains(r#""kind":"ask""#), "{text}");
        assert!(text.contains(r#""askId":"a1""#), "{text}");
        assert!(text.contains(r#""sentAt":"#), "{text}");
        assert_eq!(Frame::decode(&line[..line.len() - 1]), Ok(frame));
    }

    #[test]
    fn decode_refuses_what_it_cannot_trust() {
        let base = |body: &str| {
            format!(
                r#"{{"v":1,"id":"f1","from":{{"session":"s","callsign":"alpha"}},"sentAt":1,"body":{body}}}"#
            )
        };
        let message = base(r#"{"kind":"message","text":"hi"}"#);
        assert!(Frame::decode(message.as_bytes()).is_ok());
        assert_eq!(
            Frame::decode(message.replace(r#""v":1"#, r#""v":2"#).as_bytes()),
            Err(DecodeError::Version(2))
        );
        assert_eq!(
            Frame::decode(message.replace("alpha", "Alpha").as_bytes()),
            Err(DecodeError::Invalid("sender callsign"))
        );
        assert_eq!(
            Frame::decode(base(r#"{"kind":"message","text":" \u0007 "}"#).as_bytes()),
            Err(DecodeError::Invalid("text: empty"))
        );
        assert_eq!(
            Frame::decode(base(r#"{"kind":"reply","askId":"../x","text":"y"}"#).as_bytes()),
            Err(DecodeError::Invalid("ask id"))
        );
        assert!(matches!(
            Frame::decode(base(r#"{"kind":"shout","text":"y"}"#).as_bytes()),
            Err(DecodeError::Malformed(_))
        ));
        assert_eq!(
            Frame::decode(&vec![b' '; FRAME_CAP + 1]),
            Err(DecodeError::TooLong)
        );
    }

    #[test]
    fn responses_answer_only_their_frame() {
        let refused = Response {
            v: 1,
            id: "f1".into(),
            outcome: Outcome::Refused {
                reason: "telecom is off".into(),
            },
        };
        let line = refused.encode();
        let text = std::str::from_utf8(&line).unwrap();
        assert!(text.contains(r#""status":"refused""#), "{text}");
        let line = &line[..line.len() - 1];
        assert_eq!(Response::decode(line, "f1"), Ok(refused.outcome));
        assert_eq!(
            Response::decode(line, "f2"),
            Err(DecodeError::Invalid("response id"))
        );
    }

    #[test]
    fn presence_records_are_read_only_for_their_callsign() {
        let record = Presence {
            v: 1,
            session: "s-1".into(),
            callsign: "bravo".into(),
            pid: 7,
            checkout: "/repo".into(),
            branch: Some("main".into()),
            task: Some("fix\nthe\u{7} test".into()),
            activity: Activity::Busy,
            since: 1,
            tool: Some("bash".into()),
            parent: None,
            model: None,
            started_at: 1,
        };
        let bytes = serde_json::to_vec(&record).unwrap();
        let read = Presence::decode(&bytes, "bravo").unwrap();
        assert_eq!(read.task.as_deref(), Some("fix the test"));
        assert_eq!(Presence::decode(&bytes, "alpha"), None);
    }

    proptest! {
        #[test]
        fn decoded_frames_hold_their_invariants(line in proptest::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(frame) = Frame::decode(&line) {
                prop_assert_eq!(frame.v, WIRE_VERSION);
                prop_assert!(is_id(&frame.id) && is_callsign(&frame.from.callsign));
            }
        }

        #[test]
        fn decoded_texts_carry_no_control_characters(text in "\\PC{0,64}[\\x00-\\x1f]{0,4}\\PC{0,64}") {
            let line = serde_json::json!({
                "v": 1, "id": "f", "from": {"session": "s", "callsign": "a"},
                "sentAt": 0, "body": {"kind": "broadcast", "text": text},
            }).to_string();
            if let Ok(Frame { body: Body::Broadcast { text }, .. }) = Frame::decode(line.as_bytes()) {
                prop_assert!(text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'));
                prop_assert!(!text.trim().is_empty());
            }
        }

        #[test]
        fn one_line_is_bounded(text in "\\PC{0,300}", cap in 1usize..80) {
            let line = one_line(&text, cap);
            prop_assert!(line.chars().count() <= cap);
            prop_assert!(!line.contains('\n'));
        }

        #[test]
        fn callsigns_are_safe_file_names(word in "\\PC{0,40}") {
            if is_callsign(&word) {
                prop_assert!(!word.contains('/') && !word.contains('.') && word.len() <= 32);
            }
        }
    }
}
