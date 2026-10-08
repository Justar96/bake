//! A port of `AssistantStreamAccumulator.push` and `snapshot` in
//! `packages/llm/llm/src/assistant-stream.ts`: the compaction of one
//! Assistant attempt's timed chunks into durable stream records.
//!
//! Where TypeScript throws a `TypeError` or reaches `assertNever`, `push`
//! returns [`StreamPushError::ChunkShape`]; where it would read a number
//! spelled with a fraction or exponent as a time or an index, it returns
//! [`StreamPushError::FloatLexeme`], since serde_json does not keep the
//! value JavaScript reads.

use serde_json::{Map, Value};

use crate::MAX_SAFE_INTEGER;
use crate::v2_to_v3::contains_negative_zero;

/// Why `push` did not decide a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamPushError {
    /// TypeScript throws: a time or index that is not a safe integer, a
    /// chunk that is not lossless JSON, a member of the wrong kind, or an
    /// unknown chunk type.
    ChunkShape,
    /// A time or index spelled with a fraction or exponent.
    FloatLexeme,
}

/// Which delta a text-like record packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeltaKind {
    Text,
    Reasoning,
}

impl DeltaKind {
    const fn record_type(self) -> &'static str {
        match self {
            Self::Text => "text-chunks",
            Self::Reasoning => "reasoning-chunks",
        }
    }
}

/// `MutableRecord`.
#[derive(Debug)]
enum StreamRecord {
    Deltas {
        kind: DeltaKind,
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        texts: Vec<String>,
        last_time: i64,
    },
    ToolCall {
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        id: String,
        /// `None` when the first chunk had no own `name`.
        name: Option<String>,
        args: Vec<String>,
        last_time: i64,
    },
    Chunk {
        time: i64,
        chunk: Value,
    },
}

/// `AssistantStreamAccumulator`.
#[derive(Debug, Default)]
pub(crate) struct AssistantStreamAccumulator {
    records: Vec<StreamRecord>,
}

impl AssistantStreamAccumulator {
    /// `push({ time, chunk })` with the event's `time` and `data.chunk`,
    /// either absent. Nothing is recorded when it fails.
    pub(crate) fn push(
        &mut self,
        time: Option<&Value>,
        chunk: Option<&Value>,
    ) -> Result<(), StreamPushError> {
        let time = safe_time(time)?;
        // `snapshotChunk`: `undefined` and `-0` are not lossless JSON.
        let chunk = match chunk {
            Some(chunk) if !contains_negative_zero(chunk) => chunk,
            _ => return Err(StreamPushError::ChunkShape),
        };
        // Reading `type` of `null` throws; any other non-object has no own
        // `type`, so it reaches `assertNever`.
        let Value::Object(fields) = chunk else {
            return Err(StreamPushError::ChunkShape);
        };
        let chunk_type = match fields.get("type") {
            Some(Value::String(chunk_type)) => chunk_type.as_str(),
            _ => return Err(StreamPushError::ChunkShape),
        };
        match chunk_type {
            "text-delta" => self.push_delta(DeltaKind::Text, time, fields),
            "reasoning-delta" => self.push_delta(DeltaKind::Reasoning, time, fields),
            "tool-call-delta" => self.push_tool_call(time, chunk, fields),
            "block-start" | "block-end" | "usage" | "finish" => {
                self.records.push(StreamRecord::Chunk {
                    time,
                    chunk: chunk.clone(),
                });
                Ok(())
            }
            _ => Err(StreamPushError::ChunkShape),
        }
    }

    fn push_delta(
        &mut self,
        kind: DeltaKind,
        time: i64,
        fields: &Map<String, Value>,
    ) -> Result<(), StreamPushError> {
        let index = safe_index(fields.get("index"))?;
        let Some(Value::String(text)) = fields.get("text") else {
            return Err(StreamPushError::ChunkShape);
        };
        if let Some(StreamRecord::Deltas {
            kind: previous_kind,
            index: previous_index,
            dt,
            texts,
            last_time,
            ..
        }) = self.records.last_mut()
            && *previous_kind == kind
            && *previous_index == index
            && let Some(gap) = safe_gap(*last_time, time)
        {
            dt.push(gap);
            texts.push(text.clone());
            *last_time = time;
            return Ok(());
        }
        self.records.push(StreamRecord::Deltas {
            kind,
            time0: time,
            index,
            dt: Vec::new(),
            texts: vec![text.clone()],
            last_time: time,
        });
        Ok(())
    }

