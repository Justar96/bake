//! Payloads nested 10,000 and 1,000,000 containers deep, as an MCP tool
//! schema can be logged, read through the scan, restore, and request
//! derivation on a 256 KiB stack, so any recursion over the nesting fails on
//! every OS.

use bake_session::{
    MigratedV2, PathPlatform, PlainAppendLog, PlainLogFile, PromptDecision, RestoreRefusal,
    SeedRejection, V1CodecRecovery, V1CodecVersion, consumed_work, context_pressure,
    decode_v0_v1_items, dismantle, fork_seed, goal_projection, json_text,
    migrate_released_generation, migrate_released_history, migrate_released_zstd_generation,
    parse_json, released_zstd_plaintext, replay_requests, replay_restored_requests,
    restore_migrated, restore_plain_log, restored_inbox, scan_log, session_title, subagent_catalog,
    subagent_identity, subagent_timing, system_prompt_commits, token_usage, tools_changed,
    turn_boundary, unfinished_work,
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
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn arrays_a_million_deep() {
    read_deep(1_000_000, false);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
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
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
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
        drop(inbox.clone());
        drop(inbox);
        let consumed = consumed_work(&restored).expect("consumed work");
        drop(consumed.clone());
        drop(consumed);

        let pressure = context_pressure(&restored).expect("pressure");
        assert!(pressure.surface_tokens > 0);

        let tools = parse_json(&format!(r#"[{{"name":"t","parameters":{deep}}}]"#)).expect("tools");
        let Value::Array(tools) = tools else {
            unreachable!()
        };
        assert!(!tools_changed(&restored, &tools));
        tools.into_iter().for_each(dismantle);

        // Public results drop, clone, and compare without recursing.
        let unfinished = unfinished_work(&restored);
        assert!(unfinished.inbox.is_ok());
        assert!(unfinished.clone() == unfinished);
        drop(unfinished);
        let boundary = turn_boundary(&restored).expect("boundary");
        assert!(boundary.clone() == boundary);
        drop(boundary);
        drop(token_usage(&restored));
        drop(goal_projection(&restored));

        for request in replay_restored_requests(&restored).expect("replay") {
            dismantle(request.to_json());
        }
        drop(restored);
    });
}

fn rows_bytes(rows: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

const DEEP_HEADER: &str = r#"{"type":"session","version":3,"id":"deep","createdAt":5,"isSeeded":false,"delegationDepth":0}"#;

/// A completed turn with one step, and an open compaction, whose `turn/end`
/// and `compaction/start` carry an extra member holding `deep`.
fn deep_closer_log(deep: &str) -> Vec<u8> {
    rows_bytes(&[
        DEEP_HEADER.to_owned(),
        r#"{"type":"turn/start","seq":0,"time":100,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":101,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"step/end","seq":2,"time":102,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"turn/end","seq":3,"time":103,"data":{{"turn":1,"reason":{{"kind":"completed"}},"x":{deep}}}}}"#
        ),
        format!(
            r#"{{"type":"compaction/start","seq":4,"time":104,"data":{{"compactionId":"c","turn":null,"x":{deep}}}}}"#
        ),
    ])
}

/// A turn whose `turn` is an object holding `deep`, as boundary-cases'
/// `object-turn-copied` logs one.
fn deep_turn_log(deep: &str) -> Vec<u8> {
    rows_bytes(&[
        DEEP_HEADER.to_owned(),
        format!(r#"{{"type":"turn/start","seq":0,"time":100,"data":{{"turn":{{"n":{deep}}}}}}}"#),
        r#"{"type":"turn/end","seq":1,"time":101,"data":{"reason":{"kind":"completed"}}}"#
            .to_owned(),
    ])
}

/// `ConsumedWork::end`, `OpenCompaction::data`, and
/// `TurnBoundaryState::last_turn` hold a deep row's data; each result
/// clones, compares, formats, and drops without recursing. The deepened
/// sweep's default sample need not reach these positions, so they are
/// pinned here.
fn closers_deep(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        let bytes = deep_closer_log(&deep);
        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");

        let consumed = consumed_work(&restored).expect("consumed work");
        let end = consumed.end.as_ref().expect("an accounting turn end");
        assert!(json_text(end).contains(&deep));
        assert!(consumed.clone() == consumed);
        std::hint::black_box(format!("{consumed:?}"));
        drop(consumed);

        let unfinished = unfinished_work(&restored);
        let compaction = unfinished.compaction.as_ref().expect("an open compaction");
        assert!(json_text(&compaction.data).contains(&deep));
        assert!(unfinished.clone() == unfinished);
        std::hint::black_box(format!("{unfinished:?}"));
        drop(unfinished);
        drop(restored);

        let bytes = deep_turn_log(&deep);
        let restored = restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).expect("restore");
        let boundary = turn_boundary(&restored).expect("boundary");
        assert!(json_text(&boundary.last_turn).contains(&deep));
        assert!(boundary.clone() == boundary);
        std::hint::black_box(format!("{boundary:?}"));
        drop(boundary);
        drop(restored);
    });
}

