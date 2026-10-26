//! The session's side of cached compaction ([`crate::compaction::cached`]):
//! the agent's last request, captured as the provider sends it, and the
//! summary attempt a compaction makes before Pi's summary.
//!
//! Adapted from pi-cached-compaction 0.2.0
//! (`e1e20de0a7e433070414602f1105cdcac258ef4f`, MIT, Copyright (c) Nico
//! Bailon) `index.ts`: its `before_provider_request` capture and
//! `checkCachedPath`. While `compaction.cachedSummary` is on, the agent's
//! stream function records each request's final payload and options
//! ([`capturing`]); turning it off drops the record. Background compaction
//! keeps Pi's summary.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use bake_agent::{AgentMessage as LiveMessage, StreamFn};
use bake_ai::{AbortSignal, CacheRetention, Message, Model, SimpleStreamOptions};
use serde_json::Value;

use super::SessionEvent;
use super::recovery::{CompactionReason, Core};
use super::{SharedSettings, read_settings};
use crate::compaction::CompactionPreparation;
use crate::compaction::cached::{CachedRequest, CachedSummary, cache_state, summarize};
use crate::session::AgentMessage;

/// Why there is no cached summary before the session sent a request, or
/// when its provider never shows the payload it sends.
pub(super) const NO_REQUEST: &str = "no earlier request to repeat (none was sent since the session opened, or its provider does not expose it)";

/// How the agent converts its messages for a request: Pi's conversion, the
/// declarations code mode hides, and `blockImages`.
pub(super) type RequestMessages = Arc<dyn Fn(&[LiveMessage]) -> Vec<Message> + Send + Sync>;

/// The agent's last request.
#[derive(Clone)]
struct LastRequest {
    provider: String,
    model: String,
    payload: Arc<Value>,
    options: Arc<SimpleStreamOptions>,
}

/// What cached compaction keeps between requests.
#[derive(Default)]
pub(super) struct CachedRequests {
    last: Mutex<Option<LastRequest>>,
    /// Set once, when the agent's conversion is built.
    pub convert: OnceLock<RequestMessages>,
}

impl CachedRequests {
    fn last(&self) -> Option<LastRequest> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, last: Option<LastRequest>) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = last;
    }
}

/// `stream_fn` recording each request's final payload, after any
/// `on_payload` of its own, while `compaction.cachedSummary` is on. The
/// record keeps the request's options without its signal, its key, and the
/// observers of its response, which belong to the agent's request.
pub(super) fn capturing(
    stream_fn: StreamFn,
    settings: SharedSettings,
    requests: Arc<CachedRequests>,
) -> StreamFn {
    Arc::new(move |model, context, mut options| {
        if !read_settings(&settings).cached_compaction() {
            requests.set(None);
            return stream_fn(model, context, options);
        }
        let hook = options.base.on_payload.take();
        let mut kept = options.clone();
        kept.base.signal = None;
        kept.base.api_key = None;
        kept.base.on_response = None;
        kept.base.on_provider_stream_event = None;
        kept.base.on_payload = hook.clone();
        let kept = Arc::new(kept);
        let requests = Arc::clone(&requests);
        options.base.on_payload = Some(Arc::new(move |params, model| {
            let replaced = hook.as_ref().and_then(|hook| hook(params, model));
            requests.set(Some(LastRequest {
                provider: model.provider.clone(),
                model: model.id.clone(),
                payload: Arc::new(replaced.clone().unwrap_or_else(|| params.clone())),
                options: Arc::clone(&kept),
            }));
            replaced
        }));
        stream_fn(model, context, options)
    })
}

