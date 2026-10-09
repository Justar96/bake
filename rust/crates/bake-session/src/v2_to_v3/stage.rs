//! `ReleasedV2ToV3Stage` and `renamePtcEvent` from
//! `session-format-v2-to-v3/src/migration.ts`.

use std::collections::HashSet;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::admission::assert_source_event;
use super::canonical::canonicalize;
use super::header::SourceHeader;
use super::js::{count, is_count, record};
use super::references::remap_event;
use super::{SURFACE_TYPES, StageError};
use crate::json_parse::{Deep, clone_fields, clone_value, dismantle, replace_member};

const SYSTEM_PLUGIN: &str = "@deepseek-ai/dsh-system-prompt";
const SYSTEM_ID_PREFIX: &str = "v2-to-v3-system-";
const IDENTITY_TAG: &str = "session-format-v2-to-v3";
/// Source admission guarantees the shapes this stage reads after it; this
/// limit marks a broken guarantee instead of guessing JavaScript's `TypeError`.
const INVARIANT: &str = "admission-invariant";

/// The stage's streaming state: target seqs, identities, cut, and system head.
pub(super) struct Stage {
    header: SourceHeader,
    mapping: Vec<u64>,
    original_ids: HashSet<String>,
    generated_ids: HashSet<String>,
    target_seq: u64,
    source_cut: Option<u64>,
    target_cut: Option<u64>,
    last_foreign_delivery: Option<u64>,
    /// The open step's `turn` and `step` values as the source carried them.
    step: Option<(Value, Value)>,
    head: Option<u64>,
    prompt: String,
    events: Deep<Vec<Value>>,
}

impl Stage {
    pub(super) fn new(header: SourceHeader) -> Self {
        let cut = (!header.is_seeded).then_some(0);
        Self {
            header,
            mapping: Vec::new(),
            original_ids: HashSet::new(),
            generated_ids: HashSet::new(),
            target_seq: 0,
            source_cut: cut,
            target_cut: cut,
            last_foreign_delivery: None,
            step: None,
            head: None,
            prompt: String::new(),
            events: Deep::default(),
        }
    }

    /// `transformEvent` for one decoded logical event.
    pub(super) fn transform(&mut self, event: Value) -> Result<(), StageError> {
        let event = Deep::new(event);
        let seq = self.mapping.len() as u64;
        if !dense(event.get("seq"), seq) {
            return Err(StageError::Invalid(
                "format v2 source events must be dense".to_owned(),
            ));
        }
        assert_source_event(&event)?;
        let event = match event.into_inner() {
            Value::Object(event) => Deep::new(event),
            other => {
                dismantle(other);
                return Err(StageError::NativeLimit(INVARIANT.to_owned()));
            }
        };
        let Some(Value::String(event_type)) = event.get("type") else {
            return Err(StageError::NativeLimit(INVARIANT.to_owned()));
        };
        let event_type = event_type.clone();
        let time = Deep::new(clone_value(&event["time"]));
        self.observe_message_ids(&event_type, &event)?;
        let data = Deep::new(clone_fields(record(event.get("data"), &event_type)?));
        let mut source = event;
        if event_type == "request/header" {
            let mut header = Deep::new(clone_fields(record(data.get("header"), "request header")?));
            let prompt = match header.shift_remove("system") {
                Some(Value::String(prompt)) => prompt,
                other => {
                    other.into_iter().for_each(dismantle);
                    String::new()
                }
            };
            if prompt != self.prompt {
                self.emit_system(prompt, seq, &event_type, &time)?;
            }
            let mut data = Deep::new(clone_fields(&data));
            replace_member(&mut data, "header", Value::Object(header.into_inner()));
            replace_member(&mut source, "data", Value::Object(data.into_inner()));
        }
        if SURFACE_TYPES.contains(&event_type.as_str()) && self.head.is_none() {
            return Err(StageError::Unsupported(
                "format v2 surface before first step cannot acquire a system head without changing chronology"
                    .to_owned(),
            ));
        }
        if event_type == "session/end-seed" && data.get("inherited") == Some(&Value::Bool(true)) {
            if !self.header.is_seeded {
                return Err(StageError::Invalid(
                    "format v2 unseeded Session contains an inherited end-seed marker".to_owned(),
                ));
            }
            self.source_cut = Some(seq);
            self.target_cut = Some(self.target_seq);
        }
        if event_type == "session-log-deepseek/delivery-accepted" {
            let version = data.get("sessionFormatVersion");
            // Admission counts a present version, so `===` compares integers.
            if version.is_some_and(|version| !version.is_u64()) {
                return Err(StageError::NativeLimit(INVARIANT.to_owned()));
            }
            if is_count(version, 3) {
                return Err(StageError::Invalid(
                    "format v2 delivery marker claims target format v3".to_owned(),
                ));
            }
            if is_count(version, 2)
                && data.get("sessionId").and_then(Value::as_str) != Some(self.header.id.as_str())
            {
                self.last_foreign_delivery = Some(seq);
            }
        }
        let target = remap_event(source.into_inner(), seq, self.target_seq, &self.mapping)?;
        self.mapping.push(self.target_seq);
        self.target_seq += 1;
        let target = canonicalize(rename_ptc_event(&event_type, target)?)?;
        self.events.push(target);
        match event_type.as_str() {
            "step/start" => {
                let coordinate = |key: &str| data.get(key).map_or(Value::Null, clone_value);
                self.step = Some((coordinate("turn"), coordinate("step")));
                if self.head.is_none() {
                    self.emit_system(String::new(), seq, &event_type, &time)?;
                }
            }
            "step/end" | "turn/end" => self.step = None,
            _ => {}
        }
        Ok(())
    }

