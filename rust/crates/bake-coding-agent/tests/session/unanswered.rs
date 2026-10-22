//! The unanswered tool calls of a leaf, against two oracles: each case
//! states the calls Pi's `transformMessages` (`packages/ai/src/api/
//! transform-messages.ts`, v1.3.0) closes with its synthetic result, worked
//! out by hand from that rule, and every case and every truncated file also
//! checks agreement with the synthetic results `bake-ai`'s
//! `transform_messages` inserts into the request the agent would build.

use std::fs;

use bake_ai::api::transform_messages::transform_messages;
use bake_ai::providers::faux::faux_model;
use bake_ai::types::{Message, UserContentBlock};
use bake_coding_agent::session::messages::convert_to_llm;
use bake_coding_agent::session::{
    AgentMessage, EditContent, FileEntry, LeafSelector, NewSessionOptions, SessionEntry,
    SessionManager, UnansweredToolCall, build_session_context, build_unanswered_tool_calls,
    parse_session_entries,
};
use serde_json::{Value, json};

use crate::support::{TempDir, entry, message, usage, user_msg};

const SYNTHETIC: &str = "No result provided";

fn assistant_value(stop: &str, calls: &[(&str, &str)]) -> Value {
    let mut content = vec![json!({"type": "text", "text": "working"})];
    content.extend(calls.iter().map(|(id, name)| {
        json!({"type": "toolCall", "id": id, "name": name, "arguments": {"path": "a"}})
    }));
    json!({
        "role": "assistant", "content": content, "api": "anthropic-messages",
        "provider": "anthropic", "model": "test", "usage": usage(),
        "stopReason": stop, "timestamp": 1,
    })
}

fn result_value(call: &str, name: &str) -> Value {
    json!({
        "role": "toolResult", "toolCallId": call, "toolName": name,
        "content": [{"type": "text", "text": format!("output of {call}")}],
        "isError": false, "timestamp": 1,
    })
}

fn message_entry(id: &str, parent: Option<&str>, message: Value) -> Value {
    json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2025-01-01T00:00:00Z", "message": message})
}

fn user(id: &str, parent: Option<&str>) -> Value {
    message_entry(
        id,
        parent,
        json!({"role": "user", "content": "go", "timestamp": 1}),
    )
}

fn assistant(id: &str, parent: &str, stop: &str, calls: &[(&str, &str)]) -> Value {
    message_entry(id, Some(parent), assistant_value(stop, calls))
}

fn result(id: &str, parent: &str, call: &str, name: &str) -> Value {
    message_entry(id, Some(parent), result_value(call, name))
}

fn system(id: &str, parent: &str) -> Value {
    message_entry(
        id,
        Some(parent),
        json!({"role": "system", "content": "rules", "timestamp": 1}),
    )
}

fn other(id: &str, parent: &str, kind: &str, members: Value) -> Value {
    let mut value =
        json!({"type": kind, "id": id, "parentId": parent, "timestamp": "2025-01-01T00:00:00Z"});
    if let (Some(object), Value::Object(members)) = (value.as_object_mut(), members) {
        object.extend(members);
    }
    value
}

fn entries(values: Vec<Value>) -> Vec<SessionEntry> {
    values.into_iter().map(entry).collect()
}

fn call(entry_id: &str, id: &str, name: &str) -> (String, String, String) {
    (entry_id.to_owned(), id.to_owned(), name.to_owned())
}

fn triples(calls: &[UnansweredToolCall]) -> Vec<(String, String, String)> {
    calls
        .iter()
        .map(|call| {
            (
                call.entry_id.clone(),
                call.tool_call_id.clone(),
                call.tool_name.clone(),
            )
        })
        .collect()
}

/// The request the agent builds from the leaf's context, with ids rewritten
/// by `normalize` when given.
fn request(
    entries: &[SessionEntry],
    leaf: LeafSelector<'_>,
    normalize: Option<&dyn Fn(&str) -> String>,
) -> Vec<Message> {
    let context = build_session_context(entries, leaf);
    let live: Vec<_> = context
        .messages
        .iter()
        .map(AgentMessage::to_agent)
        .collect();
    let messages = convert_to_llm(&live);
    match normalize {
        Some(normalize) => {
            let rewrite = |id: &str, _: &_, _: &_| normalize(id);
            transform_messages(&messages, &faux_model("faux-1"), Some(&rewrite))
        }
        None => transform_messages(&messages, &faux_model("faux-1"), None),
    }
}

