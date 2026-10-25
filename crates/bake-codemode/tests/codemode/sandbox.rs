//! Pi v1.3.0's `packages/codemode/test/sandbox.test.ts`, run against the one
//! place Bake's VM runs: a run's thread. Pi runs each case on its worker,
//! inline, and remote backends; their transports are not ported. Cases
//! about the sandbox object (`registerTool`, `close`), worker files, and the
//! raw worker's malformed messages map to `Run` requests and to the bridge
//! decoder's unit tests in `src/host.rs`; the crate README lists them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_codemode::{
    Event, ExecuteRequest, Failure, GlobalDeclaration, MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS,
    OutputItem, StartError, ToolDeclaration,
};
use serde_json::{Value, json};

use crate::harness::{
    BOUND, CallStatus, Execute, Setup, Tool, async_tool, console, echo, engine, execute, image,
    json, run, text, tool,
};

// Base64 of the leading bytes of each format. image() only inspects the signature.
const PNG: &str = "iVBORw0KGgo=";
const JPEG: &str = "/9j/4A==";
const GIF: &str = "R0lGODlh";
const WEBP: &str = "UklGRgAAAABXRUJQ";

fn global(
    name: &str,
    spread: bool,
    body: impl Fn(Option<Value>) -> Option<Value> + Send + Sync + 'static,
) -> (GlobalDeclaration, Execute) {
    let body = Arc::new(body);
    let execute: Execute = Arc::new(move |args, _| {
        let body = Arc::clone(&body);
        Box::pin(async move { Ok(body(args)) })
    });
    (
        GlobalDeclaration {
            name: name.to_owned(),
            spread,
        },
        execute,
    )
}

// ---- script execution

#[tokio::test]
async fn returns_the_scripts_return_value_after_a_json_round_trip() {
    let execution = run("return { a: 1, b: [true, 'x'] }").await;
    assert_eq!(
        execution.value(),
        Some(&json!({ "a": 1, "b": [true, "x"] }))
    );
    assert!(execution.output.is_empty());
    assert!(execution.calls.is_empty());
    assert_eq!(run("return 'plain'").await.value(), Some(&json!("plain")));
    assert_eq!(run("").await.value(), None);
}

#[tokio::test]
async fn supports_top_level_await() {
    let execution = run("const x = await Promise.resolve(41); return x + 1").await;
    assert_eq!(execution.value(), Some(&json!(42)));
}

#[tokio::test]
async fn collects_text_image_and_console_output_in_order() {
    let execution = run(&format!(
        r#"
        console.log("hello", 1, {{ a: 1 }});
        text({{ json: true }});
        text(undefined);
        text(7);
        image("data:image/png;base64,{PNG}");
        image({{ image_url: "data:image/jpeg;base64,{JPEG}" }});
        image({{ type: "image", data: "{GIF}", mimeType: "image/gif" }});
        image("data:image/png;base64,{WEBP}");
        image({{ type: "image", data: "{PNG}" }});
        console.error(new Error("bad"));
        return null;
        "#
    ))
    .await;
    assert_eq!(execution.value(), Some(&Value::Null));
    let (last, items) = execution
        .output
        .split_last()
        .unwrap_or_else(|| panic!("no output"));
    assert_eq!(
        items,
        [
            console("hello 1 {\"a\":1}"),
            text("{\"json\":true}"),
            text("undefined"),
            text("7"),
            image(PNG, "image/png"),
            image(JPEG, "image/jpeg"),
            image(GIF, "image/gif"),
            // The MIME type comes from the data, not from the declared type.
            image(WEBP, "image/webp"),
            image(PNG, "image/png"),
        ]
    );
    assert!(
        matches!(last, OutputItem::Text { text, console: true } if text.starts_with("Error: bad")),
        "{last:?}"
    );
}

