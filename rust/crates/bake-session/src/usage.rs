//! Development-only fold of a restored Session's provider-reported token
//! usage, as token-meter's `tokenUsageProjectionDefinition` in
//! `packages/llm/token-meter/src/usage-projection.ts` folds the same events.
//!
//! Each Assistant settlement, `assistant/message` or `assistant/attempt`,
//! contributes one usage sample: an `assistant/message`'s own `usage` member
//! when it has one, otherwise the stream's last raw `usage` chunk, found as
//! `lastAssistantStreamChunk` in `packages/llm/llm/src/assistant-stream.ts`
//! finds it, scanning backwards and stopping at the first hit, even one
//! without a `usage` member. A sample for the coordinate the slot already
//! holds replaces it in the totals; `llm/retry-started` for that coordinate
//! empties the slot, so the retried attempt adds instead.
//!
//! Restoration proved each settlement's `turn` and `step` safe counts and its
//! `stream` an array, and qualified every number in an `assistant/message`;
//! an `assistant/attempt` stream and `llm/retry-started` data are not
//! qualified. Counts are [`JsCount`]s summed with JavaScript's `+` and `-`,
//! so a fraction, an unsafe integer, or a string count folds as TypeScript
//! folds it. Where JavaScript would throw a `TypeError`, or a count is a
//! number spelling no writer produces, [`token_usage`] refuses with a
//! [`UsageLimit`] and claims no TypeScript outcome.

use serde_json::Value;

use crate::RestoredLog;
use crate::js_count::JsCount;

/// `TokenUsageProjection`: provider token counts by bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenUsageBuckets {
    pub uncached_input_tokens: JsCount,
    pub output_tokens: JsCount,
    pub cache_read_tokens: JsCount,
    pub cache_write_tokens: JsCount,
}

impl Default for TokenUsageBuckets {
    fn default() -> Self {
        Self {
            uncached_input_tokens: JsCount::Number(0.0),
            output_tokens: JsCount::Number(0.0),
            cache_read_tokens: JsCount::Number(0.0),
            cache_write_tokens: JsCount::Number(0.0),
        }
    }
}

/// The replacement slot: the coordinate and buckets of the last counted
/// sample.
#[derive(Debug, Clone, PartialEq)]
pub struct LastTokenUsage {
    pub turn: u64,
    pub step: u64,
    pub buckets: TokenUsageBuckets,
}

/// `TokenUsageState`: the running totals, whose wire view token-meter
/// publishes, and the replacement slot. The totals are the state before
/// `viewSchema.parse`, which in TypeScript throws on a fractional, string, or
/// `NaN` total rather than publish it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenUsageState {
    pub totals: TokenUsageBuckets,
    pub last: Option<LastTokenUsage>,
}

/// Event `seq`, a stored event or a closer, needs JavaScript behavior this
/// port does not reproduce; nothing is claimed about TypeScript's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageRefusal {
    pub seq: u64,
    pub limit: UsageLimit,
}

/// Input whose fold depends on JavaScript coercion, rounding, or a thrown
/// `TypeError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLimit {
    /// A sampled count is a number not spelled as `JSON.stringify` writes
    /// its value, such as `1e3`, `1.0`, or `-0`.
    Number,
    /// The sample is not an object, its `inputTokens` or `outputTokens` is
    /// neither a number nor a string, or a cache count is neither absent,
    /// `null`, a number, nor a string.
    Usage,
    /// The backward stream scan reaches a `null` record, or a `chunk` record
    /// whose `chunk` is absent or `null`, before a `usage` chunk.
    Stream,
    /// `llm/retry-started` data is `null`, or has no `turn` member while the
    /// slot is empty, where the fold reads `step` from the empty slot.
    Retry,
}

/// Fold `tokenUsageProjectionDefinition` over the restored stored events and
/// then the closers. The appended end seed carries no usage.
pub fn token_usage(restored: &RestoredLog) -> Result<TokenUsageState, UsageRefusal> {
    let mut state = TokenUsageState::default();
    for event in restored.stored().events() {
        let envelope = event.envelope();
        apply(&mut state, envelope.event_type, envelope.data).map_err(|limit| UsageRefusal {
            seq: envelope.seq,
            limit,
        })?;
    }
    for closer in restored.closers() {
        let event_type = closer["type"].as_str().expect("closer type");
        apply(&mut state, event_type, &closer["data"]).map_err(|limit| UsageRefusal {
            seq: closer["seq"].as_u64().expect("closer seq"),
            limit,
        })?;
    }
    Ok(state)
}