fn is_synthetic(message: &Message) -> bool {
    matches!(message, Message::ToolResult(result) if result.is_error
        && result.details.is_none()
        && matches!(result.content.as_slice(), [UserContentBlock::Text(text)] if text.text == SYNTHETIC))
}

/// The `(id, name)` of each synthetic result the request holds.
fn synthetic_results(request: &[Message]) -> Vec<(String, String)> {
    request
        .iter()
        .filter(|message| is_synthetic(message))
        .filter_map(|message| match message {
            Message::ToolResult(result) => {
                Some((result.tool_call_id.clone(), result.tool_name.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The projection agrees with the request built from the same leaf, with
/// stored ids and with a one-to-one rewrite.
fn assert_agrees(entries: &[SessionEntry], leaf: LeafSelector<'_>, context: &str) {
    let projected: Vec<(String, String)> = build_unanswered_tool_calls(entries, leaf)
        .into_iter()
        .map(|call| (call.tool_call_id, call.tool_name))
        .collect();
    assert_eq!(
        projected,
        synthetic_results(&request(entries, leaf, None)),
        "{context}"
    );
    let prefix = |id: &str| format!("n-{id}");
    let rewritten: Vec<(String, String)> =
        synthetic_results(&request(entries, leaf, Some(&prefix)))
            .into_iter()
            .map(|(id, name)| (id.strip_prefix("n-").unwrap_or(&id).to_owned(), name))
            .collect();
    assert_eq!(projected, rewritten, "{context}, rewritten ids");
}

/// The hand-derived answer, and agreement with the request.
fn check(values: Vec<Value>, leaf: LeafSelector<'_>, expected: &[(String, String, String)]) {
    let entries = entries(values);
    assert_eq!(
        triples(&build_unanswered_tool_calls(&entries, leaf)),
        expected,
        "leaf {leaf:?}"
    );
    assert_agrees(&entries, leaf, &format!("leaf {leaf:?}"));
}

#[test]
fn a_call_without_a_result_is_unanswered() {
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
        ],
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            result("r1", "a1", "c1", "read"),
            assistant("a2", "r1", "stop", &[]),
        ],
        LeafSelector::Last,
        &[],
    );
}

/// A user message closes the calls before it; a result after it no longer
/// answers them, and Pi keeps that result in the request as it is.
#[test]
fn a_result_after_a_user_message_does_not_answer() {
    let values = vec![
        user("u1", None),
        assistant("a1", "u1", "toolUse", &[("c1", "bash")]),
        user("u2", Some("a1")),
        result("r1", "u2", "c1", "bash"),
    ];
    let request = request(&entries(values.clone()), LeafSelector::Last, None);
    assert!(
        matches!(request.last(), Some(Message::ToolResult(result)) if !is_synthetic(&Message::ToolResult(result.clone())))
    );
    check(values, LeafSelector::Last, &[call("a1", "c1", "bash")]);
}

/// A later assistant message closes the calls too, with or without calls of
/// its own.
#[test]
fn a_later_assistant_message_closes_calls() {
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            assistant("a2", "a1", "stop", &[]),
            result("r1", "a2", "c1", "read"),
        ],
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            assistant("a2", "a1", "toolUse", &[("c2", "read")]),
            result("r1", "a2", "c1", "read"),
        ],
        LeafSelector::Last,
        &[call("a1", "c1", "read"), call("a2", "c2", "read")],
    );
}

/// Ids are matched within one assistant message's calls: a result from an
/// earlier turn with the same id answers nothing later.
#[test]
fn duplicate_ids_across_turns_are_separate_calls() {
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("x", "read")]),
            result("r1", "a1", "x", "read"),
            user("u2", Some("r1")),
            assistant("a2", "u2", "toolUse", &[("x", "read")]),
        ],
        LeafSelector::Last,
        &[call("a2", "x", "read")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("x", "read")]),
            user("u2", Some("a1")),
            assistant("a2", "u2", "toolUse", &[("x", "read")]),
            result("r1", "a2", "x", "read"),
        ],
        LeafSelector::Last,
        &[call("a1", "x", "read")],
    );
    // Within one message a repeated id is answered by one result, and
    // otherwise listed once per block, as Pi adds one result per block.
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("x", "read"), ("x", "grep")]),
        ],
        LeafSelector::Last,
        &[call("a1", "x", "read"), call("a1", "x", "grep")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("x", "read"), ("x", "grep")]),
            result("r1", "a1", "x", "read"),
        ],
        LeafSelector::Last,
        &[],
    );
}

