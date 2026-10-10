//! Message conversion and stream assembly for OpenAI Responses.
//!
//! Ported from Pi `packages/ai/src/api/openai-responses-shared.ts` (v1.1.0).
//! Not ported: grammar-constrained custom tools, message-anchored
//! `additional_tools`, and client tool search; without them every request
//! sends the current tool set at the top level.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::transform_messages::transform_messages;
use crate::api::{count, non_empty_str, sanitize_id_chars, truncate_units, truthy};
use crate::models::calculate_cost;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, Model, StopReason,
    TextContent, ThinkingContent, Tool, ToolCall, TranscriptContext, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::event_stream::AssistantMessageEventSender;
use crate::utils::hash::short_hash;
use crate::utils::json_parse::parse_streaming_json_object;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::resolve_transcript;

fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> String {
    let mut payload = Map::new();
    payload.insert("v".into(), json!(1));
    payload.insert("id".into(), json!(id));
    if let Some(phase) = phase.filter(|phase| !phase.is_empty()) {
        payload.insert("phase".into(), json!(phase));
    }
    Value::Object(payload).to_string()
}

fn parse_text_signature(signature: Option<&str>) -> Option<(String, Option<String>)> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    if signature.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<Value>(signature)
        && parsed.get("v").and_then(Value::as_i64) == Some(1)
        && let Some(id) = parsed.get("id").and_then(Value::as_str)
    {
        let phase = parsed
            .get("phase")
            .and_then(Value::as_str)
            .filter(|phase| *phase == "commentary" || *phase == "final_answer")
            .map(str::to_owned);
        return Some((id.to_owned(), phase));
    }
    Some((signature.to_owned(), None))
}

fn convert_tool_result_output(model: &Model, content: &[UserContentBlock]) -> Value {
    let text = content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<_> = content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Image(image) => Some(image),
            UserContentBlock::Text(_) => None,
        })
        .collect();
    if images.is_empty() || !model.accepts_images() {
        let text = if !text.is_empty() {
            text
        } else if !images.is_empty() {
            "(see attached image)".to_owned()
        } else {
            "(no tool output)".to_owned()
        };
        return json!(text);
    }
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({ "type": "input_text", "text": text }));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
        }));
    }
    Value::Array(output)
}

/// Options of [`convert_responses_messages`].
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesMessagesOptions {
    /// Whether the leading system message is sent; true when `None`.
    pub include_system_prompt: Option<bool>,
    /// Whether later system messages are sent in place.
    pub supports_mid_convo_system_messages: bool,
}

