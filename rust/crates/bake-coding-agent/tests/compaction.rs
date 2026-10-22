//! Pi v1.4.0's compaction as the oracle. `fixtures/pi-compaction/golden.json`
//! records what Pi's own `compaction.ts`, `branch-summarization.ts`,
//! `utils.ts`, and `messages.ts` produce for one set of messages and session
//! entries (see `generate.mjs` beside it): the summarization prompts and
//! request options byte for byte, the summaries, the serialized
//! conversation, token estimates, cut points, preparations, and the
//! summary messages `convertToLlm` sends.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bake_agent::StreamFn;
use bake_ai::providers::faux::{faux_assistant_message, faux_model};
use bake_ai::utils::text::{blocks_text, get_system_message_text, user_content_text};
use bake_ai::{
    AbortController, Message, Model, ModelThinkingLevel, StopReason, Usage, UserContent,
    assistant_message_channel,
};
use bake_coding_agent::compaction::branch_summarization::{
    GenerateBranchSummaryOptions, generate_branch_summary, prepare_branch_entries,
};
use bake_coding_agent::compaction::utils::serialize_conversation;
use bake_coding_agent::compaction::{
    CompactionSettings, SummarizationOptions, compact, estimate_context_tokens,
    estimate_projected_context_tokens, estimate_tokens, find_cut_point,
    generate_summary_with_usage, prepare_compaction, projected_context_tokens,
};
use bake_coding_agent::session::json::js_stringify;
use bake_coding_agent::session::messages::convert_to_llm;
use bake_coding_agent::session::{
    AgentMessage, LeafSelector, SessionEntry, build_session_projection,
};
use serde_json::{Value, json};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/pi-compaction/golden.json")).unwrap()
}

fn messages(value: &Value) -> Vec<AgentMessage> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|message| AgentMessage::from_json(message.as_object().unwrap().clone()))
        .collect()
}

fn entries(oracle: &Value) -> Vec<SessionEntry> {
    oracle["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| SessionEntry::from_json(entry.as_object().unwrap().clone()).unwrap())
        .collect()
}

fn model(oracle: &Value) -> Model {
    let mut model = faux_model("faux-1");
    model.api = "faux".into();
    model.provider = "faux".into();
    model.reasoning = oracle["model"]["reasoning"].as_bool().unwrap();
    model.context_window = oracle["model"]["contextWindow"].as_u64().unwrap();
    model.max_tokens = oracle["model"]["maxTokens"].as_u64().unwrap();
    model
}

fn thinking(level: &str) -> ModelThinkingLevel {
    serde_json::from_value(json!(level)).unwrap()
}

/// JSON as JavaScript holds it: `0.0` is `0`.
fn to_json(value: &impl serde::Serialize) -> Value {
    let value = serde_json::to_value(value).unwrap();
    serde_json::from_str(&js_stringify(&value)).unwrap()
}

/// A stream function that records each request as `generate.mjs` does and
/// answers with the next queued text.
#[derive(Clone, Default)]
struct Recorder {
    requests: Arc<Mutex<Vec<Value>>>,
    answers: Arc<Mutex<VecDeque<String>>>,
}

impl Recorder {
    fn stream_fn(&self) -> StreamFn {
        let recorder = self.clone();
        Arc::new(move |model, context, options| {
            let transcript = context.messages();
            let system = match &transcript[0] {
                Message::System(system) => get_system_message_text(system),
                _ => String::new(),
            };
            let prompt = match &transcript[1] {
                Message::User(user) => user_content_text(&user.content, "\n"),
                _ => String::new(),
            };
            let mut request = json!({
                "model": model.id,
                "systemPrompt": system,
                "roles": transcript.iter().map(Message::role).collect::<Vec<_>>(),
                "prompt": prompt,
                "reasoning": options.reasoning.map(|level| level.as_str()),
                "cacheRetention": to_json(&options.base.cache_retention),
                "sessionIdSupplied": options.base.session_id.as_deref() == Some("routing"),
            });
            // JSON leaves out an unset JavaScript `maxTokens`.
            if let (Some(max_tokens), Some(object)) =
                (options.base.max_tokens, request.as_object_mut())
            {
                object.insert("maxTokens".into(), json!(max_tokens));
            }
            recorder.requests.lock().unwrap().push(request);
            let text = recorder
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "## Goal\nsummary".to_owned());
            let mut message = faux_assistant_message(&text);
            message.api = "faux".into();
            message.provider = "faux".into();
            message.model = "faux-1".into();
            message.usage = Usage {
                input: 30,
                output: 7,
                total_tokens: 37,
                ..Usage::default()
            };
            message.stop_reason = StopReason::Stop;
            let (sender, stream) = assistant_message_channel();
            sender.finish(message);
            Ok(stream)
        })
    }

    fn answer(&self, texts: &[&str]) {
        let mut answers = self.answers.lock().unwrap();
        answers.clear();
        answers.extend(texts.iter().map(|text| (*text).to_owned()));
    }

    fn take(&self) -> Value {
        Value::Array(std::mem::take(&mut *self.requests.lock().unwrap()))
    }
}