    /// `finish`: the target cut, after the foreign delivery check.
    pub(super) fn finish(self) -> Result<(Vec<Value>, u64), StageError> {
        let cut = count(
            self.source_cut.map(Value::from).as_ref(),
            "format v2 inherited end-seed marker",
            INVARIANT,
        )?;
        if self
            .last_foreign_delivery
            .is_some_and(|seq| !self.header.has_parent || seq >= cut)
        {
            return Err(StageError::Invalid(
                "current-generation delivery marker names the wrong Session".to_owned(),
            ));
        }
        let target_cut = count(
            self.target_cut.map(Value::from).as_ref(),
            "format v3 inherited event count",
            INVARIANT,
        )?;
        Ok((self.events.into_inner(), target_cut))
    }

    fn observe_message_ids(
        &mut self,
        event_type: &str,
        event: &Map<String, Value>,
    ) -> Result<(), StageError> {
        let data = record(event.get("data"), event_type)?;
        let messages: Vec<&Value> = match event_type {
            "user/message" => vec![&event["data"]],
            "assistant/message" | "tool/result" => {
                record(data.get("message"), "message")?;
                vec![&data["message"]]
            }
            "agent/inbox/spliced" => message_list(data.get("inserted"))?,
            "session/title-llm-request" => message_list(data.get("messages"))?,
            _ => Vec::new(),
        };
        for message in messages {
            // Admission requires every owned message identity to be a non-empty string.
            let Some(Value::String(id)) = message.get("id") else {
                return Err(StageError::NativeLimit(INVARIANT.to_owned()));
            };
            if self.generated_ids.contains(id) {
                return Err(StageError::Unsupported(
                    "source message id collides with a generated system message id".to_owned(),
                ));
            }
            self.original_ids.insert(id.clone());
        }
        Ok(())
    }

    /// `emitSystem`: a head message anchored at source event `anchor_seq`.
    fn emit_system(
        &mut self,
        prompt: String,
        anchor_seq: u64,
        anchor_type: &str,
        anchor_time: &Value,
    ) -> Result<(), StageError> {
        let Some((turn, step)) = self.step.clone() else {
            return Err(StageError::Unsupported(
                "format v2 changed request prompt outside an open step cannot retain source chronology"
                    .to_owned(),
            ));
        };
        let id = system_message_id(&self.header.id, anchor_seq, anchor_type);
        if self.original_ids.contains(&id) || self.generated_ids.contains(&id) {
            return Err(StageError::Unsupported(
                "generated system message id collides with an existing message id".to_owned(),
            ));
        }
        self.generated_ids.insert(id.clone());
        let seq = self.target_seq;
        self.target_seq += 1;
        let mut source = Map::new();
        source.insert("kind".to_owned(), Value::from("plugin"));
        source.insert("plugin".to_owned(), Value::from(SYSTEM_PLUGIN));
        let content = if prompt.is_empty() {
            Vec::new()
        } else {
            let mut block = Map::new();
            block.insert("type".to_owned(), Value::from("text"));
            block.insert("text".to_owned(), Value::from(prompt.as_str()));
            vec![Value::Object(block)]
        };
        let mut message = Map::new();
        message.insert("id".to_owned(), Value::String(id));
        message.insert("role".to_owned(), Value::from("system"));
        message.insert("source".to_owned(), Value::Object(source));
        message.insert("content".to_owned(), Value::Array(content));
        let mut data = Map::new();
        data.insert("turn".to_owned(), turn);
        data.insert("step".to_owned(), step);
        data.insert("message".to_owned(), Value::Object(message));
        let mut event = Map::new();
        event.insert("type".to_owned(), Value::from("system/message"));
        event.insert("seq".to_owned(), Value::from(seq));
        event.insert("time".to_owned(), anchor_time.clone());
        event.insert("data".to_owned(), Value::Object(data));
        match self.head {
            None => {
                event.insert("surfaceOp".to_owned(), Value::from("append"));
            }
            Some(head) => {
                // Canonicalization turns released start/end into startSeq/endSeq.
                let mut operation = Map::new();
                operation.insert("op".to_owned(), Value::from("replace"));
                operation.insert("start".to_owned(), Value::from(head));
                operation.insert("end".to_owned(), Value::from(head));
                event.insert("surfaceOp".to_owned(), Value::Object(operation));
                event.insert(
                    "sourceEventSeqs".to_owned(),
                    Value::Array(vec![Value::from(head)]),
                );
            }
        }
        self.events.push(canonicalize(event)?);
        self.head = Some(seq);
        self.prompt = prompt;
        Ok(())
    }
}