fn normalize_id_part(part: &str) -> String {
    let sanitized = sanitize_id_chars(part);
    let normalized = if sanitized.len() > 64 {
        truncate_units(&sanitized, 64)
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_owned()
}

fn foreign_item_id(item_id: &str) -> String {
    truncate_units(&format!("fc_{}", short_hash(item_id)), 64)
}

/// Pi's `convertResponsesMessages`: the transcript as Responses input items.
pub fn convert_responses_messages(
    model: &Model,
    context: &TranscriptContext,
    allowed_tool_call_providers: &BTreeSet<&str>,
    options: &ConvertResponsesMessagesOptions,
) -> Result<Vec<Value>, String> {
    let normalized = resolve_transcript(context, options.supports_mid_convo_system_messages);
    let normalize = |id: &str, _: &Model, source: &AssistantMessage| -> String {
        if !allowed_tool_call_providers.contains(model.provider.as_str()) || !id.contains('|') {
            return normalize_id_part(id);
        }
        let mut parts = id.split('|');
        let call_id = normalize_id_part(parts.next().unwrap_or(""));
        let item_id = parts.next().unwrap_or("");
        let foreign = source.provider != model.provider || source.api != model.api;
        let mut item = if foreign {
            foreign_item_id(item_id)
        } else {
            normalize_id_part(item_id)
        };
        if !item.starts_with("fc_") {
            item = normalize_id_part(&format!("fc_{item}"));
        }
        format!("{call_id}|{item}")
    };
    let transformed = transform_messages(normalized.messages(), model, Some(&normalize));
    let include_initial = options.include_system_prompt.unwrap_or(true);
    let supports_developer_role = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_developer_role)
        != Some(false);
    let instruction_role = if model.reasoning && supports_developer_role {
        "developer"
    } else {
        "system"
    };
    let mut items: Vec<Value> = Vec::new();
    let mut msg_index = 0usize;
    for (source_index, message) in transformed.iter().enumerate() {
        let leading_system = source_index == 0 && matches!(message, Message::System(_));
        match message {
            Message::System(system) => {
                if !leading_system || include_initial {
                    let text = if leading_system {
                        get_system_message_text(system)
                    } else {
                        render_system_message_update(system)
                    };
                    if !text.is_empty() {
                        items.push(json!({ "role": instruction_role, "content": text }));
                    }
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    items.push(json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] }));
                }
                UserContent::Blocks(blocks) => {
                    if blocks.is_empty() {
                        continue;
                    }
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({ "type": "input_text", "text": text.text }),
                            UserContentBlock::Image(image) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
                            }),
                        })
                        .collect();
                    items.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(assistant) => {
                let same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let same_model = same_provider_and_api && assistant.model == model.id;
                let different_model = same_provider_and_api && assistant.model != model.id;
                let mut output: Vec<Value> = Vec::new();
                let mut text_block_index = 0usize;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if let Some(signature) = thinking
                                .thinking_signature
                                .as_deref()
                                .filter(|s| !s.is_empty())
                            {
                                let item: Value =
                                    serde_json::from_str(signature).map_err(|error| {
                                        format!(
                                            "Invalid reasoning item in thinking signature: {error}"
                                        )
                                    })?;
                                output.push(item);
                            }
                        }
                        AssistantContentBlock::Text(text) => {
                            let parsed = parse_text_signature(text.text_signature.as_deref());
                            let fallback = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            let (id, phase) = match parsed {
                                Some((id, phase)) if !id.is_empty() => {
                                    let id = if id.encode_utf16().count() > 64 {
                                        format!("msg_{}", short_hash(&id))
                                    } else {
                                        id
                                    };
                                    (id, phase)
                                }
                                Some((_, phase)) => (fallback, phase),
                                None => (fallback, None),
                            };
                            let mut item = json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "output_text", "text": text.text, "annotations": [] }],
                                "status": "completed",
                                "id": id
                            });
                            if let (Some(phase), Some(object)) = (phase, item.as_object_mut()) {
                                object.insert("phase".into(), json!(phase));
                            }
                            output.push(item);
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            let mut parts = call.id.split('|');
                            let call_id = parts.next().unwrap_or("");
                            let item_id = parts
                                .next()
                                .filter(|item| !different_model && item.starts_with("fc_"));
                            let mut item = Map::new();
                            item.insert("type".into(), json!("function_call"));
                            if let Some(item_id) = item_id {
                                item.insert("id".into(), json!(item_id));
                            }
                            item.insert("call_id".into(), json!(call_id));
                            item.insert("name".into(), json!(call.name));
                            item.insert(
                                "arguments".into(),
                                json!(serde_json::to_string(&call.arguments).unwrap_or_default()),
                            );
                            if same_model && let Some(namespace) = &call.namespace {
                                item.insert("namespace".into(), json!(namespace));
                            }
                            output.push(Value::Object(item));
                        }
                    }
                }
                if output.is_empty() {
                    continue;
                }
                items.extend(output);
            }
            Message::ToolResult(result) => {
                let call_id = result.tool_call_id.split('|').next().unwrap_or("");
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": convert_tool_result_output(model, &result.content)
                }));
            }
        }
        if !leading_system {
            msg_index += 1;
        }
    }
    Ok(items)
}

