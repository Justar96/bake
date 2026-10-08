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
//! qualified. Where JavaScript would throw a `TypeError` or compute with
//! coerced or rounded values, [`token_usage`] refuses with a
//! [`UsageLimit`] and claims no TypeScript outcome.

use serde_json::Value;

use crate::{MAX_SAFE_INTEGER, RestoredLog};

/// `TokenUsageProjection`: provider token counts by bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsageBuckets {
    pub uncached_input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// The replacement slot: the coordinate and buckets of the last counted
/// sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastTokenUsage {
    pub turn: u64,
    pub step: u64,
    pub buckets: TokenUsageBuckets,
}

/// `TokenUsageState`: the running totals, whose wire view token-meter
/// publishes, and the replacement slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// A sampled count is not spelled as a safe integer (a fraction, an
    /// exponent, or out of range), or a running total leaves the safe-integer
    /// range, where doubles may round.
    Number,
    /// The sample is not an object, its `inputTokens` or `outputTokens` is not
    /// a number, or a cache count is neither absent, `null`, nor a number.
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
        match state.last {
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
        .filter(|last| last.turn == turn && last.step == step)
        .map(|last| last.buckets);
    if previous == Some(buckets) {
        return Ok(());
    }
    let previous = previous.unwrap_or_default();
    let replace = |total: i64, previous: i64, next: i64| {
        safe(total - previous)
            .and_then(|kept| safe(kept + next))
            .ok_or(UsageLimit::Number)
    };
    let totals = state.totals;
    state.totals = TokenUsageBuckets {
        uncached_input_tokens: replace(
            totals.uncached_input_tokens,
            previous.uncached_input_tokens,
            buckets.uncached_input_tokens,
        )?,
        output_tokens: replace(
            totals.output_tokens,
            previous.output_tokens,
            buckets.output_tokens,
        )?,
        cache_read_tokens: replace(
            totals.cache_read_tokens,
            previous.cache_read_tokens,
            buckets.cache_read_tokens,
        )?,
        cache_write_tokens: replace(
            totals.cache_write_tokens,
            previous.cache_write_tokens,
            buckets.cache_write_tokens,
        )?,
    };
    state.last = Some(LastTokenUsage {
        turn,
        step,
        buckets,
    });
    Ok(())
}

/// `usageOf`: the settlement's sample, `None` when it reports none.
fn sample<'a>(event_type: &str, data: &'a Value) -> Result<Option<&'a Value>, UsageLimit> {
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

/// `bucketsFrom`, refusing every input JavaScript would not sum exactly.
fn buckets(usage: &Value) -> Result<TokenUsageBuckets, UsageLimit> {
    let usage = usage.as_object().ok_or(UsageLimit::Usage)?;
    let count = |key: &str, optional: bool| match usage.get(key) {
        None | Some(Value::Null) if optional => Ok(0),
        Some(Value::Number(number)) => number.as_i64().and_then(safe).ok_or(UsageLimit::Number),
        _ => Err(UsageLimit::Usage),
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

fn safe(value: i64) -> Option<i64> {
    (value.unsigned_abs() <= MAX_SAFE_INTEGER).then_some(value)
}