#[test]
fn estimates_serialization_and_conversion_match_pi() {
    let oracle = golden();
    let messages = messages(&oracle["messages"]);
    let estimates: Vec<u64> = messages.iter().map(estimate_tokens).collect();
    assert_eq!(to_json(&estimates), oracle["estimates"]);
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(
        json!({
            "tokens": estimate.tokens,
            "usageTokens": estimate.usage_tokens,
            "trailingTokens": estimate.trailing_tokens,
            "lastUsageIndex": estimate.last_usage_index,
        }),
        oracle["contextEstimate"]
    );
    let live: Vec<bake_agent::AgentMessage> = messages.iter().map(AgentMessage::to_agent).collect();
    let llm = convert_to_llm(&live);
    assert_eq!(serialize_conversation(&llm), oracle["serialized"]);
    let converted: Vec<Value> = llm
        .iter()
        .map(|message| {
            let text = match message {
                Message::User(user) => match &user.content {
                    UserContent::Text(text) => json!(text),
                    UserContent::Blocks(blocks) => json!(blocks_text(blocks, "\u{0}")),
                },
                _ => Value::Null,
            };
            json!({ "role": message.role(), "text": text })
        })
        .collect();
    assert_eq!(Value::Array(converted), oracle["converted"]);
}

#[test]
fn edited_contexts_estimate_from_characters_like_pi() {
    let oracle = golden();
    let edited: Vec<SessionEntry> = oracle["edited"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| SessionEntry::from_json(entry.as_object().unwrap().clone()).unwrap())
        .collect();
    let projection = build_session_projection(&edited, LeafSelector::Last);
    let estimate = estimate_projected_context_tokens(&projection, &edited);
    assert_eq!(
        json!({
            "tokens": estimate.tokens,
            "usageTokens": estimate.usage_tokens,
            "trailingTokens": estimate.trailing_tokens,
            "lastUsageIndex": estimate.last_usage_index,
        }),
        oracle["projectedEstimate"]
    );
}

// Pi's `getContextUsage`: after a compaction the tokens are unknown until
// a response after it records usage; a response without usage does not
// count, and before any compaction the projected estimate stands.
#[test]
fn context_tokens_are_unknown_after_a_compaction_until_a_response_measures_them() {
    let all = entries(&golden());
    let tokens = |branch: &[SessionEntry]| {
        projected_context_tokens(
            &build_session_projection(branch, LeafSelector::Last),
            branch,
        )
    };
    let estimate = |branch: &[SessionEntry]| {
        estimate_projected_context_tokens(
            &build_session_projection(branch, LeafSelector::Last),
            branch,
        )
        .tokens
    };
    // e5 is the compaction, e7 the next user message, and e8 the first
    // response after it with usage; e11 is a response with none.
    assert_eq!(all[5].entry_type(), "compaction");
    assert_eq!(tokens(&all[..5]), Some(estimate(&all[..5])));
    assert_eq!(tokens(&all[..8]), None, "only a summary and a prompt since");
    let mut unmeasured = all[11].as_json().clone();
    unmeasured.insert("parentId".into(), json!("e7"));
    let mut branch = all[..8].to_vec();
    branch.push(SessionEntry::from_json(unmeasured).unwrap());
    assert_eq!(tokens(&branch), None, "a response without usage");
    assert!(estimate(&all[..9]) > 0);
    assert_eq!(tokens(&all[..9]), Some(estimate(&all[..9])));
    assert_eq!(tokens(&all), Some(estimate(&all)));
}

