//! Cached compaction in a session ([`super::super::cached_compaction`]),
//! over a provider that builds a payload and passes it through
//! `on_payload` before it sends, as the HTTP adapters do, then answers as
//! the faux provider, whose usage reports the prompt cache per session.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use bake_ai::providers::faux::FauxProviderState;
use bake_ai::{
    ApiProvider, AssistantMessageEventStream, CacheRetention, Message, SimpleStreamOptions,
    StreamOptions, assistant_message_channel,
};

use super::*;
use crate::compaction::utils::SUMMARIZATION_SYSTEM_PROMPT;

/// Wraps the faux provider: the payload is `{ tag, model, messages }`, and
/// a request whose signal is aborted once `on_payload` returns is not sent.
struct PayloadFaux {
    faux: Arc<FauxProvider>,
    /// Changing it changes every later payload, as a changed request
    /// option would.
    tag: Arc<AtomicU64>,
    sent: Arc<Mutex<Vec<Value>>>,
}

impl ApiProvider for PayloadFaux {
    fn api(&self) -> &str {
        self.faux.api()
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.stream_simple(model, context, options.into())
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let built = json!({
            "tag": self.tag.load(AtomicOrdering::SeqCst),
            "model": model.id,
            "messages": context
                .messages()
                .iter()
                .map(|message| serde_json::to_value(message).unwrap_or(Value::Null))
                .collect::<Vec<_>>(),
        });
        let payload = match &options.base.on_payload {
            Some(hook) => hook(&built, model).unwrap_or(built),
            None => built,
        };
        if options
            .base
            .signal
            .as_ref()
            .is_some_and(bake_ai::AbortSignal::aborted)
        {
            let (sender, stream) = assistant_message_channel();
            let mut aborted = AssistantMessage::pending(model);
            aborted.stop_reason = StopReason::Aborted;
            sender.finish(aborted);
            return stream;
        }
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(payload);
        self.faux.stream_simple(model, context, options)
    }
}

struct Cached {
    h: Harness,
    tag: Arc<AtomicU64>,
    sent: Arc<Mutex<Vec<Value>>>,
}

fn cached_harness() -> Cached {
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let tag = Arc::new(AtomicU64::new(0));
    let sent = Arc::default();
    let apis = Arc::new(ApiRegistry::new());
    apis.register(Arc::new(PayloadFaux {
        faux: Arc::clone(&faux),
        tag: Arc::clone(&tag),
        sent: Arc::clone(&sent),
    }));
    let mut registry = ModelRegistry::new(
        ModelConfig::from_value(json!({}), "models.json"),
        AuthStorage::empty(),
    );
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [{ "id": "faux-1", "contextWindow": 200_000, "maxTokens": 4096 }],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    Cached {
        h: Harness {
            faux,
            registry: Arc::new(registry),
            apis,
        },
        tag,
        sent,
    }
}

fn cached_on() -> Settings {
    settings(json!({ "compaction": { "keepRecentTokens": 1, "cachedSummary": true } }))
}

/// What a summary request carried: its system text, the text of its last
/// message, and its cache retention.
type Seen = Arc<Mutex<Vec<(String, String, Option<CacheRetention>)>>>;

