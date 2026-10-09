//! Payloads nested 10,000 and 1,000,000 containers deep, as an MCP tool
//! schema can be logged, read through the scan, restore, and request
//! derivation on a 256 KiB stack, so any recursion over the nesting fails on
//! every OS.

use bake_session::{
    PathPlatform, PlainAppendLog, PromptDecision, RestoreRefusal, SeedRejection, consumed_work,
    context_pressure, dismantle, fork_seed, goal_projection, json_text, parse_json,
    replay_requests, replay_restored_requests, restore_plain_log, restored_inbox, scan_log,
    session_title, subagent_catalog, subagent_identity, subagent_timing, system_prompt_commits,
    token_usage, tools_changed, turn_boundary, unfinished_work,
};
use serde_json::Value;

const STACK: usize = 256 * 1024;
const BUDGET: usize = 1 << 20;

/// An array or an object chain `depth` containers deep around `1`.
fn nested(depth: usize, object: bool) -> String {
    if object {
        format!("{}1{}", "{\"k\":".repeat(depth), "}".repeat(depth))
    } else {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }
}

/// One completed turn whose user message and header tool schema hold `deep`.
fn log(deep: &str) -> Vec<u8> {
    let rows = [
        r#"{"type":"session","version":3,"id":"deep","createdAt":5,"isSeeded":false,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"turn/start","seq":0,"time":100,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":101,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"user/message","seq":2,"time":102,"data":{{"id":"u1","role":"user","content":[{{"type":"text","text":"go"}}],"source":{{"kind":"user"}},"deep":{deep}}},"surfaceOp":"append"}}"#
        ),
        format!(
            r#"{{"type":"request/header","seq":3,"time":103,"data":{{"header":{{"config":{{"provider":"p","model":"m"}},"tools":[{{"name":"t","parameters":{deep}}}]}},"reason":"initial"}}}}"#
        ),
        r#"{"type":"assistant/message","seq":4,"time":104,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","source":{"kind":"model","provider":"p","model":"m"},"content":[{"type":"text","text":"ok"}]},"stream":[]},"surfaceOp":"append"}"#.to_owned(),
        r#"{"type":"step/end","seq":5,"time":105,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"turn/end","seq":6,"time":106,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
    ];
    let mut bytes = Vec::new();
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

fn on_small_stack(run: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(run)
        .expect("spawn")
        .join()
        .expect("the deep payload is read without overflowing the stack");
}