#[test]
fn closers_ten_thousand_deep() {
    closers_deep(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn closers_a_million_deep() {
    closers_deep(1_000_000);
}

#[test]
fn projections_ten_thousand_deep() {
    project_deep(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
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
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
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
        drop(unfinished);

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
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn interrupted_consumers_a_million_deep() {
    interrupted_deep_consumers(1_000_000);
}

#[test]
fn a_value_parsed_deep_compares_by_its_text() {
    let value: Value = parse_json("[[1]]").expect("json");
    assert_eq!(json_text(&value), "[[1]]");
}

/// A raw-block Zstd frame holding `content`, as a writer's frame decodes.
fn raw_frame(content: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    let mut blocks = content.chunks(131_072).peekable();
    if blocks.peek().is_none() {
        frame.extend([1, 0, 0]);
    }
    while let Some(block) = blocks.next() {
        let header = (u32::try_from(block.len()).expect("a block length") << 3)
            | u32::from(blocks.peek().is_none());
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(block);
    }
    frame
}

/// The header record and rows of a completed v0, v1, or v2 turn whose
/// request header holds `deep` as an MCP tool schema and whose tool result
/// holds it as `meta`, which every older generation keeps opaque.
fn released_log(version: u64, deep: &str) -> (String, String) {
    let header = if version == 2 {
        r#"{"type":"session","version":2,"id":"deep","createdAt":1000,"isSeeded":false,"delegationDepth":0}"#.to_owned()
    } else {
        format!(
            r#"{{"type":"session","version":{version},"id":"deep","createdAt":1000,"delegationDepth":0}}"#
        )
    };
    let stream = if version == 2 { r#","stream":[]"# } else { "" };
    let rows = [
        r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"request/header","seq":2,"time":1003,"data":{{"header":{{"config":{{"provider":"p","model":"m"}},"system":"sys","tools":[{{"name":"echo","description":"d","parameters":{{"type":"object","properties":{{"x":{deep}}}}}}}]}},"reason":"initial"}}}}"#
        ),
        r#"{"type":"user/message","seq":3,"time":1004,"data":{"role":"user","id":"u1","content":[{"type":"text","text":"hi"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#.to_owned(),
        format!(
            r#"{{"type":"assistant/message","seq":4,"time":1005,"data":{{"turn":1,"step":1,"message":{{"id":"a1","role":"assistant","content":[{{"type":"tool-call","id":"c1","name":"echo","arguments":"{{}}"}}],"source":{{"kind":"model","provider":"p","model":"m"}}}}{stream}}},"surfaceOp":"append"}}"#
        ),
        r#"{"type":"tool/call","seq":5,"time":1006,"data":{"turn":1,"step":1,"callId":"c1","name":"echo","arguments":"{}"}}"#.to_owned(),
        format!(
            r#"{{"type":"tool/result","seq":6,"time":1007,"data":{{"turn":1,"step":1,"message":{{"id":"r1","role":"user","content":[{{"type":"tool-result","toolCallId":"c1","content":[{{"type":"text","text":"out"}}]}}],"source":{{"kind":"tool","callId":"c1"}}}},"meta":{deep}}},"surfaceOp":"append"}}"#
        ),
        r#"{"type":"step/end","seq":7,"time":1008,"data":{"turn":1,"step":1}}"#.to_owned(),
        r#"{"type":"turn/end","seq":8,"time":1009,"data":{"turn":1,"reason":{"kind":"completed"}}}"#.to_owned(),
    ];
    let body = rows.iter().map(|row| format!("{row}\n")).collect();
    (format!("{header}\n"), body)
}

fn plain_released_log(version: u64, deep: &str) -> Vec<u8> {
    let (header, body) = released_log(version, deep);
    format!("{header}{body}").into_bytes()
}

fn zstd_released_log(version: u64, deep: &str) -> Vec<u8> {
    let (header, body) = released_log(version, deep);
    let mut bytes = raw_frame(header.as_bytes());
    bytes.extend(raw_frame(body.as_bytes()));
    bytes
}

/// The file name a writer of `version` gives a log.
fn released_name(version: u64) -> &'static str {
    match version {
        0 => "session.jsonl",
        1 => "session.v1.jsonl",
        _ => "session.v2.jsonl",
    }
}

/// The migrated Session restores with `deep` in its header tool schema and
/// in the tool result's `meta`; everything it holds is dismantled.
fn check_migrated(migrated: MigratedV2, deep: &str) {
    let restored = restore_migrated(&migrated, PathPlatform::Posix, BUDGET).expect("restore");
    assert!(
        migrated
            .events
            .iter()
            .filter(|event| json_text(event).contains(deep))
            .count()
            == 2
    );
    drop(migrated);
    let header = restored.request_header().expect("header");
    assert!(json_text(&header).contains(deep));
    dismantle(header);
    let messages = restored.messages();
    assert_eq!(messages.len(), 4);
    messages.into_iter().for_each(dismantle);
    drop(restored);
}

/// v0, v1, and v2 logs holding `deep` migrate, plain and Zstd, through
/// every public migration path, and the parsed rows' history reads to v3.
fn migrate_deep(depth: usize, object: bool) {
    on_small_stack(move || {
        let deep = nested(depth, object);
        for version in [0, 1, 2] {
            let plain = plain_released_log(version, &deep);
            let migrated = migrate_released_generation(&plain, version, BUDGET)
                .unwrap_or_else(|refusal| panic!("v{version}: {refusal:?}"));
            check_migrated(migrated, &deep);

            let zstd = zstd_released_log(version, &deep);
            let decoded = released_zstd_plaintext(&zstd, usize::MAX).expect("Zstd plaintext");
            assert!(decoded.plaintext == plain);
            let migrated = migrate_released_zstd_generation(&decoded, version, BUDGET)
                .unwrap_or_else(|refusal| panic!("v{version} Zstd: {refusal:?}"));
            check_migrated(migrated, &deep);

            if version < 2 {
                let (header, body) = released_log(version, &deep);
                let header = parse_json(header.trim_end()).expect("header");
                let rows: Vec<Value> = body
                    .lines()
                    .map(|row| parse_json(row).expect("row"))
                    .collect();
                let codec = if version == 0 {
                    V1CodecVersion::V0
                } else {
                    V1CodecVersion::V1
                };
                let decoded = decode_v0_v1_items(
                    &header,
                    &rows,
                    codec,
                    V1CodecRecovery::Strict,
                    PathPlatform::Posix,
                    BUDGET,
                )
                .expect("decode");
                dismantle(header);
                rows.into_iter().for_each(dismantle);
                let migrated = migrate_released_history(&decoded).expect("history");
                drop(decoded);
                check_migrated(migrated, &deep);
            }
        }
    });
}

#[test]
fn migrations_ten_thousand_deep() {
    migrate_deep(10_000, false);
    migrate_deep(10_000, true);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn migrations_a_million_deep() {
    migrate_deep(1_000_000, true);
}

/// A directory this test owns, removed on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bake-session-deep-{}-{name}-{count}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A write `open` migrates a plain v0, v1, or v2 log holding `deep` and
/// publishes a v3 log that holds it.
fn open_deep(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        for version in [0, 1, 2] {
            let scratch = Scratch::new("open");
            let dir = scratch.0.join("_no-cwd").join("deep");
            std::fs::create_dir_all(&dir).expect("session directory");
            std::fs::write(
                dir.join(released_name(version)),
                plain_released_log(version, &deep),
            )
            .expect("seed");
            let file = PlainLogFile::open(&scratch.0, "deep", BUDGET)
                .unwrap_or_else(|refusal| panic!("v{version}: {refusal:?}"));
            drop(file);
            let written = std::fs::read_to_string(dir.join("session.v3.jsonl")).expect("v3 log");
            assert_eq!(written.matches(&deep).count(), 2, "v{version}");
        }
    });
}