#[tokio::test]
async fn rejects_invalid_text_and_image_arguments() {
    let execution = run(r#"
        const errors = [];
        const circular = {};
        circular.self = circular;
        for (const run of [
            () => text(circular),
            () => image(""),
            () => image("https://example.com/a.png"),
            () => image("data:image/png,raw"),
            () => image({ type: "text", text: "x" }),
            () => image({ type: "image", data: "" }),
            () => image(42),
            () => image("data:image/png;base64,AAAA!"),
            () => image("data:image/png;base64,AAAAA"),
            () => image("data:image/png;base64,AA=A"),
            () => image("data:image/png;base64,"),
            () => image("data:image/png;base64,AAAA\n[Output truncated]"),
            () => image({ type: "image", data: "AAAA!", mimeType: "image/png" }),
            () => image("data:image/png;base64,AAAA"),
            () => image("data:image/png;base64,QUJD"),
            () => image("data:image/jpeg;base64,/9j/9w=="),
        ]) {
            try { run(); errors.push("no error"); } catch (error) { errors.push(error.name + ": " + error.message); }
        }
        return errors;
    "#)
    .await;
    assert!(execution.output.is_empty());
    let Some(Value::Array(errors)) = execution.value() else {
        panic!("{:?}", execution.value());
    };
    let errors: Vec<&str> = errors.iter().filter_map(Value::as_str).collect();
    assert!(
        errors[0].starts_with("TypeError: ") && errors[0].contains("circular"),
        "{}",
        errors[0]
    );
    let mut expected = vec![
        "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }",
        "TypeError: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead",
        "TypeError: invalid image output. Pass a base64 data URI instead",
        "TypeError: image only accepts image blocks, got \"text\"",
        "TypeError: image expected image block data",
        "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }",
    ];
    expected.extend(["TypeError: invalid image output. The image data is not valid base64 (truncated or corrupted?)"; 6]);
    expected.extend(
        ["TypeError: invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image";
            3],
    );
    assert_eq!(&errors[1..], expected.as_slice());
}

// https://github.com/earendil-works/pi/issues/10215
#[tokio::test]
async fn accepts_wrapped_and_large_base64_image_data() {
    let large = format!("iVBORw0KGgoA{}", "QUJD".repeat(256 * 1024));
    let execution = run(&format!(
        "image(\"data:image/png;base64,iVBORw0K\\r\\nGgo=\\n\");\nimage(\"data:image/png;base64,{large}\");"
    ))
    .await;
    assert!(execution.outcome.result.is_ok());
    assert_eq!(
        execution.output,
        [image(PNG, "image/png"), image(&large, "image/png")]
    );
}

#[tokio::test]
async fn ends_the_script_successfully_on_exit_keeping_output_and_store_writes() {
    let execution = execute(
        r#"
        text("before");
        store("k", 1);
        await tools.echo(1);
        try { exit(); } catch {}
        text("after");
        return "unreachable";
        "#,
        Setup::tools(vec![echo()]),
    )
    .await;
    let completion = execution.completion();
    assert_eq!(completion.value, None);
    assert_eq!(execution.output, [text("before")]);
    assert_eq!(completion.store_writes.set.get("k"), Some(&json!(1)));
    assert!(completion.store_writes.delete.is_empty());
}

#[tokio::test]
async fn keeps_output_produced_before_a_failure() {
    let execution = run("text(\"partial\");\nthrow new Error(\"boom\")").await;
    assert!(matches!(execution.failure(), Failure::Exception(_)));
    assert_eq!(execution.output, [text("partial")]);
}

#[tokio::test]
async fn reports_syntax_errors_with_the_scripts_line_number() {
    let execution = run("const a = 1;\nconst b = ;\nreturn a").await;
    let error = execution.exception();
    assert_eq!(error.name.as_deref(), Some("SyntaxError"));
    assert!(
        error
            .stack
            .as_deref()
            .is_some_and(|stack| stack.contains("codemode.js:2")),
        "{error:?}"
    );
}

#[tokio::test]
async fn reports_thrown_errors_with_the_scripts_line_number() {
    let execution = run("const a = 1;\nthrow new TypeError('boom ' + a)").await;
    let error = execution.exception();
    assert_eq!(error.name.as_deref(), Some("TypeError"));
    assert_eq!(error.message, "boom 1");
    assert!(
        error
            .stack
            .as_deref()
            .is_some_and(|stack| stack.contains("codemode.js:2")),
        "{error:?}"
    );
}

#[tokio::test]
async fn formats_stacks_like_v8_without_prelude_frames() {
    let execution = run("console.log(new Error('inner'));\nthrow new RangeError('outer')").await;
    let stack = execution.exception().stack.clone().unwrap_or_default();
    let mut lines = stack.lines();
    assert_eq!(lines.next(), Some("RangeError: outer"));
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("    at ") && line.contains("codemode.js:2")),
        "{stack}"
    );
    assert!(!stack.contains("codemode-prelude.js"));
    let first = execution.texts()[0];
    assert!(
        first.starts_with("Error: inner\n    at ")
            && first
                .lines()
                .nth(1)
                .is_some_and(|line| line.contains("codemode.js:1")),
        "{first}"
    );
    assert!(!format!("{:?}", execution.output).contains("codemode-prelude.js"));
}

