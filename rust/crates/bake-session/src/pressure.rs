//! Development-only fold of a restored Session's context pressure, as
//! token-meter's `contextPressureProjectionDefinition` in
//! `packages/llm/token-meter/src/usage-projection.ts` folds the same events,
//! with its view.
//!
//! The newest `request/header` or `request/context` names the request route,
//! and the newest `request/context` its context window. Each Assistant
//! settlement's usage sample, read as the token-usage fold reads it, stamps
//! the prompt-side pressure, input plus cache traffic, together with the
//! route, window, and surface total its request saw. The surface total is
//! `foldSurfaceProjection` in `surface-projection.ts`: each surface append
//! adds its message's `estimateMessage` price from `estimate.ts`; a
//! `compaction/summary` or `compaction/prune` arms a shadow-price claim that
//! only the immediately following event can use; a surface replacement
//! consumes a claim naming its exact range and adds its own price less the
//! claim's, folds with no change when no claim is armed, and refuses when the
//! armed claim names another range. A message is priced as logged, without
//! the `image/offload` projection.
//!
//! The fold runs over the stored events and then the closers. The end seed
//! that follows them, appended or stored, expires any armed claim and changes
//! nothing else, so the folded state has no claim.
//!
//! Restoration proved each surface message an object of the right role with
//! an array `content`, its numbers spelled as `JSON.stringify` writes them,
//! its nesting bounded, and a `system/message`'s blocks text or reasoning; it
//! qualified `request/context` numbers the same way and `request/header`
//! routes. Sampled counts are [`JsCount`]s summed with JavaScript's `+`, so
//! a fraction, an unsafe integer, or a string count folds as TypeScript folds
//! it, and the context window is the double TypeScript holds. Where
//! JavaScript would throw a `TypeError`, or a value is a spelling no writer
//! produces, [`context_pressure`] refuses with a [`PressureLimit`] and claims
//! no TypeScript outcome.

use serde_json::{Map, Value};

use crate::js_count::JsCount;
use crate::json_text::json_number_text;
use crate::usage::{sample, sampled_count};
use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// Fixed text density of `estimate.ts`.
const CHARS_PER_TOKEN: u64 = 4;
/// Per-block structural overhead of `estimate.ts`.
const BLOCK_OVERHEAD: u64 = 4;
/// Per-message role framing of `estimate.ts`.
const ROLE_OVERHEAD: u64 = 4;
const SURFACE_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];

/// A request's provider and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRoute {
    pub provider: String,
    pub model: String,
}

/// `ContextPressureState` after the end seed, which leaves no claim.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextPressureState {
    pub context_window: Option<f64>,
    pub sampled_context_window: Option<f64>,
    pub pressure_tokens: Option<JsCount>,
    pub request_route: Option<RequestRoute>,
    pub sampled_route: Option<RequestRoute>,
    /// May be negative: a claim can price more than the surface holds.
    pub surface_tokens: i64,
    pub sampled_surface_tokens: Option<i64>,
}

/// `ContextPressureProjection` as the definition's `wire.view` computes it,
/// before `viewSchema.parse`. TypeScript's `restore` and `viewCheckpoint`
/// parse it with `z.number().int().nonnegative()` and throw on a fractional,
/// string, or `NaN` count rather than publish it; this view keeps the value.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextPressureView {
    pub context_window: Option<f64>,
    pub sampled_context_window: Option<f64>,
    pub request_route: Option<RequestRoute>,
    pub sampled_route: Option<RequestRoute>,
    pub pressure_tokens: Option<JsCount>,
    /// `Math.max(0, pressureTokens + surfaceTokens - sampledSurfaceTokens)`
    /// in doubles; `NaN` when the pressure does not convert to a number.
    pub projected_tokens: Option<f64>,
}

impl ContextPressureState {
    /// The definition's `wire.view`, before `viewSchema.parse` (see
    /// [`ContextPressureView`]).
    pub fn view(&self) -> ContextPressureView {
        ContextPressureView {
            context_window: self.context_window,
            sampled_context_window: self.sampled_context_window,
            request_route: self.request_route.clone(),
            sampled_route: self.sampled_route.clone(),
            pressure_tokens: self.pressure_tokens.clone(),
            projected_tokens: self
                .pressure_tokens
                .as_ref()
                .zip(self.sampled_surface_tokens)
                .map(|(pressure, sampled)| projected(pressure, self.surface_tokens, sampled)),
        }
    }
}

/// `Math.max(0, pressure + surface - sampled)`: `NaN` stays `NaN`, and -0
/// becomes 0.
fn projected(pressure: &JsCount, surface: i64, sampled: i64) -> f64 {
    let sum = pressure.plus(&JsCount::Number(surface as f64)).to_number();
    let difference = sum - sampled as f64;
    if difference.is_nan() {
        f64::NAN
    } else {
        difference.max(0.0) + 0.0
    }
}

