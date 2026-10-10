//! Provider-neutral model streaming for Bake's Rust agent.
//!
//! `bake-ai` ports the core of Pi's `ai` package (`packages/ai`, release
//! v1.1.0, revision `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the
//! crate's `NOTICE`): the message and event types sessions store, an event
//! stream with a final result, lenient JSON, retry and overflow
//! classification, transcript transforms, a scripted faux provider, and the
//! three wire protocols the CLIProxyAPI route uses.
//!
//! | Module | Pi source (`packages/ai/src/`) |
//! |---|---|
//! | [`types`] | `types.ts`, `utils/diagnostics.ts` |
//! | [`options`] | `types.ts` (`StreamOptions`, `SimpleStreamOptions`), `utils/provider-env.ts` |
//! | [`models`] | `models.ts` (`calculateCost`, thinking-level clamping) |
//! | [`utils::event_stream`] | `utils/event-stream.ts` |
//! | [`utils::abort`] | `utils/abort.ts` and the web `AbortSignal` |
//! | [`utils::json_parse`] | `utils/json-parse.ts` and the `partial-json` parser |
//! | [`utils::sanitize_unicode`] | `utils/sanitize-unicode.ts` |
//! | [`utils::overflow`] | `utils/overflow.ts` |
//! | [`utils::retry`] | `utils/retry.ts` |
//! | [`utils::provider_retry`] | `utils/provider-retry.ts` |
//! | [`utils::error_body`] | `utils/error-body.ts` and the SDKs' `APIError` messages |
//! | [`utils::hash`] | `utils/hash.ts` |
//! | [`utils::text`] | `utils/text.ts` |
//! | [`utils::estimate`] | `utils/estimate.ts` |
//! | [`utils::sse`] | the SSE decoder in `api/anthropic-messages.ts` |
//! | [`transcript`] | `utils/transcript.ts` |
//! | [`api::transform_messages`] | `api/transform-messages.ts` |
//! | [`api::simple_options`] | `api/simple-options.ts` |
//! | [`api::constrained_sampling`] | `api/constrained-sampling.ts` (JSON-schema strict mode) |
//! | [`api::openai_completions`] | `api/openai-completions.ts` |
//! | [`api::openai_responses`] | `api/openai-responses.ts` |
//! | [`api::openai_responses_shared`] | `api/openai-responses-shared.ts` |
//! | [`api::anthropic_messages`] | `api/anthropic-messages.ts` |
//! | [`providers::faux`] | `providers/faux.ts` |
//! | [`stream`] | `compat.ts` (`stream`, `complete`, the API registry) |
//! | [`http`] | the request handling of the `openai` and `@anthropic-ai/sdk` clients |
//!
//! Every provider returns an [`AssistantMessageEventStream`] at once and runs
//! the request on a Tokio task; failures arrive as an `error` event, never as
//! a panic. Calls must be made inside a Tokio runtime; outside one the stream
//! ends with an error event.

pub mod api;
pub mod http;
pub mod models;
pub mod options;
pub mod providers;
pub mod stream;
pub mod types;
pub mod utils;

#[cfg(test)]
mod test_server;

pub use utils::transcript;

pub use options::{ProviderHeaders, SimpleStreamOptions, StreamOptions};
pub use stream::{ApiProvider, ApiRegistry, RegistryError};
pub use types::*;
pub use utils::abort::{AbortController, AbortSignal};
pub use utils::event_stream::{
    AssistantMessageEventSender, AssistantMessageEventStream, EventSender, EventStream,
    assistant_message_channel, event_channel,
};

/// Unix time in milliseconds, Pi's `Date.now()`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// A pseudo-random number in `[0, 1)`, Pi's `Math.random()` for jitter and
/// faux chunk sizes. Not for security.
pub fn random_f64() -> f64 {
    use std::cell::Cell;
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(RandomState::new().hash_one(now_ms()) | 1);
    }
    STATE.with(|state| {
        // xorshift64*
        let mut x = state.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        state.set(x);
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    })
}
