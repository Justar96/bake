//! Transcript normalization and system-message replay.
//!
//! Ported from Pi `packages/ai/src/utils/transcript.ts` (v1.1.0). The
//! leading system message is the prompt; later system messages add
//! instructions, patch named sections, and add or remove tools.

use std::collections::{BTreeSet, HashMap};

use crate::types::{
    Context, Message, Sections, SystemContent, SystemMessage, Tool, ToolReference,
    TranscriptContext,
};
use crate::utils::text::{get_system_message_text, system_content_text};

/// The leading system message for a prompt and tools, or `None` when both
/// are empty.
pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    let has_prompt = system_prompt.is_some_and(|prompt| !prompt.is_empty());
    let tools = tools.filter(|tools| !tools.is_empty());
    if !has_prompt && tools.is_none() {
        return None;
    }
    Some(SystemMessage {
        content: SystemContent::Text(system_prompt.unwrap_or_default().to_owned()),
        sections: None,
        tools_added: tools.map(<[Tool]>::to_vec),
        tools_removed: None,
        timestamp: 0,
    })
}

/// Folds `Context::system_prompt` and `Context::tools` into a leading system
/// message: the only way to build a [`TranscriptContext`].
pub fn normalize_context(context: Context) -> TranscriptContext {
    let initial =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let mut messages = Vec::with_capacity(context.messages.len() + 1);
    messages.extend(initial.map(Message::System));
    messages.extend(context.messages);
    TranscriptContext { messages }
}

fn system(message: &Message) -> Option<&SystemMessage> {
    match message {
        Message::System(system) => Some(system),
        _ => None,
    }
}

/// The leading system message, if the transcript starts with one.
pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    messages.first().and_then(system)
}

/// The tools available after every system message, in first-declaration order.
pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    for message in messages.iter().filter_map(system) {
        for removed in message.tools_removed.iter().flatten() {
            tools.retain(|tool| tool.name != removed.name);
        }
        for added in message.tools_added.iter().flatten() {
            // A `Map.set` on an existing key keeps the key's position.
            if let Some(existing) = tools.iter_mut().find(|tool| tool.name == added.name) {
                *existing = added.clone();
            } else {
                tools.push(added.clone());
            }
        }
    }
    tools
}