/// Pi's `convertResponsesTools` for JSON-schema function tools.
pub fn convert_responses_tools(
    tools: &[Tool],
    supports_strict_mode: bool,
) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?
                .unwrap_or(false);
            let mut item = Map::new();
            item.insert("type".into(), json!("function"));
            item.insert("name".into(), json!(tool.name));
            item.insert("description".into(), json!(tool.description));
            item.insert(
                "parameters".into(),
                get_json_schema_tool_parameters(tool, Some(strict))?,
            );
            if supports_strict_mode {
                item.insert("strict".into(), json!(strict));
            }
            Ok(Value::Object(item))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

/// A service-tier pricing hook: adjusts usage for the tier in effect.
pub type ServiceTierPricing = Box<dyn Fn(&mut Usage, Option<&str>) + Send + Sync>;

/// Assembles Responses stream events into an assistant message.
pub struct ResponsesStreamProcessor {
    model: Model,
    /// The message being built.
    pub output: AssistantMessage,
    saw_terminal: bool,
    slots: HashMap<Option<i64>, (SlotKind, usize)>,
    partial_json: HashMap<usize, String>,
    reasoning_by_id: HashMap<String, usize>,
    service_tier: Option<String>,
    pricing: Option<ServiceTierPricing>,
}

/// The text a JavaScript template literal makes of a member.
fn template(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn texts(items: Option<&Value>, separator: &str) -> String {
    items
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.get("text").and_then(Value::as_str).unwrap_or(""))
                .collect::<Vec<_>>()
                .join(separator)
        })
        .unwrap_or_default()
}

fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<(StopReason, Option<String>), String> {
    Ok(match status {
        None | Some("") => (StopReason::Stop, None),
        Some("completed") => (StopReason::Stop, None),
        Some("incomplete") => match incomplete_reason {
            Some("max_output_tokens") => (StopReason::Length, None),
            Some(reason) => (
                StopReason::Error,
                Some(format!("Response incomplete: {reason}")),
            ),
            None => (
                StopReason::Error,
                Some("Response incomplete without a provider reason".to_owned()),
            ),
        },
        Some("failed" | "cancelled") => (StopReason::Error, None),
        Some("in_progress" | "queued") => (StopReason::Stop, None),
        Some(other) => return Err(format!("Unhandled stop reason: {other}")),
    })
}

impl ResponsesStreamProcessor {
    /// A processor for `model` filling `output`; `service_tier` is the
    /// requested tier and `pricing` applies tier multipliers.
    pub fn new(
        model: &Model,
        output: AssistantMessage,
        service_tier: Option<String>,
        pricing: Option<ServiceTierPricing>,
    ) -> Self {
        Self {
            model: model.clone(),
            output,
            saw_terminal: false,
            slots: HashMap::new(),
            partial_json: HashMap::new(),
            reasoning_by_id: HashMap::new(),
            service_tier,
            pricing,
        }
    }

    fn emit(
        &self,
        sender: &AssistantMessageEventSender,
        make: impl FnOnce(AssistantMessage) -> AssistantMessageEvent,
    ) {
        sender.push(make(self.output.clone()));
    }

    fn apply_message_phase(&mut self, item: &Value) {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
        {
            self.output.stop_reason = StopReason::Stop;
        }
    }

    fn slot(&self, output_index: Option<i64>, kind: SlotKind) -> Option<usize> {
        self.slots
            .get(&output_index)
            .filter(|(slot_kind, _)| *slot_kind == kind)
            .map(|(_, index)| *index)
    }