fn apply(state: &mut TokenUsageState, event_type: &str, data: &Value) -> Result<(), UsageLimit> {
    if event_type == "llm/retry-started" {
        if data.is_null() {
            return Err(UsageLimit::Retry);
        }
        let member = |key| data.as_object().and_then(|fields| fields.get(key));
        match &state.last {
            // `undefined === data.turn` holds only for an absent member.
            None if member("turn").is_none() => return Err(UsageLimit::Retry),
            Some(last)
                if strictly_equal(member("turn"), last.turn)
                    && strictly_equal(member("step"), last.step) =>
            {
                state.last = None;
            }
            _ => {}
        }
        return Ok(());
    }
    if event_type != "assistant/message" && event_type != "assistant/attempt" {
        return Ok(());
    }
    let Some(sample) = sample(event_type, data)? else {
        return Ok(());
    };
    let buckets = buckets(sample)?;
    let coordinate = |key: &str| data[key].as_u64().expect("restored settlement coordinate");
    let (turn, step) = (coordinate("turn"), coordinate("step"));
    let previous = state
        .last
        .as_ref()
        .filter(|last| last.turn == turn && last.step == step)
        .map(|last| last.buckets.clone());
    if previous.as_ref() == Some(&buckets) {
        return Ok(());
    }
    let previous = previous.unwrap_or_default();
    // `addReplacing`: `total - previous + next`.
    let replace = |total: &JsCount, previous: &JsCount, next: &JsCount| {
        JsCount::Number(total.to_number() - previous.to_number()).plus(next)
    };
    let totals = &state.totals;
    state.totals = TokenUsageBuckets {
        uncached_input_tokens: replace(
            &totals.uncached_input_tokens,
            &previous.uncached_input_tokens,
            &buckets.uncached_input_tokens,
        ),
        output_tokens: replace(
            &totals.output_tokens,
            &previous.output_tokens,
            &buckets.output_tokens,
        ),
        cache_read_tokens: replace(
            &totals.cache_read_tokens,
            &previous.cache_read_tokens,
            &buckets.cache_read_tokens,
        ),
        cache_write_tokens: replace(
            &totals.cache_write_tokens,
            &previous.cache_write_tokens,
            &buckets.cache_write_tokens,
        ),
    };
    state.last = Some(LastTokenUsage {
        turn,
        step,
        buckets,
    });
    Ok(())
}

/// `usageOf`: the settlement's sample, `None` when it reports none.
pub(crate) fn sample<'a>(
    event_type: &str,
    data: &'a Value,
) -> Result<Option<&'a Value>, UsageLimit> {
    if event_type == "assistant/message"
        && let Some(usage) = data.get("usage")
    {
        return Ok(Some(usage));
    }
    let stream = data["stream"]
        .as_array()
        .expect("restored settlement stream");
    for record in stream.iter().rev() {
        if record.is_null() {
            return Err(UsageLimit::Stream);
        }
        if record.get("type").and_then(Value::as_str) != Some("chunk") {
            continue;
        }
        let chunk = match record.get("chunk") {
            None | Some(Value::Null) => return Err(UsageLimit::Stream),
            Some(chunk) => chunk,
        };
        if chunk.get("type").and_then(Value::as_str) == Some("usage") {
            return Ok(chunk.get("usage"));
        }
    }
    Ok(None)
}

/// `bucketsFrom`, with `?? 0` for the cache counts.
fn buckets(usage: &Value) -> Result<TokenUsageBuckets, UsageLimit> {
    let usage = usage.as_object().ok_or(UsageLimit::Usage)?;
    let count = |key: &str, optional: bool| {
        sampled_count(usage.get(key), optional)
            .map_err(|refusal| refusal.unwrap_or(UsageLimit::Number))
    };
    Ok(TokenUsageBuckets {
        uncached_input_tokens: count("inputTokens", false)?,
        output_tokens: count("outputTokens", false)?,
        cache_read_tokens: count("cacheReadTokens", true)?,
        cache_write_tokens: count("cacheWriteTokens", true)?,
    })
}

/// JavaScript `===` between a member and a safe count: only a number equal
/// to it, as `JSON.parse` would read it, which `float_roundtrip` parsing
/// matches for every finite lexeme.
fn strictly_equal(member: Option<&Value>, count: u64) -> bool {
    member.and_then(Value::as_f64) == Some(count as f64)
}

/// One sampled count as the fold reads it: an absent or `null` optional
/// count is 0, a number spelled as a writer spells it is its double, and a
/// string is kept. `Err(None)` for any other number spelling;
/// `Err(Some(UsageLimit::Usage))` for any other value.
pub(crate) fn sampled_count(
    value: Option<&Value>,
    optional: bool,
) -> Result<JsCount, Option<UsageLimit>> {
    match value {
        None | Some(Value::Null) if optional => Ok(JsCount::Number(0.0)),
        Some(Value::Number(number)) => JsCount::from_writer_number(number).ok_or(None),
        Some(Value::String(text)) => Ok(JsCount::String(text.clone())),
        _ => Err(Some(UsageLimit::Usage)),
    }
}