#[test]
fn cut_points_and_preparations_match_pi() {
    let oracle = golden();
    let entries = entries(&oracle);
    for case in oracle["cutPoints"].as_array().unwrap() {
        let keep = case["keep"].as_u64().unwrap();
        let cut = find_cut_point(&entries, 0, entries.len(), keep);
        let turn_start = cut.turn_start_index.map_or(-1, |index| index as i64);
        assert_eq!(
            json!({
                "firstKeptEntryIndex": cut.first_kept_entry_index,
                "turnStartIndex": turn_start,
                "isSplitTurn": cut.is_split_turn,
            }),
            case["result"],
            "keep {keep}"
        );
    }
    for case in oracle["preparations"].as_array().unwrap() {
        let keep = case["keep"].as_u64().unwrap();
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: keep,
        };
        let preparation = prepare_compaction(&entries, settings).map(|prep| {
            json!({
                "firstKeptEntryId": prep.first_kept_entry_id,
                "messagesToSummarize": to_json(&prep.messages_to_summarize),
                "turnPrefixMessages": to_json(&prep.turn_prefix_messages),
                "isSplitTurn": prep.is_split_turn,
                "tokensBefore": prep.tokens_before,
                "previousSummary": prep.previous_summary,
                "fileOps": {
                    "read": prep.file_ops.read.iter().collect::<Vec<_>>(),
                    "written": prep.file_ops.written.iter().collect::<Vec<_>>(),
                    "edited": prep.file_ops.edited.iter().collect::<Vec<_>>(),
                },
            })
        });
        assert_eq!(
            preparation.unwrap_or(Value::Null),
            case.get("preparation").cloned().unwrap_or(Value::Null),
            "keep {keep}"
        );
    }
    for case in oracle["branchPreparations"].as_array().unwrap() {
        let prep = prepare_branch_entries(&entries, case["budget"].as_u64().unwrap());
        assert_eq!(to_json(&prep.messages), case["messages"]);
        assert_eq!(prep.total_tokens, case["totalTokens"].as_u64().unwrap());
        assert_eq!(
            json!({
                "read": prep.file_ops.read.iter().collect::<Vec<_>>(),
                "written": prep.file_ops.written.iter().collect::<Vec<_>>(),
                "edited": prep.file_ops.edited.iter().collect::<Vec<_>>(),
            }),
            case["fileOps"]
        );
    }
}

#[tokio::test]
async fn summaries_send_pis_prompts_and_build_pis_results() {
    let oracle = golden();
    let model = model(&oracle);
    let messages = messages(&oracle["messages"]);
    let recorder = Recorder::default();
    let stream_fn = recorder.stream_fn();

    for case in oracle["summaries"].as_array().unwrap() {
        recorder.answer(&[]);
        let options = SummarizationOptions {
            thinking_level: Some(thinking(case["thinkingLevel"].as_str().unwrap())),
            ..SummarizationOptions::default()
        };
        let result = generate_summary_with_usage(
            &messages,
            &model,
            case["customInstructions"].as_str(),
            case["previousSummary"].as_str(),
            &stream_fn,
            &options,
        )
        .await
        .unwrap();
        assert_eq!(
            json!({ "text": result.text, "usage": to_json(&result.usage) }),
            case["result"]
        );
        assert_eq!(recorder.take(), case["requests"]);
    }

    let entries = entries(&oracle);
    for case in oracle["compactions"].as_array().unwrap() {
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: case["keep"].as_u64().unwrap(),
        };
        let preparation = prepare_compaction(&entries, settings).unwrap();
        recorder.answer(&[
            "## Goal\nhistory summary",
            "## Original Request\nturn prefix",
        ]);
        let options = SummarizationOptions {
            thinking_level: Some(ModelThinkingLevel::Medium),
            session_id: Some("routing".into()),
            ..SummarizationOptions::default()
        };
        let result = compact(&preparation, &model, None, &stream_fn, &options)
            .await
            .unwrap();
        assert_eq!(to_json(&result), case["result"]);
        assert_eq!(recorder.take(), case["requests"]);
    }

    for case in oracle["branch"].as_array().unwrap() {
        recorder.answer(&["## Goal\nbranch"]);
        let options = &case["options"];
        let result = generate_branch_summary(
            &entries,
            &GenerateBranchSummaryOptions {
                model: model.clone(),
                stream_fn: Arc::clone(&stream_fn),
                api_key: None,
                headers: None,
                env: None,
                signal: AbortController::new().signal(),
                custom_instructions: options["customInstructions"].as_str().map(str::to_owned),
                replace_instructions: options["replaceInstructions"] == json!(true),
                reserve_tokens: options["reserveTokens"].as_u64(),
                retry: None,
                on_retry: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            json!({
                "summary": result.summary,
                "usage": result.usage.as_ref().map(to_json),
                "readFiles": result.read_files,
                "modifiedFiles": result.modified_files,
            }),
            case["result"]
        );
        assert_eq!(recorder.take(), case["requests"]);
    }
}