    fn push_tool_call(
        &mut self,
        time: i64,
        chunk: &Value,
        fields: &Map<String, Value>,
    ) -> Result<(), StreamPushError> {
        let index = safe_index(fields.get("index"))?;
        let Some(Value::String(id)) = fields.get("id") else {
            return Err(StreamPushError::ChunkShape);
        };
        let name = match fields.get("name") {
            None => None,
            Some(Value::String(name)) => Some(name),
            Some(_) => return Err(StreamPushError::ChunkShape),
        };
        let Some(Value::String(argument)) = fields.get("argumentsDelta") else {
            return Err(StreamPushError::ChunkShape);
        };
        // `chunk.id.length === 0`: a UTF-16 length is zero exactly when the string is empty.
        if id.is_empty() || name.is_some_and(String::is_empty) {
            self.records.push(StreamRecord::Chunk {
                time,
                chunk: chunk.clone(),
            });
            return Ok(());
        }
        if let Some(StreamRecord::ToolCall {
            index: previous_index,
            dt,
            id: previous_id,
            name: previous_name,
            args,
            last_time,
            ..
        }) = self.records.last_mut()
            && *previous_index == index
            && previous_id == id
            && previous_name.as_ref() == name
            && let Some(gap) = safe_gap(*last_time, time)
        {
            dt.push(gap);
            args.push(argument.clone());
            *last_time = time;
            return Ok(());
        }
        self.records.push(StreamRecord::ToolCall {
            time0: time,
            index,
            dt: Vec::new(),
            id: id.clone(),
            name: name.cloned(),
            args: vec![argument.clone()],
            last_time: time,
        });
        Ok(())
    }

    /// `snapshot()`: the durable records in TypeScript's member order, each
    /// without its `lastTime`.
    pub(crate) fn snapshot(&self) -> Vec<Value> {
        self.records
            .iter()
            .map(|record| {
                let mut durable = Map::new();
                match record {
                    StreamRecord::Deltas {
                        kind,
                        time0,
                        index,
                        dt,
                        texts,
                        ..
                    } => {
                        durable.insert("type".to_owned(), Value::from(kind.record_type()));
                        durable.insert("time0".to_owned(), Value::from(*time0));
                        durable.insert("index".to_owned(), Value::from(*index));
                        durable.insert("dt".to_owned(), Value::from(dt.clone()));
                        durable.insert("texts".to_owned(), Value::from(texts.clone()));
                    }
                    StreamRecord::ToolCall {
                        time0,
                        index,
                        dt,
                        id,
                        name,
                        args,
                        ..
                    } => {
                        durable.insert("type".to_owned(), Value::from("tool-call-chunks"));
                        durable.insert("time0".to_owned(), Value::from(*time0));
                        durable.insert("index".to_owned(), Value::from(*index));
                        durable.insert("dt".to_owned(), Value::from(dt.clone()));
                        durable.insert("id".to_owned(), Value::from(id.as_str()));
                        if let Some(name) = name {
                            durable.insert("name".to_owned(), Value::from(name.as_str()));
                        }
                        durable.insert("args".to_owned(), Value::from(args.clone()));
                    }
                    StreamRecord::Chunk { time, chunk } => {
                        durable.insert("type".to_owned(), Value::from("chunk"));
                        durable.insert("time".to_owned(), Value::from(*time));
                        durable.insert("chunk".to_owned(), chunk.clone());
                    }
                }
                Value::Object(durable)
            })
            .collect()
    }
}

/// A safe integer spelled without a fraction or exponent.
fn safe_integer(value: Option<&Value>) -> Result<i64, StreamPushError> {
    let Some(Value::Number(number)) = value else {
        return Err(StreamPushError::ChunkShape);
    };
    if let Some(value) = number.as_u64() {
        return match i64::try_from(value) {
            Ok(value) if value.unsigned_abs() <= MAX_SAFE_INTEGER => Ok(value),
            _ => Err(StreamPushError::ChunkShape),
        };
    }
    if let Some(value) = number.as_i64() {
        return if value.unsigned_abs() <= MAX_SAFE_INTEGER {
            Ok(value)
        } else {
            Err(StreamPushError::ChunkShape)
        };
    }
    Err(StreamPushError::FloatLexeme)
}

/// `safeTime`.
fn safe_time(value: Option<&Value>) -> Result<i64, StreamPushError> {
    safe_integer(value)
}

/// `safeIndex`: a non-negative safe integer. `-0` never reaches it, since
/// `snapshotChunk` refuses it first.
fn safe_index(value: Option<&Value>) -> Result<u64, StreamPushError> {
    u64::try_from(safe_integer(value)?).map_err(|_| StreamPushError::ChunkShape)
}

/// `safeGap`: for two safe integers the double difference is safe, and adds
/// back exactly, when the exact difference is safe.
fn safe_gap(previous: i64, next: i64) -> Option<i64> {
    let gap = i128::from(next) - i128::from(previous);
    if gap.unsigned_abs() <= u128::from(MAX_SAFE_INTEGER) {
        i64::try_from(gap).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gaps_beyond_the_safe_range_start_a_new_record() {
        let max = MAX_SAFE_INTEGER as i64;
        assert_eq!(safe_gap(-max, 0), Some(max));
        assert_eq!(safe_gap(-max, 1), None);
        assert_eq!(safe_gap(max, -max), None);
        let mut accumulator = AssistantStreamAccumulator::default();
        let chunk = json!({"type": "text-delta", "index": 0, "text": "a"});
        assert_eq!(accumulator.push(Some(&json!(-max)), Some(&chunk)), Ok(()));
        assert_eq!(accumulator.push(Some(&json!(max)), Some(&chunk)), Ok(()));
        assert_eq!(accumulator.snapshot().len(), 2);
    }
}