#[tokio::test]
async fn reports_non_error_throws() {
    let execution = run("throw { code: 7 }").await;
    assert_eq!(execution.exception().message, "{\"code\":7}");
}

#[tokio::test]
async fn reports_a_non_serializable_return_value_as_a_script_error() {
    let execution = run("return 10n").await;
    assert_eq!(execution.exception().name.as_deref(), Some("TypeError"));
}

// ---- tools

#[tokio::test]
async fn exposes_tools_as_async_functions_and_records_calls() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let add = tool("add", move |args| {
        let args = args.unwrap_or(Value::Null);
        if let Ok(mut seen) = recorded.lock() {
            seen.push(args.clone());
        }
        let sum = args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0);
        Ok(Some(json!({ "sum": sum })))
    });
    let execution = execute(
        r#"
        const first = await tools.add({ a: 1, b: 2 });
        const second = await tools.add({ a: first.sum, b: 10 });
        return second.sum;
        "#,
        Setup::tools(vec![add]),
    )
    .await;
    assert_eq!(execution.value().and_then(Value::as_f64), Some(13.0));
    let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
    assert_eq!(
        seen,
        [json!({ "a": 1, "b": 2 }), json!({ "a": 3, "b": 10 })]
    );
    let calls: Vec<(&str, &CallStatus)> = execution
        .calls
        .iter()
        .map(|call| (call.name.as_str(), &call.status))
        .collect();
    assert_eq!(calls, [("add", &CallStatus::Ok), ("add", &CallStatus::Ok)]);
}

/// A record says Ok or Error whenever the script received the call's result;
/// cancelled is only for a call the run abandoned. `hold_answered_calls`
/// holds each call's task past its answer, so the script ends and the host
/// reads its records while every task is still running: only a status the
/// task wrote before it answered can be read.
#[tokio::test]
async fn records_a_call_the_script_received_the_result_of_before_the_run_ends() {
    let execution = execute(
        "return await tools.echo(1)",
        Setup {
            tools: vec![echo()],
            hold_answered_calls: true,
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(1)));
    assert_eq!(execution.calls[0].status, CallStatus::Ok);
}

/// As above for a call the script caught the error of.
#[tokio::test]
async fn records_a_call_the_script_caught_the_error_of_before_the_run_ends() {
    let execution = execute(
        "try { await tools.fail(); } catch (error) { return error.message; }",
        Setup {
            tools: vec![tool("fail", |_| Err("tool exploded".to_owned()))],
            hold_answered_calls: true,
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!("tool exploded")));
    assert_eq!(execution.calls[0].status, CallStatus::Error);
}

fn delay() -> Tool {
    async_tool("delay", |args, _| async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(args)
    })
}

#[tokio::test]
async fn runs_concurrent_calls_and_lists_tool_names() {
    let execution = execute(
        r#"
        const [a, b, c] = await Promise.all([tools.delay(1), tools.delay(2), tools.echo(3)]);
        return { values: [a, b, c], names: Object.keys(tools) };
        "#,
        Setup::tools(vec![echo(), delay()]),
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!({ "values": [1, 2, 3], "names": ["echo", "delay"] }))
    );
}

#[tokio::test]
async fn exposes_tools_under_normalized_identifiers_and_lists_them_in_all_tools() {
    let mut dash = tool("my-tool", |_| Ok(Some(json!("dash"))));
    dash.description = Some("Dashes".to_owned());
    let mut underscore = tool("my_tool", |_| Ok(Some(json!("underscore"))));
    underscore.description = Some("Shadowed".to_owned());
    let execution = execute(
        r#"
        try { ALL_TOOLS.push({}); } catch {}
        return {
            all: ALL_TOOLS,
            calls: [await tools.my_tool(), await tools["my-tool"](), await tools.mcp__docs__search()],
        };
        "#,
        Setup::tools(vec![dash, underscore, tool("mcp__docs__search", |_| Ok(Some(json!("mcp"))))]),
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!({
            "all": [
                { "name": "my_tool", "description": "Dashes" },
                { "name": "mcp__docs__search", "description": "" },
            ],
            "calls": ["dash", "dash", "mcp"],
        }))
    );
}