/// Why no context pressure was folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureRefusal {
    /// `foldSurfaceProjection` throws a plain `Error` with
    /// [`PressureRefusal::message`]: the surface replacement at `seq` follows
    /// a claim for another range.
    UnclaimedReplace {
        seq: u64,
        start: u64,
        end: u64,
        claim_start: u64,
        claim_end: u64,
    },
    /// Event `seq`, a stored event or a closer, needs JavaScript behavior
    /// this port does not reproduce; nothing is claimed about TypeScript's
    /// outcome.
    NativeSubset { seq: u64, limit: PressureLimit },
}

impl PressureRefusal {
    /// TypeScript's message, for a refusal that claims one.
    pub fn message(&self) -> Option<String> {
        match *self {
            Self::UnclaimedReplace {
                seq,
                start,
                end,
                claim_start,
                claim_end,
            } => Some(format!(
                "token surface: replace at seq {seq} over range {start}-{end} has no adjacent shadow price (armed claim covers {claim_start}-{claim_end})"
            )),
            Self::NativeSubset { .. } => None,
        }
    }
}

/// Input whose fold depends on JavaScript coercion, rounding, or a thrown
/// `TypeError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureLimit {
    /// A sampled count is a number not spelled as `JSON.stringify` writes
    /// its value, a consumed claim's
    /// `shadowedTokenCount` is not a number spelled as a safe integer, or the
    /// surface total leaves the safe-integer range.
    Number,
    /// The sample is not an object, its `inputTokens` is neither a number nor
    /// a string, or a cache count is neither absent, `null`, a number, nor a
    /// string.
    Usage,
    /// The backward stream scan reaches a `null` record, or a `chunk` record
    /// whose `chunk` is absent or `null`, before a `usage` chunk.
    Stream,
    /// A `compaction/summary` or `compaction/prune` has no object `data` or
    /// `shadowedRange`, or a range endpoint is not spelled as a non-negative
    /// safe integer, where `SessionSeq` throws or JavaScript rounds.
    Claim,
    /// A priced content block is not an object, a text or reasoning block's
    /// `text` or a tool call's `name` or `arguments` is not a string, or a
    /// tool result's `content` is not an array.
    Block,
    /// A `request/context` `provider` or `model` is not a string.
    Route,
    /// A `request/context` `contextWindow` is present but not an integer
    /// number spelled as `JSON.stringify` writes it, such as a fraction or a
    /// string, which the writers never log.
    ContextWindow,
}

/// A claim's price as logged: the replacement that consumes it must read a
/// safe integer, and an expired one is never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Claim {
    start: u64,
    end: u64,
    tokens: Option<i64>,
}

#[derive(Default)]
struct Fold {
    state: ContextPressureState,
    claim: Option<Claim>,
}

/// Fold `contextPressureProjectionDefinition` over the restored stored events
/// and then the closers; the end seed only expires the claim.
pub fn context_pressure(restored: &RestoredLog) -> Result<ContextPressureState, PressureRefusal> {
    let mut fold = Fold::default();
    for event in restored.stored().events() {
        let envelope = event.envelope();
        fold.apply(
            envelope.seq,
            envelope.event_type,
            envelope.data,
            envelope.surface_op,
        )?;
    }
    for closer in restored.closers() {
        fold.apply(
            closer["seq"].as_u64().expect("closer seq"),
            closer["type"].as_str().expect("closer type"),
            &closer["data"],
            closer.get("surfaceOp"),
        )?;
    }
    Ok(fold.state)
}

