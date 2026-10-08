//! Development-only fold of a restored parent Session's direct-child catalog,
//! as `subagentCatalogProjectionDefinition` in
//! `packages/subagent/subagent/src/catalog.ts` folds it and its wire view
//! reads it.
//!
//! [`subagent_catalog`] starts from `init(header, inheritedEventCount)` with
//! the restored inherited cut and folds the stored events and then the
//! closers. It skips every event whose type is not `subagent/catalog`, and
//! every catalog event whose seq is below the inherited cut, before any
//! validation, so an inherited catalog fact never counts and a malformed one
//! never refuses. Every other catalog event's data must pass the strict union
//! of the one-shot and continuable payload schemas; the first that fails
//! refuses the whole fold with its seq, where TypeScript throws a `ZodError`,
//! and no partial entries are returned. Admitted entries keep event order.
//! Nothing is deduplicated or sorted: a repeated child id is listed again.
//!
//! A payload is an object with only the schema's keys, so any other key,
//! `__proto__` included, refuses it. `version` is a number equal to 0, `-0`
//! included. `childId` is any string, empty included. `childCreatedAt` is a
//! non-negative safe integer: a finite number with no fractional part whose
//! magnitude is at most 2^53 − 1. `-0` passes, as Zod's `nonnegative` check
//! does, and is kept as `-0`. `mode` is `one-shot`, whose `label` is an
//! optional string, or `continuable`, whose `label` is a required string;
//! JSON `null` is a present value of the wrong type. Duplicate JSON keys keep
//! their last value, as `JSON.parse` does.
//!
//! JSON numbers reach this fold already parsed by the workspace's
//! `float_roundtrip` serde_json, which reads decimal, fraction, and exponent
//! spellings to the same `f64` as `JSON.parse`. Numbers admitted by restoration
//! have an exact answer here; existing scan and restoration limits apply
//! before the fold.
//!
//! Session construction may append a `session/end-seed` after the closers;
//! it is not a catalog event, so it cannot change this view, and it is not
//! folded. This module does not reproduce the TypeScript state's chunked list
//! checkpoint, Zod's issue tree, or any registry or child lifecycle.

use serde_json::{Map, Value};

use crate::RestoredLog;

/// `Number.MAX_SAFE_INTEGER`, exactly representable as `f64`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Both strict union members declare the same keys; only `label`'s
/// optionality differs.
const CATALOG_KEYS: [&str; 5] = ["version", "childId", "childCreatedAt", "mode", "label"];

/// A catalog entry's mode, with its frozen creation label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentCatalogMode {
    /// `label` is `None` when the event omitted it.
    OneShot {
        label: Option<String>,
    },
    Continuable {
        label: String,
    },
}

/// One direct child, as the wire view lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentCatalogEntry {
    pub id: String,
    /// The parsed JavaScript number, `-0` kept. Derived equality treats `-0`
    /// and `0` as equal; compare [`f64::to_bits`] to tell them apart.
    pub created_at: f64,
    pub mode: SubagentCatalogMode,
}

/// The first own catalog event whose data the payload schema rejects, where
/// TypeScript's `apply` throws a `ZodError`. Zod's issues are not reproduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentCatalogRefusal {
    pub seq: u64,
}

/// Fold `subagentCatalogProjectionDefinition` over the restored stored events
/// and then the closers, and return its wire view: the restored log's own
/// catalog entries in event order, or the first own event that fails
/// validation.
pub fn subagent_catalog(
    restored: &RestoredLog,
) -> Result<Vec<SubagentCatalogEntry>, SubagentCatalogRefusal> {
    let inherited_event_count = restored.stored().inherited_event_count();
    let stored = restored.stored().events().map(|event| {
        let envelope = event.envelope();
        (envelope.seq, envelope.event_type, envelope.data)
    });
    let closers = restored.closers().iter().map(|closer| {
        (
            closer["seq"].as_u64().expect("closer seq"),
            closer["type"].as_str().expect("closer type"),
            &closer["data"],
        )
    });
    let mut entries = Vec::new();
    for (seq, event_type, data) in stored.chain(closers) {
        if event_type != "subagent/catalog" || seq < inherited_event_count {
            continue;
        }
        entries.push(catalog_entry(data).ok_or(SubagentCatalogRefusal { seq })?);
    }
    Ok(entries)
}

/// The entry a payload passing the strict union yields; `None` where Zod
/// rejects it.
fn catalog_entry(data: &Value) -> Option<SubagentCatalogEntry> {
    let fields = data.as_object()?;
    let one_shot = match fields.get("mode")?.as_str()? {
        "one-shot" => true,
        "continuable" => false,
        _ => return None,
    };
    if !fields
        .keys()
        .all(|key| CATALOG_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    // Zod's zero literal admits `-0`.
    if fields.get("version")?.as_f64()? != 0.0 {
        return None;
    }
    let id = fields.get("childId")?.as_str()?.to_owned();
    let created_at = fields.get("childCreatedAt")?.as_f64()?;
    if !is_nonnegative_safe_integer(created_at) {
        return None;
    }
    let mode = if one_shot {
        SubagentCatalogMode::OneShot {
            label: optional_string(fields, "label")?.map(str::to_owned),
        }
    } else {
        SubagentCatalogMode::Continuable {
            label: fields.get("label")?.as_str()?.to_owned(),
        }
    };
    Some(SubagentCatalogEntry {
        id,
        created_at,
        mode,
    })
}

/// Zod 4's `int()` and `nonnegative()` checks: a safe integer that is not
/// below 0. `-0` passes.
fn is_nonnegative_safe_integer(value: f64) -> bool {
    value.is_finite() && value.trunc() == value && value.abs() <= MAX_SAFE_INTEGER && value >= 0.0
}

/// An absent field is `Some(None)`; a present non-string, `null` included,
/// is `None`.
fn optional_string<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<Option<&'a str>> {
    match fields.get(key) {
        None => Some(None),
        Some(value) => value.as_str().map(Some),
    }
}