/// Every system message replayed into one: content appended, sections
/// patched by name, tools resolved.
pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut timestamp: Option<i64> = None;
    for message in messages.iter().filter_map(system) {
        timestamp.get_or_insert(message.timestamp);
        let text = system_content_text(&message.content);
        if !text.is_empty() {
            content.push(text);
        }
        for (name, value) in message
            .sections
            .iter()
            .flat_map(|sections| sections.0.iter())
        {
            match value {
                None => sections.retain(|(known, _)| known != name),
                Some(value) => {
                    if let Some(entry) = sections.iter_mut().find(|(known, _)| known == name) {
                        entry.1 = value.clone();
                    } else {
                        sections.push((name.clone(), value.clone()));
                    }
                }
            }
        }
    }
    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    Some(SystemMessage {
        content: SystemContent::Text(content.join("\n\n")),
        sections: (!sections.is_empty()).then(|| {
            Sections(
                sections
                    .into_iter()
                    .map(|(name, value)| (name, Some(value)))
                    .collect(),
            )
        }),
        tools_added: (!tools.is_empty()).then_some(tools),
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

/// The current system prompt text after replaying every system message.
pub fn get_current_system_prompt(messages: &[Message]) -> String {
    get_current_system_message(messages)
        .map(|message| get_system_message_text(&message))
        .unwrap_or_default()
}

/// The transcript for APIs without mid-conversation system messages: the
/// replayed system message leads and later system messages are dropped.
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let head = get_current_system_message(&context.messages);
    let mut messages = Vec::with_capacity(context.messages.len());
    messages.extend(head.map(Message::System));
    messages.extend(
        context
            .messages
            .iter()
            .filter(|message| system(message).is_none())
            .cloned(),
    );
    TranscriptContext { messages }
}

/// Keeps later system messages in place when the model accepts them;
/// otherwise collapses them.
pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: bool,
) -> TranscriptContext {
    if supports_mid_convo_system_messages {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

/// Whether two tools declare the same interface to the model.
/// Like Pi, it compares the serialized declarations, so member order counts.
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    match (serde_json::to_string(left), serde_json::to_string(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Tool changes between two complete tool states; a changed definition is a
/// removal followed by an addition.
pub fn get_tool_state_changes(
    previous: &[Tool],
    current: &[Tool],
) -> (Vec<Tool>, Vec<ToolReference>) {
    let previous_by_name: HashMap<&str, &Tool> = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let current_by_name: HashMap<&str, &Tool> = current
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let added = current
        .iter()
        .filter(|tool| {
            previous_by_name
                .get(tool.name.as_str())
                .is_none_or(|old| !declarations_equal(old, tool))
        })
        .cloned()
        .collect();
    let removed = previous
        .iter()
        .filter(|tool| {
            current_by_name
                .get(tool.name.as_str())
                .is_none_or(|new| !declarations_equal(tool, new))
        })
        .map(|tool| ToolReference {
            name: tool.name.clone(),
        })
        .collect();
    (added, removed)
}

/// Every definition referenced by transcript tool state, last definition per
/// name, in first-declaration order.
pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    for added in messages
        .iter()
        .filter_map(system)
        .flat_map(|message| message.tools_added.iter().flatten())
    {
        if let Some(existing) = tools.iter_mut().find(|tool| tool.name == added.name) {
            *existing = added.clone();
        } else {
            tools.push(added.clone());
        }
    }
    tools
}

/// Whether a tool was removed or redeclared, which an addition-only
/// transport cannot replay.
pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
    let mut declared = BTreeSet::new();
    for message in messages.iter().filter_map(system) {
        if message
            .tools_removed
            .as_ref()
            .is_some_and(|removed| !removed.is_empty())
        {
            return true;
        }
        for tool in message.tools_added.iter().flatten() {
            if !declared.insert(tool.name.clone()) {
                return true;
            }
        }
    }
    false
}

/// Pi's `resolveTranscriptTools`: the request-level tools and whether later
/// system messages carry their additions in place.
pub fn resolve_transcript_tools(
    messages: &[Message],
    supports_tool_additions: bool,
) -> (Vec<Tool>, bool) {
    let anchors_additions = supports_tool_additions && !has_non_additive_tool_changes(messages);
    let request_tools = if anchors_additions {
        get_initial_system_message(messages)
            .and_then(|message| message.tools_added.clone())
            .unwrap_or_default()
    } else {
        get_current_tools(messages)
    };
    (request_tools, anchors_additions)
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/system-message-replay.test.ts`.

    use super::*;
    use crate::types::{AssistantContentBlock, TextContent, UserMessage};
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        tool_described(name, &format!("{name} tool"))
    }

    fn tool_described(name: &str, description: &str) -> Tool {
        Tool {
            name: name.into(),
            description: description.into(),
            parameters: json!({ "type": "object", "properties": {} }),
            constrained_sampling: None,
        }
    }

    fn user(text: &str, timestamp: i64) -> Message {
        Message::User(UserMessage {
            content: text.into(),
            timestamp,
        })
    }

    fn sections(entries: &[(&str, Option<&str>)]) -> Option<Sections> {
        Some(Sections(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned)))
                .collect(),
        ))
    }

    fn transcript() -> TranscriptContext {
        let mut assistant = crate::providers::faux::faux_assistant_message("ok");
        assistant.timestamp = 13;
        assistant.content = vec![AssistantContentBlock::Text(TextContent::new("ok"))];
        normalize_context(Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::System(SystemMessage {
                    content: "base".into(),
                    sections: sections(&[("a", Some("<a>1</a>")), ("b", Some("<b>1</b>"))]),
                    tools_added: Some(vec![tool("first")]),
                    tools_removed: None,
                    timestamp: 10,
                }),
                user("hello", 11),
                Message::System(SystemMessage {
                    content: "also do this".into(),
                    timestamp: 12,
                    ..Default::default()
                }),
                Message::Assistant(assistant),
                Message::System(SystemMessage {
                    content: "".into(),
                    sections: sections(&[
                        ("a", Some("<a>2</a>")),
                        ("b", None),
                        ("c", Some("<c>1</c>")),
                    ]),
                    tools_removed: Some(vec![ToolReference {
                        name: "first".into(),
                    }]),
                    tools_added: Some(vec![tool("second")]),
                    timestamp: 14,
                }),
            ],
        })
    }

    #[test]
    fn replays_content_sections_and_tools() {
        let context = transcript();
        let current = get_current_system_message(context.messages()).unwrap();
        assert_eq!(
            current,
            SystemMessage {
                content: "base\n\nalso do this".into(),
                sections: sections(&[("a", Some("<a>2</a>")), ("c", Some("<c>1</c>"))]),
                tools_added: Some(vec![tool("second")]),
                tools_removed: None,
                timestamp: 10,
            }
        );
        assert_eq!(
            get_current_system_prompt(context.messages()),
            "base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
        );
    }

    #[test]
    fn collapse_keeps_non_system_messages_after_the_head() {
        let collapsed = collapse_system_messages(&transcript());
        let roles: Vec<_> = collapsed.messages().iter().map(Message::role).collect();
        assert_eq!(roles, ["system", "user", "assistant"]);
        assert_eq!(collapse_system_messages(&collapsed), collapsed);
    }

    #[test]
    fn replay_without_system_messages_is_empty() {
        let context = normalize_context(Context {
            messages: vec![user("hi", 1)],
            ..Default::default()
        });
        assert_eq!(get_current_system_message(context.messages()), None);
        assert_eq!(get_current_system_prompt(context.messages()), "");
        assert_eq!(
            collapse_system_messages(&context).messages(),
            context.messages()
        );
    }

    #[test]
    fn a_late_patch_without_a_leading_message_replays_as_the_prompt() {
        let context = normalize_context(Context {
            messages: vec![
                user("old session", 1),
                Message::System(SystemMessage {
                    content: "".into(),
                    sections: sections(&[("preamble", Some("You are pi."))]),
                    tools_added: Some(vec![tool("x")]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
            ..Default::default()
        });
        assert_eq!(get_current_system_prompt(context.messages()), "You are pi.");
        let collapsed = collapse_system_messages(&context);
        let Some(Message::System(head)) = collapsed.messages().first() else {
            panic!("expected a head")
        };
        assert_eq!(head.tools_added, Some(vec![tool("x")]));
    }

    #[test]
    fn renders_complete_prompts_and_framed_updates() {
        let context = transcript();
        let (Some(Message::System(leading)), Some(Message::System(update))) =
            (context.messages().first(), context.messages().get(4))
        else {
            panic!("expected system messages");
        };
        assert_eq!(
            get_system_message_text(leading),
            "base\n\n<a>1</a>\n\n<b>1</b>"
        );
        assert_eq!(
            crate::utils::text::render_system_message_update(update),
            [
                "Updated system prompt section \"a\":\n\n<a>2</a>",
                "Removed system prompt section \"b\".",
                "Updated system prompt section \"c\":\n\n<c>1</c>",
            ]
            .join("\n\n")
        );
    }

    #[test]
    fn normalizes_the_legacy_prompt_and_tool_fields() {
        let messages = vec![user("hi", 1)];
        assert_eq!(
            normalize_context(Context {
                messages: messages.clone(),
                ..Default::default()
            })
            .messages(),
            &messages[..]
        );
        assert_eq!(
            normalize_context(Context {
                system_prompt: Some(String::new()),
                tools: Some(vec![]),
                messages: messages.clone()
            })
            .messages(),
            &messages[..]
        );
        let normalized = normalize_context(Context {
            system_prompt: Some("be brief".into()),
            tools: Some(vec![tool("a")]),
            messages: messages.clone(),
        });
        assert_eq!(
            serde_json::to_value(normalized.messages()).unwrap(),
            json!([
                { "role": "system", "content": "be brief", "toolsAdded": [serde_json::to_value(tool("a")).unwrap()], "timestamp": 0 },
                { "role": "user", "content": "hi", "timestamp": 1 }
            ])
        );
    }

    #[test]
    fn tool_state_changes_treat_changed_definitions_as_removal_plus_addition() {
        let (added, removed) = get_tool_state_changes(
            &[tool("a"), tool("b")],
            &[tool_described("b", "changed"), tool("c")],
        );
        assert_eq!(added, vec![tool_described("b", "changed"), tool("c")]);
        assert_eq!(
            removed,
            vec![
                ToolReference { name: "a".into() },
                ToolReference { name: "b".into() }
            ]
        );
        assert_eq!(
            get_tool_state_changes(&[tool("a")], &[tool("a")]),
            (vec![], vec![])
        );
        assert!(!declarations_equal(
            &tool("a"),
            &Tool {
                constrained_sampling: Some(crate::types::ConstrainedSampling::Disabled),
                ..tool("a")
            }
        ));
    }

    #[test]
    fn detects_non_additive_tool_history() {
        assert!(has_non_additive_tool_changes(transcript().messages()));
        let system = |tools: Vec<Tool>, timestamp| {
            Message::System(SystemMessage {
                tools_added: Some(tools),
                timestamp,
                ..Default::default()
            })
        };
        assert!(!has_non_additive_tool_changes(&[
            system(vec![tool("a")], 1),
            system(vec![tool("b")], 2)
        ]));
        assert!(has_non_additive_tool_changes(&[
            system(vec![tool("a")], 1),
            system(vec![tool_described("a", "changed")], 2)
        ]));
    }
}