impl Fold {
    fn apply(
        &mut self,
        seq: u64,
        event_type: &str,
        data: &Value,
        surface_op: Option<&Value>,
    ) -> Result<(), PressureRefusal> {
        let native = |limit| PressureRefusal::NativeSubset { seq, limit };
        let (delta, claim) = surface_tokens(self.claim, seq, event_type, data, surface_op)
            .map_err(|refusal| match refusal {
                SurfaceRefusal::Limit(limit) => native(limit),
                SurfaceRefusal::Replace(refusal) => refusal,
            })?;
        let state = &mut self.state;
        match event_type {
            "request/header" => {
                // Restoration proved both non-empty strings.
                let config = &data["header"]["config"];
                state.request_route = Some(RequestRoute {
                    provider: config["provider"]
                        .as_str()
                        .expect("header provider")
                        .to_owned(),
                    model: config["model"].as_str().expect("header model").to_owned(),
                });
            }
            "request/context" => {
                let route = |key: &str| data[key].as_str().map(str::to_owned);
                let (Some(provider), Some(model)) = (route("provider"), route("model")) else {
                    return Err(native(PressureLimit::Route));
                };
                state.request_route = Some(RequestRoute { provider, model });
                state.context_window = match data.get("contextWindow") {
                    None => None,
                    Some(Value::Number(window)) => Some(
                        JsCount::from_writer_number(window)
                            .map(|window| window.to_number())
                            .filter(|window| window.fract() == 0.0)
                            .ok_or(native(PressureLimit::ContextWindow))?,
                    ),
                    Some(_) => return Err(native(PressureLimit::ContextWindow)),
                };
            }
            "assistant/message" | "assistant/attempt" => {
                if let Some(usage) =
                    sample(event_type, data).map_err(|_| native(PressureLimit::Stream))?
                {
                    // Restamping with equal values leaves the state as the
                    // definition's unchanged branch does.
                    state.pressure_tokens = Some(pressure(usage).map_err(native)?);
                    state.sampled_surface_tokens = Some(state.surface_tokens);
                    state.sampled_context_window = state.context_window;
                    if state.request_route.is_some() {
                        state.sampled_route.clone_from(&state.request_route);
                    }
                }
            }
            _ => {}
        }
        state.surface_tokens =
            safe(state.surface_tokens + delta).ok_or(native(PressureLimit::Number))?;
        self.claim = claim;
        Ok(())
    }
}

enum SurfaceRefusal {
    Limit(PressureLimit),
    Replace(PressureRefusal),
}

impl From<PressureLimit> for SurfaceRefusal {
    fn from(limit: PressureLimit) -> Self {
        Self::Limit(limit)
    }
}

/// `foldSurfaceProjection`: the event's signed change to the surface total
/// and the claim it leaves for the next event.
fn surface_tokens(
    claim: Option<Claim>,
    seq: u64,
    event_type: &str,
    data: &Value,
    surface_op: Option<&Value>,
) -> Result<(i64, Option<Claim>), SurfaceRefusal> {
    if event_type == "compaction/summary" || event_type == "compaction/prune" {
        let range = data
            .get("shadowedRange")
            .and_then(Value::as_object)
            .ok_or(PressureLimit::Claim)?;
        let endpoint = |key: &str| {
            range
                .get(key)
                .and_then(Value::as_u64)
                .filter(|seq| *seq <= MAX_SAFE_INTEGER)
                .ok_or(PressureLimit::Claim)
        };
        let claim = Claim {
            start: endpoint("start")?,
            end: endpoint("end")?,
            tokens: data
                .get("shadowedTokenCount")
                .and_then(Value::as_i64)
                .and_then(safe),
        };
        return Ok((0, Some(claim)));
    }
    let Some(op) = surface_op.filter(|_| SURFACE_TYPES.contains(&event_type)) else {
        return Ok((0, None));
    };
    let tokens = i64::try_from(message_tokens(event_type, data)?)
        .ok()
        .and_then(safe)
        .ok_or(PressureLimit::Number)?;
    // The codec proved the marker `"append"` or an exact replacement with
    // safe endpoints.
    let Value::Object(replace) = op else {
        return Ok((tokens, None));
    };
    let Some(claim) = claim else {
        return Ok((0, None));
    };
    let endpoint = |key: &str| replace[key].as_u64().expect("codec-proved endpoint");
    let (start, end) = (endpoint("startSeq"), endpoint("endSeq"));
    if claim.start != start || claim.end != end {
        return Err(SurfaceRefusal::Replace(PressureRefusal::UnclaimedReplace {
            seq,
            start,
            end,
            claim_start: claim.start,
            claim_end: claim.end,
        }));
    }
    let claimed = claim.tokens.ok_or(PressureLimit::Number)?;
    let delta = safe(tokens - claimed).ok_or(PressureLimit::Number)?;
    Ok((delta, None))
}

/// `estimateMessage` of `deriveEventMessage`, 0 when it derives none.
fn message_tokens(event_type: &str, data: &Value) -> Result<u64, PressureLimit> {
    let message = match event_type {
        "user/message" => data,
        _ => &data["message"],
    };
    let content = message["content"]
        .as_array()
        .expect("restored message content");
    match event_type {
        "system/message" | "assistant/message" if content.is_empty() => Ok(0),
        "system/message" => {
            let mut characters = 0;
            for block in content {
                characters += match (block["type"].as_str(), &block["text"]) {
                    (Some("text"), Value::String(text)) => utf16_len(text),
                    (Some("text"), _) => return Err(PressureLimit::Block),
                    _ => stringified_len(block)?,
                };
            }
            Ok(characters.div_ceil(CHARS_PER_TOKEN) + ROLE_OVERHEAD)
        }
        _ => Ok(content_tokens(content)? + ROLE_OVERHEAD),
    }
}