#[test]
fn write_open_migrates_ten_thousand_deep() {
    open_deep(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn write_open_migrates_a_million_deep() {
    open_deep(1_000_000);
}

/// `content` blocks whose `tool-result` blocks nest `depth` levels around
/// one text block.
fn tool_result_chain(depth: usize) -> String {
    format!(
        r#"[{}{{"type":"text","text":"x"}}{}]"#,
        r#"{"type":"tool-result","toolCallId":"c","content":["#.repeat(depth),
        "]}".repeat(depth)
    )
}

/// The header of a v0, v1, or v2 log.
fn released_header(version: u64) -> String {
    if version == 2 {
        r#"{"type":"session","version":2,"id":"deep","createdAt":1000,"isSeeded":false,"delegationDepth":0}"#.to_owned()
    } else {
        format!(
            r#"{{"type":"session","version":{version},"id":"deep","createdAt":1000,"delegationDepth":0}}"#
        )
    }
}

/// A v0, v1, or v2 log whose user message has `content`, which the payload
/// and admission content checks walk.
fn user_content_log(version: u64, content: &str) -> Vec<u8> {
    let rows = [
        released_header(version),
        r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"user/message","seq":2,"time":1003,"data":{{"role":"user","id":"u1","content":{content},"source":{{"kind":"user"}}}},"surfaceOp":"append"}}"#
        ),
    ];
    rows.iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>()
        .into_bytes()
}