/// Parallel calls answered in any order leave only the missing ones, in
/// call order.
#[test]
fn partly_answered_parallel_calls() {
    check(
        vec![
            user("u1", None),
            assistant(
                "a1",
                "u1",
                "toolUse",
                &[("c1", "read"), ("c2", "bash"), ("c3", "grep")],
            ),
            result("r2", "a1", "c2", "bash"),
        ],
        LeafSelector::Last,
        &[call("a1", "c1", "read"), call("a1", "c3", "grep")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read"), ("c2", "bash")]),
            result("r2", "a1", "c2", "bash"),
            result("r1", "r2", "c1", "read"),
        ],
        LeafSelector::Last,
        &[],
    );
}

/// Failed assistant messages are dropped from the request with their calls,
/// and still close the calls before them.
#[test]
fn errored_and_aborted_messages_drop_their_calls() {
    for stop in ["error", "aborted"] {
        check(
            vec![
                user("u1", None),
                assistant("a1", "u1", stop, &[("c1", "bash")]),
            ],
            LeafSelector::Last,
            &[],
        );
        check(
            vec![
                user("u1", None),
                assistant("a1", "u1", "toolUse", &[("c1", "bash")]),
                assistant("a2", "a1", stop, &[("c2", "bash")]),
                result("r1", "a2", "c1", "bash"),
            ],
            LeafSelector::Last,
            &[call("a1", "c1", "bash")],
        );
    }
    // `length` and `deferred` are not failures.
    for stop in ["length", "deferred", "stop"] {
        check(
            vec![
                user("u1", None),
                assistant("a1", "u1", stop, &[("c1", "bash")]),
            ],
            LeafSelector::Last,
            &[call("a1", "c1", "bash")],
        );
    }
}

/// System messages and excluded shell runs do not close calls; other
/// non-model messages read as user messages and do.
#[test]
fn which_messages_close_calls() {
    let base = || {
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
        ]
    };
    let with = |between: Value| {
        let mut values = base();
        values.push(between);
        values.push(result("r1", "m", "c1", "read"));
        values
    };
    let bash = |exclude: bool| {
        message_entry(
            "m",
            Some("a1"),
            json!({"role": "bashExecution", "command": "ls", "output": "", "exitCode": 0,
                "cancelled": false, "truncated": false, "timestamp": 1, "excludeFromContext": exclude}),
        )
    };
    check(with(system("m", "a1")), LeafSelector::Last, &[]);
    check(with(bash(true)), LeafSelector::Last, &[]);
    check(
        with(bash(false)),
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    check(
        with(other(
            "m",
            "a1",
            "custom_message",
            json!({"customType": "note", "content": "hi", "display": true}),
        )),
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    // A state entry contributes no message.
    check(
        with(other(
            "m",
            "a1",
            "thinking_level_change",
            json!({"thinkingLevel": "high"}),
        )),
        LeafSelector::Last,
        &[],
    );
}

/// Context edits act on the projection: an omitted result leaves its call
/// unanswered, an omitted or rewritten assistant message takes its calls
/// with it.
#[test]
fn context_edits_apply() {
    let base = || {
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            result("r1", "a1", "c1", "read"),
        ]
    };
    let edited = |target: &str, replacement: Value| {
        let mut values = base();
        values.push(other(
            "e1",
            "r1",
            "context_edit",
            json!({"targetId": target, "replacement": replacement}),
        ));
        values
    };
    check(
        edited("r1", Value::Null),
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    check(edited("a1", Value::Null), LeafSelector::Last, &[]);
    check(
        edited("a1", json!({"content": "summarized"})),
        LeafSelector::Last,
        &[],
    );
    // A replaced result still answers.
    check(
        edited("r1", json!({"content": "short"})),
        LeafSelector::Last,
        &[],
    );
    // An edit on another branch does not apply.
    let mut values = base();
    values.push(other(
        "e1",
        "a1",
        "context_edit",
        json!({"targetId": "r1", "replacement": null}),
    ));
    check(values.clone(), LeafSelector::Id("r1"), &[]);
    check(values, LeafSelector::Id("e1"), &[call("a1", "c1", "read")]);
}

/// Only calls from the compaction's kept boundary on are in context; an
/// older compaction retained in the kept range contributes nothing.
#[test]
fn compaction_keeps_calls_from_its_boundary() {
    let path = |first_kept: &str| {
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            user("u2", Some("a1")),
            assistant("a2", "u2", "toolUse", &[("c2", "bash")]),
            other(
                "k1",
                "a2",
                "compaction",
                json!({"summary": "s", "firstKeptEntryId": first_kept, "tokensBefore": 9}),
            ),
        ]
    };
    check(
        path("u1"),
        LeafSelector::Last,
        &[call("a1", "c1", "read"), call("a2", "c2", "bash")],
    );
    check(path("u2"), LeafSelector::Last, &[call("a2", "c2", "bash")]);
    check(path("k1"), LeafSelector::Last, &[]);
    // The kept boundary inside a call's results: the result is kept, its
    // call is not, and nothing is unanswered.
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            result("r1", "a1", "c1", "read"),
            other(
                "k1",
                "r1",
                "compaction",
                json!({"summary": "s", "firstKeptEntryId": "r1", "tokensBefore": 9}),
            ),
        ],
        LeafSelector::Last,
        &[],
    );
    // The latest compaction keeps `a2` and, inside its kept range, the
    // older compaction, which contributes nothing; `a3` closes `c2`.
    let mut values = path("u2");
    values.push(other(
        "k2",
        "k1",
        "compaction",
        json!({"summary": "t", "firstKeptEntryId": "a2", "tokensBefore": 9}),
    ));
    values.push(assistant("a3", "k2", "toolUse", &[("c3", "grep")]));
    check(
        values,
        LeafSelector::Last,
        &[call("a2", "c2", "bash"), call("a3", "c3", "grep")],
    );
}