    fn create_slot(
        &mut self,
        output_index: Option<i64>,
        item: &Value,
        sender: &AssistantMessageEventSender,
    ) -> Option<(SlotKind, usize)> {
        let kind = match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                self.output
                    .content
                    .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                SlotKind::Thinking
            }
            Some("message") => {
                self.apply_message_phase(item);
                self.output
                    .content
                    .push(AssistantContentBlock::Text(TextContent::new("")));
                SlotKind::Text
            }
            Some("function_call") => {
                self.output
                    .content
                    .push(AssistantContentBlock::ToolCall(ToolCall {
                        id: format!(
                            "{}|{}",
                            template(item.get("call_id")),
                            template(item.get("id"))
                        ),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        namespace: item
                            .get("namespace")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        ..ToolCall::default()
                    }));
                let index = self.output.content.len() - 1;
                let initial = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                self.partial_json.insert(index, initial.to_owned());
                SlotKind::ToolCall
            }
            _ => return None,
        };
        let index = self.output.content.len() - 1;
        self.slots.insert(output_index, (kind, index));
        match kind {
            SlotKind::Thinking => {
                self.emit(sender, |partial| AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial,
                })
            }
            SlotKind::Text => self.emit(sender, |partial| AssistantMessageEvent::TextStart {
                content_index: index,
                partial,
            }),
            SlotKind::ToolCall => {
                self.emit(sender, |partial| AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial,
                })
            }
        }
        Some((kind, index))
    }

    fn append_thinking(&mut self, index: usize, delta: &str, sender: &AssistantMessageEventSender) {
        if let Some(AssistantContentBlock::Thinking(block)) = self.output.content.get_mut(index) {
            block.thinking.push_str(delta);
        }
        self.emit(sender, |partial| AssistantMessageEvent::ThinkingDelta {
            content_index: index,
            delta: delta.to_owned(),
            partial,
        });
    }

    fn append_text(&mut self, index: usize, delta: &str, sender: &AssistantMessageEventSender) {
        if let Some(AssistantContentBlock::Text(block)) = self.output.content.get_mut(index) {
            block.text.push_str(delta);
        }
        self.emit(sender, |partial| AssistantMessageEvent::TextDelta {
            content_index: index,
            delta: delta.to_owned(),
            partial,
        });
    }

    fn set_arguments(&mut self, index: usize) {
        let parsed = parse_streaming_json_object(self.partial_json.get(&index).map(String::as_str));
        if let Some(AssistantContentBlock::ToolCall(call)) = self.output.content.get_mut(index) {
            call.arguments = parsed;
        }
    }

    fn backfill_reasoning(&mut self, response_output: Option<&Value>) {
        for item in response_output
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if item.get("type").and_then(Value::as_str) != Some("reasoning")
                || !truthy(item.get("encrypted_content"))
            {
                continue;
            }
            let Some(index) = item
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| self.reasoning_by_id.get(id))
            else {
                continue;
            };
            let Some(AssistantContentBlock::Thinking(block)) = self.output.content.get_mut(*index)
            else {
                continue;
            };
            let Some(Ok(Value::Object(mut stored))) = block
                .thinking_signature
                .as_deref()
                .map(serde_json::from_str::<Value>)
            else {
                continue;
            };
            if truthy(stored.get("encrypted_content")) {
                continue;
            }
            if let Some(encrypted) = item.get("encrypted_content") {
                stored.insert("encrypted_content".into(), encrypted.clone());
            }
            block.thinking_signature = Some(Value::Object(stored).to_string());
        }
    }

    fn finalize_response(&mut self, response: Option<&Value>) -> Result<(), String> {
        self.saw_terminal = true;
        let response = response.cloned().unwrap_or(Value::Null);
        self.backfill_reasoning(response.get("output"));
        if let Some(id) = non_empty_str(response.get("id")) {
            self.output.response_id = Some(id.to_owned());
        }
        if let Some(usage) = response.get("usage").filter(|usage| truthy(Some(usage))) {
            let details = usage.get("input_tokens_details");
            let cached = count(details.and_then(|d| d.get("cached_tokens"))).unwrap_or(0);
            let cache_write = count(details.and_then(|d| d.get("cache_write_tokens"))).unwrap_or(0);
            self.output.usage = Usage {
                input: count(usage.get("input_tokens"))
                    .unwrap_or(0)
                    .saturating_sub(cached)
                    .saturating_sub(cache_write),
                output: count(usage.get("output_tokens")).unwrap_or(0),
                cache_read: cached,
                cache_write,
                reasoning: Some(
                    count(
                        usage
                            .get("output_tokens_details")
                            .and_then(|d| d.get("reasoning_tokens")),
                    )
                    .unwrap_or(0),
                ),
                total_tokens: count(usage.get("total_tokens")).unwrap_or(0),
                ..Usage::default()
            };
        }
        calculate_cost(&self.model, &mut self.output.usage);
        if let Some(pricing) = &self.pricing {
            let tier = response
                .get("service_tier")
                .and_then(Value::as_str)
                .or(self.service_tier.as_deref());
            pricing(&mut self.output.usage, tier);
        }
        let status = response.get("status").and_then(Value::as_str);
        let incomplete_reason = response
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty());
        self.output.raw_stop_reason = match (status, incomplete_reason) {
            (status, Some(reason)) => Some(format!("{}.{reason}", status.unwrap_or("undefined"))),
            (status, None) => status.map(str::to_owned),
        };
        let (stop_reason, error) = map_stop_reason(status, incomplete_reason)?;
        self.output.stop_reason = stop_reason;
        self.output.error_message = error;
        if self.output.stop_reason == StopReason::Stop && self.output.tool_calls().next().is_some()
        {
            self.output.stop_reason = StopReason::ToolUse;
        }
        Ok(())
    }

    /// Applies one parsed stream event; `Err` ends the response.
    pub fn handle_event(
        &mut self,
        event: &Value,
        sender: &AssistantMessageEventSender,
    ) -> Result<(), String> {
        let output_index = event.get("output_index").and_then(Value::as_i64);
        let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.created" => {
                self.output.response_id = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item") {
                    self.create_slot(output_index, item, sender);
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(index) = self.slot(output_index, SlotKind::Thinking) {
                    self.append_thinking(index, delta, sender);
                }
            }
            "response.reasoning_summary_part.done" => {
                if let Some(index) = self.slot(output_index, SlotKind::Thinking) {
                    self.append_thinking(index, "\n\n", sender);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(index) = self.slot(output_index, SlotKind::Text) {
                    self.append_text(index, delta, sender);
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(index) = self.slot(output_index, SlotKind::ToolCall)
                    && let Some(partial) = self.partial_json.get_mut(&index)
                {
                    partial.push_str(delta);
                    self.set_arguments(index);
                    self.emit(sender, |partial| AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: delta.to_owned(),
                        partial,
                    });
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(index) = self.slot(output_index, SlotKind::ToolCall)
                    && let Some(previous) = self.partial_json.get(&index).cloned()
                {
                    let arguments = event
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    self.partial_json.insert(index, arguments.clone());
                    self.set_arguments(index);
                    if let Some(rest) = arguments
                        .strip_prefix(previous.as_str())
                        .filter(|rest| !rest.is_empty())
                    {
                        let rest = rest.to_owned();
                        self.emit(sender, |partial| AssistantMessageEvent::ToolCallDelta {
                            content_index: index,
                            delta: rest,
                            partial,
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let item = event.get("item").cloned().unwrap_or(Value::Null);
                self.apply_message_phase(&item);
                let slot = match self.slots.get(&output_index).copied() {
                    Some(slot) => Some(slot),
                    None => self.create_slot(output_index, &item, sender),
                };
                self.finish_item(output_index, &item, slot, sender);
            }
            "response.completed" | "response.incomplete" => {
                self.finalize_response(event.get("response"))?
            }
            "error" => {
                return Err(format!(
                    "Error Code {}: {}",
                    template(event.get("code")),
                    template(event.get("message"))
                ));
            }
            "response.failed" => {
                self.saw_terminal = true;
                let response = event.get("response");
                self.output.raw_stop_reason = response
                    .and_then(|r| r.get("status"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let error = response
                    .and_then(|r| r.get("error"))
                    .filter(|error| truthy(Some(error)));
                let details = response.and_then(|r| r.get("incomplete_details"));
                let message = if let Some(error) = error {
                    let code = error
                        .get("code")
                        .filter(|code| truthy(Some(code)))
                        .map_or("unknown".to_owned(), |c| template(Some(c)));
                    let message = error
                        .get("message")
                        .filter(|message| truthy(Some(message)))
                        .map_or("no message".to_owned(), |m| template(Some(m)));
                    format!("{code}: {message}")
                } else if let Some(reason) = details
                    .and_then(|d| d.get("reason"))
                    .filter(|reason| truthy(Some(reason)))
                {
                    format!("incomplete: {}", template(Some(reason)))
                } else {
                    "Unknown error (no error details in response)".to_owned()
                };
                return Err(message);
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_item(
        &mut self,
        output_index: Option<i64>,
        item: &Value,
        slot: Option<(SlotKind, usize)>,
        sender: &AssistantMessageEventSender,
    ) {
        let item_type = item.get("type").and_then(Value::as_str);
        match (item_type, slot) {
            (Some("reasoning"), Some((SlotKind::Thinking, index))) => {
                let summary = texts(item.get("summary"), "\n\n");
                let content = texts(item.get("content"), "\n\n");
                if let Some(AssistantContentBlock::Thinking(block)) =
                    self.output.content.get_mut(index)
                {
                    if !summary.is_empty() {
                        block.thinking = summary;
                    } else if !content.is_empty() {
                        block.thinking = content;
                    }
                    block.thinking_signature = Some(item.to_string());
                }
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    self.reasoning_by_id.insert(id.to_owned(), index);
                }
                let content = match self.output.content.get(index) {
                    Some(AssistantContentBlock::Thinking(block)) => block.thinking.clone(),
                    _ => String::new(),
                };
                self.emit(sender, |partial| AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content,
                    partial,
                });
                self.slots.remove(&output_index);
            }
            (Some("message"), Some((SlotKind::Text, index))) => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|part| {
                                let field = if part.get("type").and_then(Value::as_str)
                                    == Some("output_text")
                                {
                                    "text"
                                } else {
                                    "refusal"
                                };
                                part.get(field).and_then(Value::as_str).unwrap_or("")
                            })
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                let signature = encode_text_signature_v1(
                    &template(item.get("id")),
                    item.get("phase").and_then(Value::as_str),
                );
                if let Some(AssistantContentBlock::Text(block)) = self.output.content.get_mut(index)
                {
                    block.text = text.clone();
                    block.text_signature = Some(signature);
                }
                self.emit(sender, |partial| AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text,
                    partial,
                });
                self.slots.remove(&output_index);
            }
            (Some("function_call"), Some((SlotKind::ToolCall, index))) => {
                let Some(partial) = self.partial_json.remove(&index) else {
                    return;
                };
                let source = non_empty_str(item.get("arguments"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        if partial.is_empty() {
                            "{}".to_owned()
                        } else {
                            partial
                        }
                    });
                let arguments = parse_streaming_json_object(Some(&source));
                let tool_call = match self.output.content.get_mut(index) {
                    Some(AssistantContentBlock::ToolCall(call)) => {
                        call.arguments = arguments;
                        if let Some(namespace) = item.get("namespace").and_then(Value::as_str) {
                            call.namespace = Some(namespace.to_owned());
                        }
                        call.clone()
                    }
                    _ => return,
                };
                self.emit(sender, |partial| AssistantMessageEvent::ToolCallEnd {
                    content_index: index,
                    tool_call,
                    partial,
                });
                self.slots.remove(&output_index);
            }
            _ => {}
        }
    }

    /// Checks the stream after its last event.
    pub fn finish(&self) -> Result<(), String> {
        if !self.saw_terminal {
            return Err(
                "OpenAI Responses stream ended before a terminal response event".to_owned(),
            );
        }
        if self.output.stop_reason == StopReason::ToolUse {
            for (index, block) in self.output.content.iter().enumerate() {
                if let AssistantContentBlock::ToolCall(call) = block
                    && self.partial_json.contains_key(&index)
                {
                    return Err(format!(
                        "OpenAI Responses stream completed with an unfinished tool call: {} ({})",
                        call.name, call.id
                    ));
                }
            }
        }
        Ok(())
    }
}