/// A v2 log whose assistant stream ends a block holding `block`, which
/// admission checks as one content block.
fn block_end_log(block: &str) -> Vec<u8> {
    let rows = [
        released_header(2),
        r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#.to_owned(),
        format!(
            r#"{{"type":"assistant/message","seq":2,"time":1003,"data":{{"turn":1,"step":1,"message":{{"id":"a1","role":"assistant","content":[{{"type":"text","text":"ok"}}],"source":{{"kind":"model","provider":"p","model":"m"}}}},"stream":[{{"type":"chunk","time":1003,"chunk":{{"type":"block-end","index":0,"block":{block}}}}}]}},"surfaceOp":"append"}}"#
        ),
    ];
    rows.iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>()
        .into_bytes()
}

/// The refusal text of migrating `log` from `version`.
fn refusal(log: &[u8], version: u64) -> String {
    let refusal = migrate_released_generation(log, version, BUDGET).expect_err("a refusal");
    format!("{refusal:?}")
}

/// Content nesting `tool-result` blocks `depth` levels migrates from every
/// older generation, and a refusal at the innermost block names its path.
fn migrate_tool_result_chain(depth: usize) {
    on_small_stack(move || {
        for version in [0, 1, 2] {
            let log = user_content_log(version, &tool_result_chain(depth));
            let migrated = migrate_released_generation(&log, version, BUDGET)
                .unwrap_or_else(|refusal| panic!("v{version}: {refusal:?}"));
            assert!(
                migrated
                    .events
                    .iter()
                    .any(|event| json_text(event).contains(r#"{"type":"text","text":"x"}"#)),
                "v{version}"
            );
        }
        let block = tool_result_chain(depth);
        let block = &block[1..block.len() - 1];
        let migrated = migrate_released_generation(&block_end_log(block), 2, BUDGET)
            .unwrap_or_else(|refusal| panic!("block-end: {refusal:?}"));
        drop(migrated);

        let refused = tool_result_chain(depth).replace(r#""text":"x""#, r#""text":7"#);
        let path = " content[0]".repeat(depth);
        assert!(
            refusal(&user_content_log(0, &refused), 0).ends_with(&format!(
                "user/message 2 content[0]{path} text must be a string\")"
            ))
        );
        let path = ".content[0]".repeat(depth);
        assert!(
            refusal(&user_content_log(2, &refused), 2).contains(&format!(
                "data.content[0]{path}: invalid message content kind \\\"text\\\""
            ))
        );
        let refused = &refused[1..refused.len() - 1];
        assert!(
            refusal(&block_end_log(refused), 2)
                .contains(&format!("stream[0].chunk.block{path}: invalid message"))
        );
    });
}

#[test]
fn tool_result_chains_ten_thousand_deep() {
    migrate_tool_result_chain(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn tool_result_chains_a_million_deep() {
    migrate_tool_result_chain(1_000_000);
}

/// Refusals inside nested `tool-result` content keep the recursive checks'
/// order and labels: a block before its nested content, its nested content
/// before its `isError`, and each label built from the full path.
#[test]
fn tool_result_chain_refusals_keep_their_order_and_labels() {
    let cases: [(&str, [&str; 3]); 4] = [
        (
            r#"[{"type":"text","text":"a"},{"type":"tool-result","toolCallId":"c","isError":1,"content":[{"type":"text","text":"b"},{"type":"tool-result","toolCallId":"c","content":[{"type":"text"}]}]}]"#,
            [
                r#"user/message 2 content[1] content[1] content[0] lacks required member \"text\""#,
                r#"user/message 2 content[1] content[1] content[0] lacks required member \"text\""#,
                r#"data.content[1].content[1].content[0]: invalid message content kind \"text\": SessionFormatError: user/message 0 content[0] lacks required member \"text\""#,
            ],
        ),
        (
            r#"[{"type":"text","text":"a"},{"type":"tool-result","toolCallId":"c","isError":1,"content":[{"type":"text","text":"b"},{"type":"tool-result","toolCallId":"c","content":[{"type":"text","text":"c"}]}]}]"#,
            [
                "user/message 2 content[1] isError must be a boolean",
                "user/message 2 content[1] isError must be a boolean",
                r#"data.content[1]: invalid message content kind \"tool-result\": SessionFormatError: user/message 0 content[0] isError must be a boolean"#,
            ],
        ),
        (
            r#"[{"type":"tool-result","toolCallId":"c","content":[{"type":"tool-result","toolCallId":"c","content":{}}]}]"#,
            [
                "user/message 2 content[0] content[0] content must be an array",
                "user/message 2 content[0] content[0] content must be an array",
                r#"data.content[0].content[0].content: invalid message content kind \"tool-result\": content must be an array"#,
            ],
        ),
        (
            r#"[{"type":"tool-result","toolCallId":"c","content":[{"type":"tool-result","toolCallId":"c","content":[7]}]}]"#,
            [
                "user/message 2 content[0] content[0] content[0] must be a JSON object",
                "user/message 2 content[0] content[0] content[0] must be a JSON object",
                "data.content[0].content[0].content[0] must be an object",
            ],
        ),
    ];
    for (content, expected) in cases {
        for (version, expected) in (0..).zip(expected) {
            let refusal = refusal(&user_content_log(version, content), version);
            assert!(
                refusal.ends_with(&format!("{expected}\")")),
                "v{version}: {refusal}"
            );
        }
    }
}

/// A v1 `assistant/chunk` whose raw `finish` chunk holds `deep` is copied
/// into the attempt's stream and dropped without recursing.
fn migrate_deep_raw_chunk(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        let log = format!(
            "{}\n{}\n{}\n{}\n",
            r#"{"type":"session","version":1,"id":"deep","createdAt":1000,"delegationDepth":0}"#,
            r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#,
            r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#,
            format_args!(
                r#"{{"type":"assistant/chunk","seq":2,"time":1003,"data":{{"turn":1,"step":1,"chunk":{{"type":"finish","x":{deep}}}}}}}"#
            ),
        );
        let migrated = migrate_released_generation(log.as_bytes(), 1, BUDGET)
            .unwrap_or_else(|refusal| panic!("{refusal:?}"));
        assert!(
            migrated
                .events
                .iter()
                .any(|event| json_text(event).contains(&deep))
        );
    });
}

#[test]
fn raw_chunks_ten_thousand_deep() {
    migrate_deep_raw_chunk(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn raw_chunks_a_million_deep() {
    migrate_deep_raw_chunk(1_000_000);
}

/// A v0 legacy `assistant/message` whose `content` is `deep` and whose
/// legacy source is not an object is refused, dropping the moved content
/// without recursing.
fn refuse_deep_legacy_content(depth: usize) {
    on_small_stack(move || {
        let deep = nested(depth, true);
        let log = format!(
            "{}\n{}\n{}\n{}\n",
            r#"{"type":"session","version":0,"id":"deep","createdAt":1000,"delegationDepth":0}"#,
            r#"{"type":"turn/start","seq":0,"time":1001,"data":{"turn":1}}"#,
            r#"{"type":"step/start","seq":1,"time":1002,"data":{"turn":1,"step":1}}"#,
            format_args!(
                r#"{{"type":"assistant/message","seq":2,"time":1003,"data":{{"turn":1,"step":1,"content":{deep},"provenance":1}}}}"#
            ),
        );
        let refusal = migrate_released_generation(log.as_bytes(), 0, BUDGET)
            .expect_err("a legacy source that is not an object");
        assert!(
            format!("{refusal:?}").contains("legacy source must be a JSON object"),
            "{refusal:?}"
        );
    });
}

#[test]
fn legacy_content_refusals_ten_thousand_deep() {
    refuse_deep_legacy_content(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn legacy_content_refusals_a_million_deep() {
    refuse_deep_legacy_content(1_000_000);
}

/// A released log of `version` with `rows` after a flat header.
fn released_rows_log(version: u64, rows: &[String]) -> String {
    let mut log = format!(
        r#"{{"type":"session","version":{version},"id":"s","createdAt":1,"delegationDepth":0}}"#
    );
    log.push('\n');
    for row in rows {
        log.push_str(row);
        log.push('\n');
    }
    log
}

/// The migration outcome of `log`, run on a small stack: `ok` or the refusal.
fn migration_outcome(log: String, version: u64) -> String {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(
            move || match migrate_released_generation(log.as_bytes(), version, BUDGET) {
                Ok(_) => "ok".to_owned(),
                Err(refusal) => format!("{refusal:?}"),
            },
        )
        .expect("spawn")
        .join()
        .expect("the deep payload is migrated without overflowing the stack")
}

/// A chunk whose deep `turn` has no `step` is refused at the attempt
/// without dropping a copy of the turn recursively, from v0 and v1.
fn refuse_deep_chunk_turn(depth: usize) {
    let deep = nested(depth, false);
    let rows = [format!(
        r#"{{"type":"assistant/chunk","seq":0,"time":1,"data":{{"turn":{deep},"chunk":{{"type":"text-delta","index":0,"text":"a"}}}}}}"#
    )];
    for (version, stage) in [(0, "v1-to-v2"), (1, "v1-to-v2-decoded/transformed")] {
        let outcome = migration_outcome(released_rows_log(version, &rows), version);
        assert_eq!(
            outcome,
            format!(r#"Limit("history/{stage}/undefined-member")"#)
        );
    }
}

#[test]
fn chunk_turn_refusals_ten_thousand_deep() {
    refuse_deep_chunk_turn(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn chunk_turn_refusals_a_million_deep() {
    refuse_deep_chunk_turn(1_000_000);
}

/// A replacement `surfaceOp` carrying a deep member is mapped and its
/// object dropped without recursing, from v0 and v1.
fn remap_deep_surface_operation(depth: usize) {
    let deep = nested(depth, false);
    let rows = [format!(
        r#"{{"type":"plan/mode","seq":0,"time":1,"data":{{"active":true}},"surfaceOp":{{"op":"replace","start":0,"end":0,"x":{deep}}}}}"#
    )];
    for version in [0, 1] {
        let outcome = migration_outcome(released_rows_log(version, &rows), version);
        assert!(
            outcome.ends_with("plan/mode has unexpected field surfaceOp\")"),
            "v{version}: {outcome}"
        );
    }
}

#[test]
fn surface_operations_ten_thousand_deep() {
    remap_deep_surface_operation(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn surface_operations_a_million_deep() {
    remap_deep_surface_operation(1_000_000);
}

/// A v1 message whose attempt stream holds a deep raw chunk is refused at a
/// buffered event, dropping the taken stream without recursing.
fn refuse_deep_message_stream(depth: usize) {
    let deep = nested(depth, false);
    let rows = [
        format!(
            r#"{{"type":"assistant/chunk","seq":0,"time":1,"data":{{"turn":1,"step":1,"chunk":{{"type":"finish","x":{deep}}}}}}}"#
        ),
        r#"{"type":"plan/mode","seq":1,"time":1,"data":{"active":true},"surfaceOp":"bad"}"#
            .to_owned(),
        r#"{"type":"assistant/message","seq":2,"time":1,"data":{"turn":1,"step":1},"sourceEventSeqs":[0]}"#
            .to_owned(),
    ];
    let outcome = migration_outcome(released_rows_log(1, &rows), 1);
    assert!(
        outcome.ends_with(r#"assistant/message 2 data lacks required member \"message\"")"#),
        "{outcome}"
    );
}

#[test]
fn message_stream_refusals_ten_thousand_deep() {
    refuse_deep_message_stream(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn message_stream_refusals_a_million_deep() {
    refuse_deep_message_stream(1_000_000);
}

/// A v1 `turn/start` with a deep `time` that closes an interrupted turn is
/// refused at the open attempt, dropping the generated `turn/end` without
/// recursing.
fn refuse_deep_interrupted_turn_time(depth: usize) {
    let deep = nested(depth, false);
    let rows = [
        r#"{"type":"turn/start","seq":0,"time":1,"data":{"turn":1}}"#.to_owned(),
        r#"{"type":"assistant/chunk","seq":1,"time":1,"data":{"turn":1,"chunk":{"type":"text-delta","index":0,"text":"a"}}}"#.to_owned(),
        r#"{"type":"agent/inbox/spliced","seq":2,"time":1,"data":{"target":"next-turn","inserted":[{}]}}"#.to_owned(),
        format!(r#"{{"type":"turn/start","seq":3,"time":{deep},"data":{{"turn":2}}}}"#),
    ];
    let outcome = migration_outcome(released_rows_log(1, &rows), 1);
    assert!(
        outcome.ends_with(r#"agent/inbox/spliced 2 data lacks required member \"start\"")"#),
        "{outcome}"
    );
}

#[test]
fn interrupted_turn_times_ten_thousand_deep() {
    refuse_deep_interrupted_turn_time(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn interrupted_turn_times_a_million_deep() {
    refuse_deep_interrupted_turn_time(1_000_000);
}

/// A v1 `turn/start` with a deep `turn` that does not close the open turn
/// is refused with the turn's text, written without recursing.
fn refuse_deep_unclosed_turn(depth: usize) {
    let deep = nested(depth, false);
    let rows = [
        r#"{"type":"turn/start","seq":0,"time":1,"data":{"turn":1}}"#.to_owned(),
        format!(r#"{{"type":"turn/start","seq":1,"time":1,"data":{{"turn":{deep}}}}}"#),
    ];
    let outcome = migration_outcome(released_rows_log(1, &rows), 1);
    assert!(
        outcome.ends_with("turn/start 1 turn must be a non-negative safe integer\")"),
        "{outcome}"
    );
}

#[test]
fn unclosed_turn_refusals_ten_thousand_deep() {
    refuse_deep_unclosed_turn(10_000);
}

#[test]
#[ignore = "million-level depth is slow and memory-heavy; run with --ignored. The 10,000-level twin on a 256 KiB stack already fails on any recursion."]
fn unclosed_turn_refusals_a_million_deep() {
    refuse_deep_unclosed_turn(1_000_000);
}