fn read_deep(depth: usize, object: bool) {
    on_small_stack(move || {
        let deep = nested(depth, object);
        let bytes = log(&deep);
        let scanned = scan_log(&bytes, PathPlatform::Posix, BUDGET).expect("scan");
        assert_eq!(scanned.rows().len(), 7);
        drop(scanned);

        let requests = replay_requests(&bytes, PathPlatform::Posix, BUDGET).expect("replay");
        assert_eq!(requests.len(), 1);
        let request = requests[0].to_json();
        let text = json_text(&request);
        dismantle(request);
        drop(requests);
        let expected = format!(
            r#"{{"provider":"p","model":"m","messages":[{{"id":"u1","role":"user","content":[{{"type":"text","text":"go"}}],"source":{{"kind":"user"}},"deep":{deep}}}],"toolHistory":{{"tools":[{{"name":"t","parameters":{deep}}}],"updates":[]}},"tools":[{{"name":"t","parameters":{deep}}}],"sessionId":"deep"}}"#
        );
        assert!(text == expected, "request text differs at depth {depth}");

        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");
        let messages = restored.messages();
        assert_eq!(messages.len(), 2);
        let message = json_text(&messages[0]);
        messages.into_iter().for_each(dismantle);
        assert!(message.ends_with(&format!(r#""deep":{deep}}}"#)));
        let header = restored.request_header().expect("header");
        assert!(json_text(&header).contains(&deep));
        dismantle(header);
        dismantle(restored.tool_history());
        drop(restored);

        let parsed = parse_json(&deep).expect("deep JSON");
        assert_eq!(json_text(&parsed), deep);
        dismantle(parsed);
    });
}

#[test]
fn arrays_ten_thousand_deep() {
    read_deep(10_000, false);
}

#[test]
fn objects_ten_thousand_deep() {
    read_deep(10_000, true);
}

#[test]
fn arrays_a_million_deep() {
    read_deep(1_000_000, false);
}

#[test]
fn objects_a_million_deep() {
    read_deep(1_000_000, true);
}

/// An image block holding `deep`, in the shape the restoration tables log.
fn image(deep: &str, offloaded: bool) -> String {
    let offloaded = if offloaded {
        r#","offloaded":true"#
    } else {
        ""
    };
    format!(
        r#"{{"type":"image","attachment":{{"attachmentId":"sha256:{}","mediaType":"image/png","bytes":1024,"width":2,"height":2}},"deep":{deep}{offloaded}}}"#,
        "a".repeat(64)
    )
}

/// One completed turn whose user message holds `content` and a deep member,
/// then an `image/offload` of that message's `indexes`.
fn offload_log(deep: &str, content: &str, indexes: &str) -> Vec<u8> {
    let rows = [
        r#"{"type":"session","version":3,"id":"deep","createdAt":5,"isSeeded":false,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"turn/start","seq":0,"time":100,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":101,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"user/message","seq":2,"time":102,"data":{{"id":"u1","role":"user","content":{content},"source":{{"kind":"user"}},"deep":{deep}}},"surfaceOp":"append"}}"#
        ),
        r#"{"type":"assistant/message","seq":3,"time":104,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","source":{"kind":"model","provider":"p","model":"m"},"content":[{"type":"text","text":"ok"}]},"stream":[]},"surfaceOp":"append"}"#.to_owned(),
        r#"{"type":"step/end","seq":4,"time":105,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"turn/end","seq":5,"time":106,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
        format!(
            r#"{{"type":"image/offload","seq":6,"time":107,"data":{{"targets":[{{"seq":2,"imageIndexes":{indexes}}}]}}}}"#
        ),
    ];
    let mut bytes = Vec::new();
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

/// The `image/offload` projection copies a deep message, replaces its deep
/// content, and discards partial copies when it rejects, all without
/// recursing.
fn offload_deep(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, false);
        let content = format!(
            r#"[{},{{"type":"tool-result","content":[{}]}}]"#,
            image(&deep, false),
            image(&deep, false)
        );
        let bytes = offload_log(&deep, &content, "[0,1]");
        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");
        let messages = restored.messages();
        let message = json_text(&messages[0]);
        messages.into_iter().for_each(dismantle);
        drop(restored);
        let projected = format!(
            r#"[{},{{"type":"tool-result","content":[{}]}}]"#,
            image(&deep, true),
            image(&deep, true)
        );
        assert!(
            message.contains(&format!(r#""content":{projected}"#)),
            "the projection differs at depth {depth}"
        );
        assert!(message.ends_with(&format!(r#""deep":{deep}}}"#)));

        let content = format!(
            r#"[{},{{"type":"tool-result","content":[{},{}]}}]"#,
            image(&deep, false),
            image(&deep, false),
            image(&deep, true)
        );
        let bytes = offload_log(&deep, &content, "[0,1,2]");
        let refusal = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect_err("refused");
        assert!(
            format!("{refusal:?}").contains("AlreadyOffloaded"),
            "{refusal:?}"
        );

        let content = format!("[{}]", image(&deep, false));
        let bytes = offload_log(&deep, &content, "[0,1]");
        let refusal = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect_err("refused");
        assert!(
            format!("{refusal:?}").contains("MissingIndex"),
            "{refusal:?}"
        );
    });
}

#[test]
fn offloads_ten_thousand_deep() {
    offload_deep(10_000);
}

#[test]
fn offloads_a_million_deep() {
    offload_deep(1_000_000);
}

/// A completed turn whose user message holds `deep` as a member and as a
/// `tool-result` chain `depth` blocks deep, a header whose tool schema is
/// `deep`, and two inbox splices that insert and then remove a message
/// holding `deep`.
fn projection_log(depth: usize, deep: &str) -> Vec<u8> {
    let chain = format!(
        "{}{}{}",
        r#"{"type":"tool-result","content":["#.repeat(depth),
        r#"{"type":"text","text":"x"}"#,
        "]}".repeat(depth)
    );
    let pending = |id: &str| format!(r#"{{"id":"{id}","role":"user","content":[],"deep":{deep}}}"#);
    let (first, second) = (pending("p1"), pending("p2"));
    let rows = [
        r#"{"type":"session","version":3,"id":"deep","createdAt":5,"isSeeded":false,"delegationDepth":0}"#.to_owned(),
        r#"{"type":"turn/start","seq":0,"time":100,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":101,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"user/message","seq":2,"time":102,"data":{{"id":"u1","role":"user","content":[{{"type":"text","text":"go"}},{chain}],"source":{{"kind":"user"}},"deep":{deep}}},"surfaceOp":"append"}}"#
        ),
        format!(
            r#"{{"type":"request/header","seq":3,"time":103,"data":{{"header":{{"config":{{"provider":"p","model":"m"}},"tools":[{{"name":"t","parameters":{deep}}}]}},"reason":"initial"}}}}"#
        ),
        r#"{"type":"assistant/message","seq":4,"time":104,"data":{"turn":1,"step":1,"message":{"id":"a1","role":"assistant","source":{"kind":"model","provider":"p","model":"m"},"content":[{"type":"text","text":"ok"}]},"stream":[]},"surfaceOp":"append"}"#.to_owned(),
        r#"{"type":"step/end","seq":5,"time":105,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"turn/end","seq":6,"time":106,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
        format!(
            r#"{{"type":"agent/inbox/spliced","seq":7,"time":107,"data":{{"target":"next-turn","start":0,"removedCount":0,"inserted":[{first},{second}]}}}}"#
        ),
        r#"{"type":"agent/inbox/spliced","seq":8,"time":108,"data":{"target":"next-turn","start":0,"removedCount":1,"inserted":[]}}"#.to_owned(),
    ];
    let mut bytes = Vec::new();
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

/// The public projections of a restored log copy, compare, walk, and drop
/// deep payloads without recursing.
fn project_deep(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        let bytes = projection_log(depth, &deep);
        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");

        let seed = fork_seed(&restored, None).expect("fork seed");
        assert_eq!(seed.events().len(), 9);
        assert!(json_text(&seed.events()[2]).contains(&deep));
        drop(seed.clone());
        drop(seed);

        let inbox = restored_inbox(&restored).expect("inbox");
        assert_eq!(inbox.next_turn.len(), 1);
        assert!(json_text(&inbox.next_turn[0]).contains(&deep));
        inbox.next_turn.into_iter().for_each(dismantle);
        let consumed = consumed_work(&restored).expect("consumed work");
        consumed.end.into_iter().for_each(dismantle);

        let pressure = context_pressure(&restored).expect("pressure");
        assert!(pressure.surface_tokens > 0);

        let tools = parse_json(&format!(r#"[{{"name":"t","parameters":{deep}}}]"#)).expect("tools");
        let Value::Array(tools) = tools else {
            unreachable!()
        };
        assert!(!tools_changed(&restored, &tools));
        tools.into_iter().for_each(dismantle);

        // Public results hold plain values, which the caller dismantles.
        let unfinished = unfinished_work(&restored);
        let inbox = unfinished.inbox.expect("unfinished inbox");
        inbox.next_turn.into_iter().for_each(dismantle);
        let boundary = turn_boundary(&restored).expect("boundary");
        dismantle(boundary.last_turn);
        drop(token_usage(&restored));
        drop(goal_projection(&restored));

        for request in replay_restored_requests(&restored).expect("replay") {
            dismantle(request.to_json());
        }
        drop(restored);
    });
}

#[test]
fn projections_ten_thousand_deep() {
    project_deep(10_000);
}

#[test]
fn projections_a_million_deep() {
    project_deep(1_000_000);
}

/// Rows after the header, one per line.
fn rows_log(rows: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in std::iter::once(HEADER).chain(rows.iter().map(String::as_str)) {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

const HEADER: &str = r#"{"type":"session","version":3,"id":"deep","createdAt":5,"isSeeded":false,"delegationDepth":0}"#;

/// An interrupted turn whose Assistant row's `step` is `deep` and whose
/// `tool-call` never gets a result: the closer scan copies that step before
/// Session construction refuses it at the row.
fn interrupted_deep_step(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, false);
        let bytes = rows_log(&[
            r#"{"type":"turn/start","seq":0,"time":100,"data":{"turn":1}}"#.to_owned(),
            format!(
                r#"{{"type":"assistant/message","seq":1,"time":101,"data":{{"turn":1,"step":{deep},"message":{{"id":"a1","role":"assistant","source":{{"kind":"model","provider":"p","model":"m"}},"content":[{{"type":"tool-call","id":"c","name":"t","arguments":"{{}}"}}]}},"stream":[]}},"surfaceOp":"append"}}"#
            ),
        ]);
        let refusal = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect_err("refused");
        assert!(
            matches!(
                refusal,
                RestoreRefusal::Restore {
                    seq: 1,
                    rejection: SeedRejection::Settlement
                }
            ),
            "{refusal:?}"
        );
    });
}

#[test]
fn interrupted_step_ten_thousand_deep() {
    interrupted_deep_step(10_000);
}

#[test]
fn interrupted_step_a_million_deep() {
    interrupted_deep_step(1_000_000);
}

/// A completed first turn whose `turn` value is `deep`, then an interrupted
/// turn whose `tool-call` holds deep arguments and gets no result, with a
/// deep title, read by the remaining public consumers of a restored log.
fn interrupted_deep_consumers(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        let bytes = rows_log(&[
            format!(r#"{{"type":"turn/start","seq":0,"time":100,"data":{{"turn":{deep}}}}}"#),
            r#"{"type":"turn/end","seq":1,"time":101,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
            r#"{"type":"turn/start","seq":2,"time":102,"data":{"turn":2}}"#.to_owned(),
            r#"{"type":"step/start","seq":3,"time":103,"data":{"turn":2,"step":1}}"#.to_owned(),
            format!(
                r#"{{"type":"user/message","seq":4,"time":104,"data":{{"id":"u1","role":"user","content":[{{"type":"text","text":"go"}}],"source":{{"kind":"user"}},"deep":{deep}}},"surfaceOp":"append"}}"#
            ),
            format!(
                r#"{{"type":"session/title","seq":5,"time":105,"data":{{"title":{deep},"messageSeqs":[4],"source":{{"kind":"fallback"}}}}}}"#
            ),
            format!(
                r#"{{"type":"assistant/message","seq":6,"time":106,"data":{{"turn":2,"step":1,"message":{{"id":"a1","role":"assistant","source":{{"kind":"model","provider":"p","model":"m"}},"content":[{{"type":"tool-call","id":"c","name":"t","arguments":{deep}}}]}},"stream":[]}},"surfaceOp":"append"}}"#
            ),
        ]);
        let appender = PlainAppendLog::open(&bytes, PathPlatform::Posix, BUDGET).expect("open");
        assert_eq!(appender.id(), "deep");
        drop(appender);

        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");
        assert_eq!(restored.closers().len(), 3);
        let unfinished = unfinished_work(&restored);
        assert_eq!(unfinished.tools.len(), 1);
        assert_eq!(unfinished.tools[0].call_id, "c");
        if let Ok(inbox) = unfinished.inbox {
            inbox.next_turn.into_iter().for_each(dismantle);
        }

        let title = session_title(&restored).expect("title");
        assert_eq!(json_text(&title), deep);
        dismantle(title);
        assert!(subagent_identity(&restored).is_none());
        assert!(!subagent_timing(&restored).descriptor_seen);
        assert!(subagent_catalog(&restored).expect("catalog").is_empty());
        let decision = PromptDecision {
            in_history: false,
            starts_series: false,
        };
        drop(system_prompt_commits(&restored, "prompt", decision));

        let messages = restored.messages();
        assert!(
            messages
                .iter()
                .any(|message| json_text(message).contains(&deep))
        );
        messages.into_iter().for_each(dismantle);
        drop(restored);
    });
}

#[test]
fn interrupted_consumers_ten_thousand_deep() {
    interrupted_deep_consumers(10_000);
}

#[test]
fn interrupted_consumers_a_million_deep() {
    interrupted_deep_consumers(1_000_000);
}

#[test]
fn a_value_parsed_deep_compares_by_its_text() {
    let value: Value = parse_json("[[1]]").expect("json");
    assert_eq!(json_text(&value), "[[1]]");
}