#[tokio::test]
async fn passes_undefined_arguments_and_results_through() {
    let execution = execute(
        "return [await tools.noop(), await tools.noop(null)]",
        Setup::tools(vec![tool("noop", Ok)]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!([null, null])));
}

#[tokio::test]
async fn turns_tool_errors_into_catchable_errors_in_the_script() {
    let execution = execute(
        r#"
        try {
            await tools.fail();
            return "no error";
        } catch (error) {
            return { isError: error instanceof Error, message: error.message };
        }
        "#,
        Setup::tools(vec![tool("fail", |_| Err("tool exploded".to_owned()))]),
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!({ "isError": true, "message": "tool exploded" }))
    );
    assert_eq!(execution.calls[0].name, "fail");
    assert_eq!(execution.calls[0].status, CallStatus::Error);
}

#[tokio::test]
async fn rejects_calls_to_unknown_tools() {
    let execution = run("return await tools.missing()").await;
    assert_eq!(execution.exception().name.as_deref(), Some("TypeError"));
}

#[tokio::test]
async fn aborts_unawaited_calls_when_the_script_returns() {
    let slow = async_tool("slow", |_, _| async {
        std::future::pending::<()>().await;
        Ok(None)
    });
    let execution = execute("tools.slow(); return 'early'", Setup::tools(vec![slow])).await;
    assert_eq!(execution.value(), Some(&json!("early")));
    // The run ended with the call still pending, which the host cancels.
    assert_eq!(execution.calls[0].status, CallStatus::Cancelled);
}

#[tokio::test]
async fn names_close_matches_when_a_script_reads_a_tool_that_does_not_exist() {
    async fn attempt(expression: &str) -> Value {
        let execution = execute(
            &format!("return {expression};"),
            Setup::tools(vec![echo(), tool("web-search", |_| Ok(Some(json!(""))))]),
        )
        .await;
        match &execution.outcome.result {
            Ok(completion) => completion.value.clone().unwrap_or(Value::Null),
            Err(failure) => Value::String(failure.message().to_owned()),
        }
    }
    assert_eq!(
        attempt("tools.Echo").await,
        json!(
            "tools.Echo does not exist. Did you mean tools.echo? ALL_TOOLS lists every tool; searchTools(query) finds tools by topic. Check for a member with \"Echo\" in tools."
        )
    );
    assert!(
        attempt("tools.websearch")
            .await
            .as_str()
            .is_some_and(|text| text.contains("Did you mean tools.web_search?"))
    );
    assert!(
        attempt("tools.nothing")
            .await
            .as_str()
            .is_some_and(|text| text.contains("Available: echo, web_search."))
    );
    assert_eq!(
        attempt(
            "['echo' in tools, 'nothing' in tools, String(tools.toString), JSON.stringify(tools)]"
        )
        .await,
        json!([true, false, "undefined", "{}"])
    );
}

/// Pi's "supports register and unregister between executions": each run sees
/// the tools its request declares, and a name declared twice is refused.
#[tokio::test]
async fn each_run_declares_its_own_tools() {
    let mut request = ExecuteRequest::new("return 1");
    request.tools = vec![
        ToolDeclaration::named("echo"),
        ToolDeclaration::named("echo"),
    ];
    let refused = engine().start(request).map(|_| ());
    assert_eq!(
        refused,
        Err(StartError::InvalidRequest(
            "Tool \"echo\" is already registered".to_owned()
        ))
    );
    let with = execute("return await tools.echo('a')", Setup::tools(vec![echo()])).await;
    assert_eq!(with.value(), Some(&json!("a")));
    let without = run("return 'echo' in tools").await;
    assert_eq!(without.value(), Some(&json!(false)));
}

// ---- store and load