fn answer(text: &'static str, seen: &Seen) -> FauxResponseStep {
    let seen = Arc::clone(seen);
    FauxResponseStep::from_fn(
        move |context: &TranscriptContext,
              options: &SimpleStreamOptions,
              _: FauxProviderState,
              _: &Model| {
            let system = context
                .messages()
                .iter()
                .find_map(|message| match message {
                    Message::System(system) => {
                        Some(bake_ai::utils::text::get_system_message_text(system))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let last = match context.messages().last() {
                Some(Message::User(user)) => {
                    bake_ai::utils::text::user_content_text(&user.content, "\n")
                }
                _ => String::new(),
            };
            seen.lock().unwrap_or_else(PoisonError::into_inner).push((
                system,
                last,
                options.base.cache_retention,
            ));
            Ok(faux_assistant_message(text))
        },
    )
}

async fn two_turns(cached: &Cached, settings: Settings) -> AgentSession {
    cached.h.faux.set_responses(vec![
        faux_assistant_message("Echo: one.").into(),
        faux_assistant_message("Echo: two.").into(),
    ]);
    let session = AgentSession::create(options(&cached.h, in_memory(), settings))
        .await
        .expect("session");
    for text in ["one", "two"] {
        session.prompt(text, Vec::new()).await.expect("prompt");
    }
    session
}

fn sent(cached: &Cached) -> Vec<Value> {
    cached
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

// By default, the summary repeats the agent's last request with one
// instruction, keeps its cache, and is the only summary request, a split
// turn included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_summarizes_from_the_cached_conversation() {
    let cached = cached_harness();
    let session = two_turns(&cached, keep_one()).await;
    let (events, _subscription) = record(&session);
    let seen: Seen = Arc::default();
    cached
        .h
        .faux
        .set_responses(vec![answer("## Goal\nCached summary", &seen)]);
    let result = session.compact(Some("the echo")).await.expect("compacted");
    assert_eq!(result.summary, "## Goal\nCached summary");
    assert_eq!(result.path, crate::compaction::SummaryPath::Cached);
    let summary = of_type(&events, &["compaction_summary"]);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0]["path"], "cached");
    assert_eq!(summary[0]["reason"], "manual");
    assert_eq!(summary[0]["tokensBefore"], json!(result.tokens_before));
    assert!(result.usage.is_some_and(|usage| usage.cache_read > 0));
    let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen.len(), 1, "one request, no turn-prefix summary");
    let (system, instruction, retention) = &seen[0];
    assert_ne!(system, SUMMARIZATION_SYSTEM_PROMPT);
    assert!(instruction.contains("<kept-start>\nEcho: two.\n</kept-start>"));
    assert!(instruction.ends_with("Additional focus: the echo"));
    assert_ne!(*retention, Some(CacheRetention::None));
    // Two turns and the summary were sent; the summary repeats the second
    // turn's messages and adds the reply and the instruction.
    let sent = sent(&cached);
    assert_eq!(sent.len(), 3);
    let messages = |payload: &Value| payload["messages"].as_array().cloned().unwrap_or_default();
    let (turn, summary) = (messages(&sent[1]), messages(&sent[2]));
    assert_eq!(summary.len(), turn.len() + 2);
    assert_eq!(summary[..turn.len()], turn[..]);
    let compactions = lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| entry.entry_type() == "compaction")
        .count();
    assert_eq!(compactions, 1);
    session.shutdown().await;
}

// A request that would not repeat the agent's is not sent, and Pi's
// summary runs, here as two requests for the split turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_request_falls_back_to_pi_s_summary() {
    let cached = cached_harness();
    let session = two_turns(&cached, cached_on()).await;
    cached.tag.store(1, AtomicOrdering::SeqCst);
    let seen: Seen = Arc::default();
    cached.h.faux.set_responses(vec![
        answer("## Goal\nPi summary", &seen),
        answer("## Original Request\ntwo", &seen),
    ]);
    let (events, _subscription) = record(&session);
    let result = session.compact(None).await.expect("compacted");
    assert!(result.summary.starts_with("## Goal\nPi summary\n\n---\n\n"));
    let why = "request field \"tag\" differs";
    assert_eq!(
        result.path,
        crate::compaction::SummaryPath::Fallback(why.into())
    );
    let summary = of_type(&events, &["compaction_summary"]);
    assert_eq!(
        summary
            .iter()
            .map(|event| (event["path"].clone(), event["fallbackReason"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("cached"), Value::Null),
            (json!("standard"), json!(why)),
        ],
        "the cached request was built and refused before Pi's ran"
    );
    let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen.len(), 2);
    for (system, _, retention) in &seen {
        assert_eq!(system, SUMMARIZATION_SYSTEM_PROMPT);
        assert_eq!(*retention, Some(CacheRetention::None));
    }
    // The refused cached request never reached the provider.
    assert_eq!(sent(&cached).len(), 4);
    session.shutdown().await;
}

// With the setting off nothing is captured and Pi's summary runs; turning
// it on takes effect from the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_setting_applies_from_the_next_request() {
    let cached = cached_harness();
    let off = settings(json!({ "compaction": { "keepRecentTokens": 1, "cachedSummary": false } }));
    let session = two_turns(&cached, off).await;
    assert!(!session.settings().cached_compaction());
    assert!(session.core.cached_requests.convert.get().is_some());
    session.set_cached_compaction(true).await.expect("set");
    assert!(session.settings().cached_compaction());
    let seen: Seen = Arc::default();
    cached.h.faux.set_responses(vec![
        answer("## Goal\nPi summary", &seen),
        answer("## Original Request\ntwo", &seen),
    ]);
    let result = session.compact(None).await.expect("compacted");
    assert!(matches!(
        result.path,
        crate::compaction::SummaryPath::Fallback(why) if why == super::super::cached_compaction::NO_REQUEST
    ));
    let summary = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(
        summary
            .iter()
            .all(|(system, _, _)| system == SUMMARIZATION_SYSTEM_PROMPT),
        "nothing was captured before the setting was on"
    );
    session.shutdown().await;
}