/// `estimateContent`. Nested `tool-result` content is summed from an explicit
/// stack, so any nesting is safe; every failure is the same limit, so the
/// order blocks are read in decides nothing.
fn content_tokens(blocks: &[Value]) -> Result<u64, PressureLimit> {
    let mut tokens = 0;
    let mut pending = vec![blocks];
    while let Some(blocks) = pending.pop() {
        for block in blocks {
            let block = block.as_object().ok_or(PressureLimit::Block)?;
            let text = |key: &str| match block.get(key) {
                Some(Value::String(text)) => Ok(utf16_len(text).div_ceil(CHARS_PER_TOKEN)),
                _ => Err(PressureLimit::Block),
            };
            tokens += match block.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => text("text")? + BLOCK_OVERHEAD,
                Some("tool-call") => text("name")? + text("arguments")? + BLOCK_OVERHEAD,
                Some("tool-result") => match block.get("content") {
                    Some(Value::Array(content)) => {
                        pending.push(content);
                        BLOCK_OVERHEAD
                    }
                    _ => return Err(PressureLimit::Block),
                },
                kind => {
                    // `estimateStructuralBlock` drops an image's `offloaded` mark.
                    let skipped = (kind == Some("image")).then_some("offloaded");
                    BLOCK_OVERHEAD + object_len_without(block, skipped)?.div_ceil(CHARS_PER_TOKEN)
                }
            };
        }
    }
    Ok(tokens)
}

/// `pressureFrom`: `input + (read ?? 0) + (write ?? 0)` with JavaScript's
/// `+`.
fn pressure(usage: &Value) -> Result<JsCount, PressureLimit> {
    let usage = usage.as_object().ok_or(PressureLimit::Usage)?;
    let count = |key: &str, optional: bool| {
        sampled_count(usage.get(key), optional).map_err(|refusal| match refusal {
            None => PressureLimit::Number,
            Some(_) => PressureLimit::Usage,
        })
    };
    let input = count("inputTokens", false)?;
    let read = count("cacheReadTokens", true)?;
    let write = count("cacheWriteTokens", true)?;
    Ok(input.plus(&read).plus(&write))
}

use crate::js_string::{quoted_len, utf16_len};

/// `JSON.stringify(value).length`. Member order does not change the length,
/// and a number's text is ASCII.
fn stringified_len(value: &Value) -> Result<u64, PressureLimit> {
    Ok(values_len(vec![value]))
}

/// `JSON.stringify(fields).length` without the member `skipped`, as if it
/// were removed.
fn object_len_without(
    fields: &Map<String, Value>,
    skipped: Option<&str>,
) -> Result<u64, PressureLimit> {
    let members = fields
        .iter()
        .filter(|(key, _)| Some(key.as_str()) != skipped);
    let mut length = 2;
    let mut pending = Vec::new();
    for (key, value) in members {
        length += quoted_len(key) + 1 + u64::from(!pending.is_empty());
        pending.push(value);
    }
    Ok(length + values_len(pending))
}

/// The summed `JSON.stringify` lengths of `pending`, walked from an explicit
/// stack so any depth the parser produced is safe.
fn values_len(mut pending: Vec<&Value>) -> u64 {
    let mut length = 0;
    while let Some(value) = pending.pop() {
        length += match value {
            Value::Null => 4,
            Value::Bool(flag) => 4 + u64::from(!*flag),
            Value::Number(number) => {
                json_number_text(number.as_f64().unwrap_or(f64::NAN)).len() as u64
            }
            Value::String(text) => quoted_len(text),
            Value::Array(items) => {
                pending.extend(items);
                2 + items.len().saturating_sub(1) as u64
            }
            Value::Object(fields) => {
                pending.extend(fields.values());
                2 + fields.len().saturating_sub(1) as u64
                    + fields.keys().map(|key| quoted_len(key) + 1).sum::<u64>()
            }
        };
    }
    length
}

fn safe(value: i64) -> Option<i64> {
    (value.unsigned_abs() <= MAX_SAFE_INTEGER).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_text;

    #[test]
    fn a_number_prices_its_javascript_text() {
        let value: Value =
            serde_json::from_str(r#"{"a":[0.7,1e21,1.5e-7,-5e-324,9007199254740994,0]}"#)
                .expect("JSON");
        let text = json_text(&value);
        assert_eq!(
            text,
            r#"{"a":[0.7,1e+21,1.5e-7,-5e-324,9007199254740994,0]}"#
        );
        assert_eq!(stringified_len(&value), Ok(utf16_len(&text)));
    }
}