#[tokio::test]
async fn reads_the_snapshot_and_reports_writes() {
    let execution = execute(
        r#"
        const seen = load("counter");
        store("counter", seen + 1);
        store("list", [1, { a: null }]);
        store("old", undefined);
        return [seen, load("counter"), load("missing"), load("old")];
        "#,
        Setup {
            store: json(r#"{ "counter": 41, "old": "x" }"#)
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Setup::default()
        },
    )
    .await;
    // undefined array elements become null in the JSON round trip of the return value.
    assert_eq!(execution.value(), Some(&json!([41, 42, null, null])));
    let writes = &execution.completion().store_writes;
    assert_eq!(
        Value::Object(writes.set.clone()),
        json!({ "counter": 42, "list": [1, { "a": null }] })
    );
    assert_eq!(writes.delete, ["old"]);
}

#[tokio::test]
async fn returns_copies_so_mutating_a_loaded_value_does_not_change_the_store() {
    let execution = execute(
        r#"const value = load("obj"); value.a = 2; const kept = { b: 1 }; store("kept", kept); kept.b = 2;
        return [load("obj").a, load("kept").b];"#,
        Setup {
            store: json(r#"{ "obj": { "a": 1 } }"#).as_object().cloned().unwrap_or_default(),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!([1, 1])));
    assert_eq!(
        execution.completion().store_writes.set.get("kept"),
        Some(&json!({ "b": 1 }))
    );
}

#[tokio::test]
async fn rejects_invalid_keys_values_and_oversized_writes_inside_the_script() {
    let execution = run(r#"
        const attempt = (fn) => { try { fn(); return "ok"; } catch (error) { return error.name; } };
        return [
            attempt(() => store(1, "x")),
            attempt(() => load({})),
            attempt(() => store("fn", () => 1)),
            attempt(() => store("big", "x".repeat(300 * 1024))),
            attempt(() => { for (let i = 0; i < 8; i++) store("k" + i, "x".repeat(200 * 1024)); }),
        ];
    "#)
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([
            "TypeError",
            "TypeError",
            "TypeError",
            "RangeError",
            "RangeError"
        ]))
    );
}

#[tokio::test]
async fn explains_oversized_writes() {
    let execution = run("store(\"img\", \"x\".repeat(300 * 1024));").await;
    let message = &execution.exception().message;
    assert!(
        message.contains("store(\"img\") value has 307202 characters of JSON"),
        "{message}"
    );
    assert!(message.contains("Show images with image()"), "{message}");
}

fn refused_globals(globals: Vec<GlobalDeclaration>) -> String {
    let mut request = ExecuteRequest::new("");
    request.globals = globals;
    match engine().start(request) {
        Ok(_) => panic!("the globals were accepted"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn reserves_the_store_and_load_names() {
    assert!(refused_globals(vec![GlobalDeclaration::new("store")]).contains("Invalid global"));
    assert!(refused_globals(vec![GlobalDeclaration::new("load")]).contains("Invalid global"));
}

// ---- globals

#[tokio::test]
async fn exposes_globals_as_top_level_functions_without_recording_them_as_calls() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let execution = execute(
        r#"
        await attach({ ref: 1 });
        attach("not awaited");
        return [typeof attach, typeof globalThis.attach, await tools.echo(2)];
        "#,
        Setup {
            tools: vec![echo()],
            globals: vec![global("attach", false, move |args| {
                if let Ok(mut seen) = recorded.lock() {
                    seen.push(args.unwrap_or(Value::Null));
                }
                None
            })],
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(execution.value(), Some(&json!(["function", "function", 2])));
    assert_eq!(
        execution
            .calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        ["echo"]
    );
    // Calls are reported in order, so an unawaited global is issued before the script settles.
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().map(|seen| seen.len()).unwrap_or(0) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
    assert_eq!(seen, [json!({ "ref": 1 }), json!("not awaited")]);
}

#[tokio::test]
async fn groups_namespaced_globals_and_spreads_arguments_on_request() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let execution = execute(
        r#"
        await models.list("classifier", undefined, 3);
        await models.list();
        try { models.extra = 1; } catch {}
        return [Object.keys(models), await models.first("a", "ignored"), "extra" in models];
        "#,
        Setup {
            globals: vec![
                global("models.list", true, move |args| {
                    if let Ok(mut seen) = recorded.lock() {
                        seen.push(args.unwrap_or(Value::Null));
                    }
                    None
                }),
                global("models.first", false, |args| args),
            ],
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([["list", "first"], "a", false]))
    );
    // undefined array elements become null in the JSON round trip.
    let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
    assert_eq!(seen, [json!(["classifier", null, 3]), json!([])]);
}

#[tokio::test]
async fn names_the_members_of_a_namespace_when_a_script_reads_one_that_does_not_exist() {
    let execution = execute(
        "await models.generateImage();",
        Setup {
            globals: vec![
                global("models.classify", false, |_| None),
                global("models.generateImages", false, |_| None),
            ],
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(
        execution.exception().message,
        "models.generateImage does not exist. Did you mean models.generateImages? Check for a member with \"generateImage\" in models."
    );
}

#[test]
fn rejects_invalid_and_reserved_global_names() {
    for name in [
        "a.b.c",
        "a.",
        ".a",
        "tools.x",
        "store.x",
        "a.not-valid",
        "not-valid",
        "tools",
        "console",
    ] {
        assert!(
            refused_globals(vec![GlobalDeclaration::new(name)]).contains("Invalid global"),
            "{name}"
        );
    }
    assert!(
        refused_globals(vec![
            GlobalDeclaration::new("models"),
            GlobalDeclaration::new("models.list")
        ])
        .contains("conflicts with the namespace")
    );
}

// ---- limits and lifetime

#[tokio::test]
async fn terminates_a_synchronous_infinite_loop_on_timeout() {
    let started = Instant::now();
    let execution = execute(
        "while (true) {}",
        Setup {
            timeout_ms: Some(200),
            ..Setup::default()
        },
    )
    .await;
    assert!(matches!(execution.failure(), Failure::Timeout { .. }));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn runs_without_a_deadline_when_no_timeout_is_set() {
    let wait = async_tool("wait", |_, _| async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(Some(json!("late")))
    });
    let execution = execute("return await tools.wait()", Setup::tools(vec![wait])).await;
    assert_eq!(execution.value(), Some(&json!("late")));
}

#[tokio::test]
async fn fails_a_script_that_waits_on_a_promise_nothing_can_settle() {
    let execution = execute(
        "await tools.echo(1); await new Promise(() => {}); return 'never'",
        Setup::tools(vec![echo()]),
    )
    .await;
    let Failure::NeverSettles(error) = execution.failure() else {
        panic!("{:?}", execution.failure());
    };
    assert!(error.message.contains("can never settle"), "{error:?}");
    // Returning while a call is still pending is not a stall.
    let returned = execute("tools.echo(2); return 'early'", Setup::tools(vec![echo()])).await;
    assert_eq!(returned.value(), Some(&json!("early")));
}

#[tokio::test]
async fn terminates_a_microtask_spinning_loop_on_timeout() {
    let execution = execute(
        "while (true) await null",
        Setup {
            timeout_ms: Some(200),
            ..Setup::default()
        },
    )
    .await;
    assert!(
        matches!(execution.failure(), Failure::Timeout { .. }),
        "{:?}",
        execution.failure()
    );
}

/// The next event of `run` that is not a call, within [`BOUND`].
async fn done(run: &mut bake_codemode::Run) -> Arc<bake_codemode::Outcome> {
    loop {
        match tokio::time::timeout(BOUND, run.next()).await {
            Ok(Event::Call(_)) => {}
            Ok(Event::Done(outcome)) => return outcome,
            other => panic!("{other:?}"),
        }
    }
}

/// Pi's "aborts via signal and cancels in-flight calls", with `Run::abort`
/// and `Run::abort_with` for the signal and its reason. Cancelling the
/// call's own work is the host's (the harness sets its flag on the outcome).
#[tokio::test]
async fn aborts_and_leaves_in_flight_calls_to_the_host() {
    let setup = Setup::tools(vec![tool("hang", |_| Ok(None))]);
    let mut first = crate::harness::start("await tools.hang(); return 'never'", &setup);
    let event = tokio::time::timeout(BOUND, first.next()).await;
    let Ok(Event::Call(first_call)) = event else {
        panic!("{event:?}");
    };
    assert_eq!(first_call.name(), "hang");

    // A run aborted with a reason as it starts reports the reason.
    let mut cancelled = crate::harness::start("await tools.hang()", &setup);
    cancelled.abort_with("user cancelled");
    assert_eq!(
        done(&mut cancelled).await.result,
        Err(Failure::Aborted {
            message: "user cancelled".to_owned()
        })
    );

    let mut second = crate::harness::start("await tools.hang(); return 'never'", &setup);
    let event = tokio::time::timeout(BOUND, second.next()).await;
    let Ok(Event::Call(call)) = event else {
        panic!("{event:?}");
    };
    assert_eq!(call.name(), "hang");
    second.abort();
    assert_eq!(
        done(&mut second).await.result,
        Err(Failure::Aborted {
            message: "Execution aborted".to_owned()
        })
    );
    // The call can no longer be answered; the answer is ignored.
    assert_eq!(second.answer(call.id(), Ok(None)), Ok(()));
    assert!(second.finished());

    // Pi's `close()` aborts what still runs with "Sandbox closed".
    assert!(!first.finished());
    first.abort_with("Sandbox closed");
    assert_eq!(
        done(&mut first).await.result,
        Err(Failure::Aborted {
            message: "Sandbox closed".to_owned()
        })
    );
}

// #10283: the host keeps all output, so a script that prints in a loop must not grow it without bound.
#[tokio::test]
async fn fails_a_script_whose_output_passes_the_limits_even_if_it_catches_the_error() {
    for print in [
        "text(s)",
        "console.log(s)",
        "image(\"data:image/png;base64,\" + p)",
    ] {
        let execution = run(&format!(
            r#"
            const s = "x".repeat(1 << 20);
            const p = "iVBORw0KGgoA" + "A".repeat(1 << 20);
            for (;;) {{ try {{ {print}; }} catch {{}} }}
            "#
        ))
        .await;
        let error = execution.exception();
        assert_eq!(error.name.as_deref(), Some("RangeError"), "{print}");
        assert!(error.message.contains("script output exceeded"), "{print}");
        let chars: usize = execution
            .output
            .iter()
            .map(|item| match item {
                OutputItem::Text { text, .. } => text.len(),
                OutputItem::Image { data, .. } => data.len(),
            })
            .sum();
        assert!(chars <= MAX_OUTPUT_CHARS, "{print}: {chars}");
        assert!(chars > MAX_OUTPUT_CHARS - (2 << 20), "{print}: {chars}");
    }
    let empty = run("for (;;) text(\"\");").await;
    assert_eq!(empty.exception().name.as_deref(), Some("RangeError"));
    assert_eq!(empty.output.len(), MAX_OUTPUT_ITEMS);
}

#[tokio::test]
async fn runs_executions_in_parallel_without_sharing_state() {
    let (a, b, c) = tokio::join!(
        run("globalThis.shared = 'a'; await null; return globalThis.shared"),
        run("globalThis.shared = 'b'; await null; return globalThis.shared"),
        run("return typeof globalThis.shared"),
    );
    assert_eq!(
        [a.value(), b.value(), c.value()],
        [
            Some(&json!("a")),
            Some(&json!("b")),
            Some(&json!("undefined"))
        ]
    );
}

#[tokio::test]
async fn turns_deep_recursion_into_a_catchable_range_error() {
    let execution = run(r#"
        let depth = 0;
        function dive() { depth++; dive(); }
        try { dive(); } catch (error) { return [error.name, depth > 1000]; }
    "#)
    .await;
    assert_eq!(execution.value(), Some(&json!(["RangeError", true])));
}

// ---- escape hatches

#[tokio::test]
async fn has_no_host_globals() {
    let execution = run(r#"
        return [
            typeof process, typeof require, typeof module, typeof setTimeout, typeof fetch,
            typeof WebAssembly, typeof std, typeof os, typeof globalThis.constructor,
        ]
    "#)
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "function"
        ]))
    );
}

#[tokio::test]
async fn keeps_eval_and_function_inside_the_vm() {
    // Code generation is allowed: it can only produce more code in the same Wasm instance.
    let execution = execute(
        r#"
        return [
            eval("typeof process"),
            new Function("return typeof process")(),
            tools.echo.constructor("return typeof require")(),
            (async () => {}).constructor("return typeof setTimeout")() instanceof Promise,
        ];
        "#,
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!(["undefined", "undefined", "undefined", true]))
    );
}

#[tokio::test]
async fn rejects_dynamic_import() {
    let execution = run(r#"
        try { await import("node:fs"); return "imported"; } catch (error) { return error.constructor.name; }
    "#)
    .await;
    assert_ne!(execution.value(), Some(&json!("imported")));
}

// #10444: the prelude shares the built-ins with the script, so patching them could corrupt what
// the prelude sends to the host.
#[tokio::test]
async fn ignores_patches_to_built_ins_and_built_in_globals() {
    let execution = execute(
        r#"
        Array.prototype.toJSON = () => null;
        Object.prototype.toJSON = () => 5;
        Promise.prototype.then = () => {};
        Map.prototype.get = () => undefined;
        globalThis.JSON = { stringify: () => "x", parse: () => "x" };
        store("k", [1]);
        return [await tools.echo([2]), JSON.stringify({ a: 1 })];
        "#,
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!([[2], "{\"a\":1}"])));
    assert_eq!(
        execution.completion().store_writes.set.get("k"),
        Some(&json!([1]))
    );
}

#[tokio::test]
async fn freezes_intrinsics_that_are_only_reachable_from_instances() {
    let execution = run(r#"
        return [
            Object.getPrototypeOf(function* () {}).prototype,
            Object.getPrototypeOf(async function () {}),
            Object.getPrototypeOf(Int8Array).prototype,
            Object.getPrototypeOf([][Symbol.iterator]()),
            Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())),
            Object.getPrototypeOf(new Map()[Symbol.iterator]()),
            Object.getPrototypeOf(/a/[Symbol.matchAll]("")),
        ].every((object) => Object.isFrozen(object));
    "#)
    .await;
    assert_eq!(execution.value(), Some(&json!(true)));
}

#[tokio::test]
async fn still_lets_instances_override_properties_of_frozen_prototypes() {
    let execution = run(r#"
        const object = {};
        object.toString = () => "custom";
        function Legacy() {}
        Legacy.prototype = Object.create(Error.prototype);
        Legacy.prototype.constructor = Legacy;
        const bare = new Error();
        bare.message = "set later";
        class MyError extends Error {
            constructor(message) {
                super(message);
                this.name = "MyError";
            }
        }
        let patched = "silent";
        try { Error.prototype.name = "Patched"; } catch (error) { patched = error.constructor.name; }
        return [String(object), new Legacy().constructor === Legacy, bare.message, new MyError("x").name, Error.prototype.name, patched];
    "#)
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([
            "custom",
            true,
            "set later",
            "MyError",
            "Error",
            "TypeError"
        ]))
    );
}

#[tokio::test]
async fn reports_errors_whose_name_or_message_is_not_a_string() {
    let execution = run("const error = new Error('x'); error.message = 42; throw error;").await;
    let error = execution.exception();
    assert_eq!(error.name.as_deref(), Some("Error"));
    assert_eq!(error.message, "42");
}

#[tokio::test]
async fn keeps_tools_and_console_frozen() {
    let execution = execute(
        r#"
        try { tools.echo = () => 'nope'; } catch {}
        try { tools.extra = () => 'nope'; } catch {}
        try { globalThis.tools = null; } catch {}
        return ["extra" in tools, await tools.echo('still')];
        "#,
        Setup::tools(vec![echo()]),
    )
    .await;
    assert_eq!(execution.value(), Some(&json!([false, "still"])));
}

/// Pi's "embedded sources parse as JavaScript": every run evaluates the
/// prelude; this one uses each of its globals.
#[tokio::test]
async fn the_embedded_prelude_evaluates() {
    let execution = run(
        "return [typeof tools, Array.isArray(ALL_TOOLS), typeof text, typeof image, typeof exit, typeof store, typeof load, typeof console.log]",
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([
            "object", true, "function", "function", "function", "function", "function", "function"
        ]))
    );
}

/// The discovery functions are host-answered globals.
#[tokio::test]
async fn discovery_functions_are_answered_by_the_host() {
    let globals = bake_codemode::discovery_globals()
        .into_iter()
        .map(|declaration| {
            let name = declaration.name.clone();
            let execute: Execute = Arc::new(move |args, _| {
                let name = name.clone();
                Box::pin(async move { Ok(Some(json!({ "called": name, "args": args }))) })
            });
            (declaration, execute)
        })
        .collect();
    let execution = execute(
        "return [await searchTools('issues', { limit: 2 }), await describeTool('read'), await describeNamespace('docs')]",
        Setup {
            globals,
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(
        execution.value(),
        Some(&json!([
            { "called": "searchTools", "args": ["issues", { "limit": 2 }] },
            { "called": "describeTool", "args": ["read"] },
            { "called": "describeNamespace", "args": ["docs"] },
        ]))
    );
}