/// Each leaf answers for its own branch; the empty leaf has none.
#[test]
fn branches_answer_for_their_own_path() {
    let values = vec![
        user("u1", None),
        assistant("a1", "u1", "toolUse", &[("c1", "read")]),
        result("r1", "a1", "c1", "read"),
        assistant("b1", "u1", "toolUse", &[("c1", "read")]),
        other(
            "s1",
            "u1",
            "branch_summary",
            json!({"summary": "left", "fromId": "b1"}),
        ),
    ];
    check(values.clone(), LeafSelector::Id("r1"), &[]);
    check(
        values.clone(),
        LeafSelector::Id("b1"),
        &[call("b1", "c1", "read")],
    );
    check(
        values.clone(),
        LeafSelector::Id("a1"),
        &[call("a1", "c1", "read")],
    );
    check(values.clone(), LeafSelector::Last, &[]);
    check(values.clone(), LeafSelector::Empty, &[]);
    // An unknown leaf is the last entry, as in Pi.
    check(values, LeafSelector::Id("missing"), &[]);
}

/// Messages the agent cannot read are dropped from the request: their calls
/// are not listed, they close nothing, and an unreadable result answers
/// nothing.
#[test]
fn malformed_messages() {
    let mut no_usage = assistant_value("toolUse", &[("c2", "read")]);
    if let Some(object) = no_usage.as_object_mut() {
        object.remove("usage");
    }
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            message_entry("m", Some("a1"), no_usage),
            result("r1", "m", "c1", "read"),
        ],
        LeafSelector::Last,
        &[],
    );
    let mut bad_block = assistant_value("toolUse", &[]);
    if let Some(content) = bad_block.get_mut("content").and_then(Value::as_array_mut) {
        content.push(json!({"type": "toolCall", "id": 5, "name": "read"}));
    }
    check(
        vec![user("u1", None), message_entry("a1", Some("u1"), bad_block)],
        LeafSelector::Last,
        &[],
    );
    let mut no_id = result_value("c1", "read");
    if let Some(object) = no_id.as_object_mut() {
        object.remove("toolCallId");
    }
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            message_entry("r1", Some("a1"), no_id),
        ],
        LeafSelector::Last,
        &[call("a1", "c1", "read")],
    );
    check(
        vec![
            user("u1", None),
            assistant("a1", "u1", "toolUse", &[("c1", "read")]),
            message_entry("m", Some("a1"), json!("not an object")),
            message_entry("n", Some("m"), json!({"role": "future"})),
            result("r1", "n", "c1", "read"),
        ],
        LeafSelector::Last,
        &[],
    );
}