// An overflow never reads the cache: the conversation does not fit; nor
// does a run without the model calls to fall back after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overflow_or_a_tight_call_limit_keeps_pi_s_summary() {
    let cached = cached_harness();
    let session = two_turns(&cached, cached_on()).await;
    let preparation = {
        let branch: Vec<SessionEntry> = lock(&session.session)
            .branch_entries(None)
            .into_iter()
            .cloned()
            .collect();
        crate::compaction::prepare_compaction(
            &branch,
            session
                .settings()
                .compaction_settings(None)
                .expect("settings"),
        )
        .expect("preparation")
    };
    let model = session.model().expect("model");
    let outcome = session
        .core
        .cached_summary(
            &preparation,
            &model,
            None,
            AbortController::new().signal(),
            CompactionReason::Overflow,
        )
        .await;
    assert_eq!(
        outcome,
        crate::compaction::cached::CachedSummary::Fallback(
            "the last request overflowed the context window".into()
        )
    );
    // A run whose model-call limit has no room for the cached request and
    // Pi's two summaries after it keeps Pi's summary.
    *session
        .core
        .run_control
        .lock()
        .unwrap_or_else(PoisonError::into_inner) =
        Some(bake_agent::RunControl::new(bake_agent::RunLimits {
            max_model_calls: Some(2),
            ..Default::default()
        }));
    let outcome = session
        .core
        .cached_summary(
            &preparation,
            &model,
            None,
            AbortController::new().signal(),
            CompactionReason::Manual,
        )
        .await;
    assert_eq!(
        outcome,
        crate::compaction::cached::CachedSummary::Fallback(
            "the run's model-call limit leaves no room to try the cache first".into()
        )
    );
    *session
        .core
        .run_control
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    session.shutdown().await;
}

// The threshold and keep settings apply at once and are kept in memory
// without a settings file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_settings_change_at_once() {
    let cached = cached_harness();
    let session = AgentSession::create(options(&cached.h, in_memory(), Settings::default()))
        .await
        .expect("session");
    let saved = session
        .set_compaction_threshold_percent(Some(70))
        .await
        .expect("set");
    assert!(matches!(saved, DefaultSave::Failed(_)), "no settings file");
    session
        .set_compaction_keep_recent_tokens(Some(30_000))
        .await
        .expect("set");
    session
        .set_branch_summary_skip_prompt(true)
        .await
        .expect("set");
    session.set_status_line_cache(true).await.expect("set");
    session.set_status_line_tokens(true).await.expect("set");
    session.set_status_line_tokens(false).await.expect("set");
    let settings = session.settings();
    assert_eq!(
        settings.status_line,
        crate::settings::StatusLineSettings {
            tokens: false,
            cache: true
        }
    );
    let compaction = settings.compaction_settings(None).expect("valid");
    assert_eq!(compaction.threshold_percent, Some(70));
    assert_eq!(compaction.keep_recent_tokens, 30_000);
    assert!(settings.branch_summary_skip_prompt);
    session
        .set_compaction_threshold_percent(None)
        .await
        .expect("cleared");
    assert_eq!(
        session
            .settings()
            .compaction_settings(None)
            .map(|compaction| compaction.threshold_percent),
        Ok(None)
    );
    assert!(
        session
            .set_compaction_threshold_percent(Some(0))
            .await
            .is_err()
    );
    session.shutdown().await;
}

// Pi's footer reading: the totals of every response, as `/session` counts
// them, and the share of the latest prompt read from the cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_totals_count_every_response_and_the_latest_cache_share() {
    let cached = cached_harness();
    let session = two_turns(&cached, keep_one()).await;
    let totals = session.usage_totals();
    let stats = session.session_stats();
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (
            stats.input,
            stats.output,
            stats.cache_read,
            stats.cache_write
        )
    );
    assert!(
        totals.cache_read > 0 && totals.cache_write > 0,
        "{totals:?}"
    );
    let last = session
        .messages()
        .iter()
        .rev()
        .find_map(|message| message.as_assistant().cloned())
        .expect("a response");
    let prompt = last.usage.input + last.usage.cache_read + last.usage.cache_write;
    let expected = last.usage.cache_read as f64 / prompt as f64;
    let hit = totals.latest_cache_hit.expect("measured");
    assert!((hit - expected).abs() < 1e-9, "{hit} {expected}");
    session.shutdown().await;
}
