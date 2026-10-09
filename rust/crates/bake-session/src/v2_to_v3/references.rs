//! `remapEvent` in `session-format-v2-to-v3/src/references.ts`: only audited
//! same-artifact references move to target seqs. Delivery `throughSeq`,
//! captured generations, workflow seqs, block indices, turn and step
//! coordinates, and numbers inside tool JSON keep their source values.

use serde_json::{Map, Value};

use super::StageError;
use super::js::{count, record};
use crate::json_parse::{Deep, clone_fields, replace_member};

const LIMIT: &str = "reference-float-lexeme";

/// Move `event` from source seq `source_seq` to `target_seq`. `mapping`
/// holds the target seq of every earlier source event, so its length is
/// `source_seq`.
pub(super) fn remap_event(
    event: Map<String, Value>,
    source_seq: u64,
    target_seq: u64,
    mapping: &[u64],
) -> Result<Map<String, Value>, StageError> {
    let mut event = Deep::new(event);
    let remap = Remap {
        source_seq,
        mapping,
    };
    let event_type = event["type"].as_str().unwrap_or_default().to_owned();
    let mut data = Deep::new(clone_fields(record(event.get("data"), &event_type)?));
    match event_type.as_str() {
        "command/done" => {
            if let Some(source) = data.get("sourceEventSeq") {
                let target = remap.one(Some(source))?;
                replace_member(&mut data, "sourceEventSeq", target);
            }
        }
        "compaction/summary" | "compaction/prune" => {
            let range = remap.range(data.get("shadowedRange"))?;
            let seqs = remap.list(data.get("shadowedSeqs"))?;
            replace_member(&mut data, "shadowedRange", range);
            replace_member(&mut data, "shadowedSeqs", seqs);
        }
        "session/title" | "session/title-llm-request" => {
            let seqs = remap.list(data.get("messageSeqs"))?;
            replace_member(&mut data, "messageSeqs", seqs);
        }
        _ => {}
    }
    event.insert("seq".to_owned(), Value::from(target_seq));
    replace_member(&mut event, "data", Value::Object(data.into_inner()));
    if let Some(sources) = event.get("sourceEventSeqs") {
        let sources = remap.list(Some(sources))?;
        replace_member(&mut event, "sourceEventSeqs", sources);
    }
    match event.get("surfaceOp") {
        None => {}
        Some(operation) if operation == "append" => {}
        Some(operation) => {
            let operation = remap.range(Some(operation))?;
            replace_member(&mut event, "surfaceOp", operation);
        }
    }
    Ok(event.into_inner())
}

struct Remap<'a> {
    source_seq: u64,
    mapping: &'a [u64],
}

impl Remap<'_> {
    fn one(&self, value: Option<&Value>) -> Result<Value, StageError> {
        let source = count(value, "source event reference", LIMIT)?;
        match usize::try_from(source)
            .ok()
            .and_then(|index| self.mapping.get(index))
        {
            Some(target) if source < self.source_seq => Ok(Value::from(*target)),
            _ => Err(StageError::Invalid(
                "reference must name an earlier source event".to_owned(),
            )),
        }
    }

    fn list(&self, value: Option<&Value>) -> Result<Value, StageError> {
        let Some(Value::Array(items)) = value else {
            return Err(StageError::Invalid(
                "sequence references must be an array".to_owned(),
            ));
        };
        items
            .iter()
            .map(|item| self.one(Some(item)))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    }

    fn range(&self, value: Option<&Value>) -> Result<Value, StageError> {
        let mut range = Deep::new(clone_fields(record(value, "sequence range")?));
        let start = self.one(range.get("start"))?;
        let end = self.one(range.get("end"))?;
        replace_member(&mut range, "start", start);
        replace_member(&mut range, "end", end);
        Ok(Value::Object(range.into_inner()))
    }
}