fn tool_session(dir: &TempDir) -> std::path::PathBuf {
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    let append = |session: &mut SessionManager, value: Value| {
        session.append_message(message(value)).expect("append")
    };
    let first = session.append_message(user_msg("start")).expect("append");
    let parallel = append(
        &mut session,
        assistant_value("toolUse", &[("c1", "read"), ("c2", "bash")]),
    );
    append(&mut session, result_value("c2", "bash"));
    append(
        &mut session,
        json!({"role": "system", "content": "rules", "timestamp": 1}),
    );
    append(&mut session, result_value("c1", "read"));
    append(&mut session, assistant_value("toolUse", &[("c1", "read")]));
    session.append_message(user_msg("stop")).expect("append");
    append(&mut session, result_value("c1", "read"));
    append(&mut session, assistant_value("aborted", &[("c3", "bash")]));
    append(&mut session, assistant_value("toolUse", &[("c4", "grep")]));
    let omitted = append(&mut session, result_value("c4", "grep"));
    session
        .append_context_edit(&omitted, None)
        .expect("context edit");
    session
        .append_custom_message_entry("note", "hi".into(), true, None)
        .expect("custom");
    append(&mut session, assistant_value("toolUse", &[("c5", "read")]));
    session
        .append_context_edit(&omitted, Some(EditContent::Text("again".into())))
        .expect("context edit");
    session
        .append_compaction("sum", Some(&parallel), 9, None, None, None)
        .expect("compaction");
    append(&mut session, assistant_value("toolUse", &[("c6", "edit")]));
    session.branch(&first).expect("branch");
    append(&mut session, assistant_value("toolUse", &[("c7", "write")]));
    session.session_file().expect("file").to_path_buf()
}

fn parsed(text: &str) -> Vec<SessionEntry> {
    parse_session_entries(text)
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            _ => None,
        })
        .collect()
}

/// The written session has the calls the cases above expect, on both of
/// its branches.
#[test]
fn a_written_session_with_tool_calls() {
    let dir = TempDir::new("unanswered-written");
    let file = tool_session(&dir);
    let entries = parsed(&fs::read_to_string(&file).expect("read"));
    let names = |leaf| -> Vec<String> {
        build_unanswered_tool_calls(&entries, leaf)
            .into_iter()
            .map(|call| call.tool_call_id)
            .collect()
    };
    let main_tip = entries
        .iter()
        .rev()
        .nth(1)
        .map(|entry| entry.id().to_owned())
        .expect("entries");
    // c1 again (closed by `stop`), c5, then c6 at the tail; c4's result is
    // replaced after its omission and answers again.
    assert_eq!(names(LeafSelector::Id(&main_tip)), ["c1", "c5", "c6"]);
    assert_eq!(names(LeafSelector::Last), ["c7"]);
    for entry in &entries {
        assert_agrees(&entries, LeafSelector::Id(entry.id()), entry.id());
    }
}

/// Every prefix of a session file with tool calls, as a writer killed
/// mid-line leaves one, yields the calls the request built from it closes,
/// for the last leaf at every byte and all leaves at each line boundary.
/// The request pairs each call it keeps with exactly one result before the
/// next user or assistant message. Parsing only: the loader's repair is covered
/// by `robustness::every_truncation_opens_and_repairs`.
#[test]
fn every_truncation_agrees_with_the_request() {
    let dir = TempDir::new("unanswered-truncate");
    let file = tool_session(&dir);
    let bytes = fs::read(&file).expect("read");
    let mut leaves_checked = 0;
    for cut in 0..=bytes.len() {
        let text = String::from_utf8_lossy(&bytes[..cut]);
        let entries = parsed(&text);
        let at_line_end = cut == bytes.len() || bytes.get(cut) == Some(&b'\n');
        let mut leaves = vec![LeafSelector::Last];
        if at_line_end {
            leaves.extend(entries.iter().map(|entry| LeafSelector::Id(entry.id())));
        }
        for leaf in leaves {
            assert_agrees(&entries, leaf, &format!("cut at {cut}, leaf {leaf:?}"));
            assert_paired(&request(&entries, leaf, None), cut);
            leaves_checked += 1;
        }
    }
    assert!(leaves_checked > bytes.len());
}

/// Each assistant message's calls are followed by exactly one result per
/// call, in any order, before any other message but a system message. A
/// stored result after a user message stays in the request unpaired, as Pi
/// leaves it; a synthetic result is never unpaired.
fn assert_paired(request: &[Message], cut: usize) {
    let mut index = 0;
    while let Some(message) = request.get(index) {
        index += 1;
        let Message::Assistant(assistant) = message else {
            assert!(
                !is_synthetic(message),
                "cut at {cut}: unpaired synthetic result at {index}"
            );
            continue;
        };
        let mut calls: Vec<&str> = assistant
            .tool_calls()
            .map(|call| call.id.as_str())
            .collect();
        let mut results = Vec::new();
        while let Some(next) = request.get(index) {
            match next {
                Message::ToolResult(result) if results.len() < calls.len() => {
                    results.push(result.tool_call_id.as_str());
                }
                Message::System(_) => {}
                _ => break,
            }
            index += 1;
        }
        calls.sort_unstable();
        results.sort_unstable();
        assert_eq!(calls, results, "cut at {cut}");
    }
}