/// `'v2-to-v3-system-'` and the hex SHA-256 of
/// `JSON.stringify(['session-format-v2-to-v3', id, seq, type])`, written by
/// [`crate::json_text`], which escapes a lone surrogate in `id` as
/// `JSON.stringify` does.
pub(super) fn system_message_id(session_id: &str, anchor_seq: u64, anchor_type: &str) -> String {
    let identity = Value::Array(vec![
        Value::from(IDENTITY_TAG),
        Value::from(session_id),
        Value::from(anchor_seq),
        Value::from(anchor_type),
    ]);
    let digest = Sha256::digest(crate::json_text(&identity).as_bytes());
    let mut id = String::with_capacity(SYSTEM_ID_PREFIX.len() + 64);
    id.push_str(SYSTEM_ID_PREFIX);
    for byte in digest {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// `event.seq !== this.mapping.length` under JavaScript's `===`: the
/// decoder admits a -0 seq only at position 0.
fn dense(seq: Option<&Value>, expected: u64) -> bool {
    match seq {
        Some(Value::Number(number)) => {
            number.as_u64() == Some(expected)
                || (expected == 0
                    && number
                        .as_f64()
                        .is_some_and(|number| number == 0.0 && number.is_sign_negative()))
        }
        _ => false,
    }
}

fn message_list(value: Option<&Value>) -> Result<Vec<&Value>, StageError> {
    match value {
        Some(Value::Array(items)) => Ok(items.iter().collect()),
        _ => Err(StageError::NativeLimit(INVARIANT.to_owned())),
    }
}

/// `renamePtcEvent`: the preset value, the dispatch event names, and plugin
/// attribution in the three owned message source slots. Content, tool
/// arguments, message IDs, and other payloads keep `code` spellings.
fn rename_ptc_event(
    event_type: &str,
    mut event: Map<String, Value>,
) -> Result<Map<String, Value>, StageError> {
    let renamed_type = match event_type {
        "tool/code-dispatch-start" => Some("tool/ptc-dispatch-start"),
        "tool/code-dispatch" => Some("tool/ptc-dispatch"),
        _ => None,
    };
    if let Some(renamed) = renamed_type {
        event.insert("type".to_owned(), Value::from(renamed));
        return Ok(event);
    }
    let Some(Value::Object(data)) = event.get_mut("data") else {
        return Err(StageError::NativeLimit(INVARIANT.to_owned()));
    };
    match event_type {
        "agent-preset/selected" => {
            if data
                .get("agentPreset")
                .is_some_and(|preset| preset == "code")
            {
                data.insert("agentPreset".to_owned(), Value::from("ptc"));
            }
        }
        "user/message" => rename_message_source(data)?,
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let key = if event_type == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            let Some(Value::Array(messages)) = data.get_mut(key) else {
                return Err(StageError::NativeLimit(INVARIANT.to_owned()));
            };
            for message in messages {
                let Value::Object(message) = message else {
                    return Err(StageError::NativeLimit(INVARIANT.to_owned()));
                };
                rename_message_source(message)?;
            }
        }
        _ => {}
    }
    Ok(event)
}

fn rename_message_source(message: &mut Map<String, Value>) -> Result<(), StageError> {
    let Some(Value::Object(source)) = message.get_mut("source") else {
        return Err(StageError::NativeLimit(INVARIANT.to_owned()));
    };
    if source.get("kind").is_some_and(|kind| kind == "plugin")
        && source
            .get("plugin")
            .is_some_and(|plugin| plugin == "tools-code-mode")
    {
        source.insert("plugin".to_owned(), Value::from("tools-ptc"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_ids_hash_the_json_identity() {
        // Expected digests come from node:crypto over JSON.stringify of the same array.
        let identity = Value::Array(vec![
            Value::from(IDENTITY_TAG),
            Value::from("s\"\n\u{1f}\u{7f}\u{2028}"),
            Value::from(3_u64),
            Value::from("step/start"),
        ]);
        assert_eq!(
            identity.to_string(),
            "[\"session-format-v2-to-v3\",\"s\\\"\\n\\u001f\u{7f}\u{2028}\",3,\"step/start\"]"
        );
        assert_eq!(
            system_message_id("s\"\n\u{1f}\u{7f}\u{2028}", 3, "step/start"),
            "v2-to-v3-system-ad44e165efaaadc8d38cda6d832e9dbee04374e6584fe9d86eecab7503cc9548"
        );
        assert_eq!(
            system_message_id("s", 0, "step/start"),
            "v2-to-v3-system-4e7efa1596aaff72ef17321e0f38d4e1051cff3cd85e7d991a0a4980462e6fcc"
        );
    }
}