impl Core {
    /// pi-cached-compaction's `checkCachedPath` and summary: the
    /// compaction of `preparation` written from the conversation the
    /// provider has cached, or why Pi's summary must run instead. Nothing
    /// is sent for an overflow, which the cached conversation cannot fit,
    /// for a model other than the last request's, or when the last response
    /// read and wrote no cache or the cache has expired.
    pub(super) async fn cached_summary(
        &self,
        preparation: &CompactionPreparation,
        model: &Model,
        custom_instructions: Option<&str>,
        signal: AbortSignal,
        reason: CompactionReason,
    ) -> CachedSummary {
        let fallback = |reason: &str| CachedSummary::Fallback(reason.to_owned());
        if !read_settings(&self.settings).cached_compaction() {
            return CachedSummary::Off;
        }
        if reason == CompactionReason::Overflow {
            return fallback("the last request overflowed the context window");
        }
        let Some(last) = self.cached_requests.last() else {
            return fallback(NO_REQUEST);
        };
        if last.provider != model.provider || last.model != model.id {
            return fallback("the model changed since the last request");
        }
        // A run's model-call limit must leave room for Pi's summary after
        // a cached request that fails.
        let split = preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty();
        let pi_calls = if split && !preparation.messages_to_summarize.is_empty() {
            2
        } else {
            1
        };
        let control = self.control();
        if control
            .as_ref()
            .is_some_and(|control| !control.has_model_call_allowance(1 + pi_calls))
        {
            return fallback("the run's model-call limit leaves no room to try the cache first");
        }
        let Some(convert) = self.cached_requests.convert.get().cloned() else {
            return fallback("the agent's conversion is not ready");
        };
        let long = last.options.base.cache_retention == Some(CacheRetention::Long);
        let first_kept = preparation.first_kept_entry_id.clone();
        let state_model = model.clone();
        let read = self
            .with_session(move |session| {
                let branch: Vec<_> = session.branch_entries(None).into_iter().cloned().collect();
                let cache = cache_state(&state_model, &branch, long);
                let projection = session.build_session_projection();
                let conversation: Vec<LiveMessage> = projection
                    .messages
                    .iter()
                    .map(AgentMessage::to_agent)
                    .collect();
                let kept = projection
                    .entries
                    .iter()
                    .position(|entry| entry.source_entry.id() == first_kept)
                    .map(|start| {
                        projection.entries[start..]
                            .iter()
                            .flat_map(|entry| entry.messages.iter())
                            .map(AgentMessage::to_agent)
                            .collect::<Vec<LiveMessage>>()
                    });
                (cache, conversation, kept)
            })
            .await;
        let (cache, conversation, kept) = match read {
            Ok(read) => read,
            Err(error) => return CachedSummary::Fallback(error),
        };
        if !cache.cached {
            return fallback("the last request did not use the prompt cache");
        }
        if cache
            .expires_at
            .is_some_and(|expires| bake_ai::now_ms() >= expires)
        {
            return fallback("the prompt cache has expired");
        }
        let Some(kept) = kept else {
            return fallback("the first kept entry is not on the branch");
        };
        let conversation = convert(&conversation);
        let kept: Vec<Message> = convert(&kept)
            .into_iter()
            .filter(|message| !matches!(message, Message::System(_)))
            .collect();
        // The kept messages must be the conversation's last ones, as the
        // instruction tells the model.
        let tail: Vec<&Message> = conversation
            .iter()
            .filter(|message| !matches!(message, Message::System(_)))
            .collect();
        let ends_with_kept = tail.len() >= kept.len()
            && tail[tail.len() - kept.len()..]
                .iter()
                .zip(&kept)
                .all(|(sent, kept)| *sent == kept);
        if kept.is_empty() || !ends_with_kept {
            return fallback("the kept messages do not end the conversation");
        }
        self.emit(&SessionEvent::CompactionSummary {
            reason,
            cached: true,
            tokens_before: preparation.tokens_before,
            fallback: None,
        });
        summarize(
            CachedRequest {
                payload: last.payload,
                options: (*last.options).clone(),
                conversation,
                kept: kept.len(),
            },
            preparation,
            model,
            custom_instructions,
            &self.stream_fn,
            signal,
            control.as_ref(),
        )
        .await
    }
}
