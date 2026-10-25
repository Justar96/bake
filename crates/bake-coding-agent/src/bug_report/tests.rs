use super::*;
use crate::session::{AgentMessage, NewSessionOptions};

fn message(value: Value) -> AgentMessage {
    AgentMessage::from_json(value.as_object().expect("object").clone())
}

fn bundle(hint: Option<&str>, settings: Option<&Value>) -> BugReportBundle {
    bundle_with(hint, settings, false)
}

fn bundle_with(
    hint: Option<&str>,
    settings: Option<&Value>,
    include_session: bool,
) -> BugReportBundle {
    let model: Model = serde_json::from_value(json!({
        "id": "m1", "name": "Model One", "api": "openai-responses", "provider": "proxy",
        "baseUrl": "https://user:pw@proxy.example/v1?token=abc&region=eu",
        "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "headers": {"X-Api-Key": "secret-header", "Accept": "json"},
        "reasoning": true, "contextWindow": 1000, "maxTokens": 100
    }))
    .expect("model");
    let metadata = collect_metadata(MetadataInput {
        id: Some("report-1".into()),
        hint,
        session_id: "session-1",
        cwd: "/home/me/project",
        include_session,
        message_count: 3,
        model: Some(&model),
        provider_auth_configured: Some(true),
        thinking_level: "medium",
        global_settings: settings,
        environment: collect_environment(
            |name| (name == "SHELL").then(|| "/usr/bin/zsh".to_owned()),
            ["BAKE_HOME".to_owned(), "PATH".to_owned()],
        ),
    });
    BugReportBundle {
        metadata,
        diagnostics: json!({"assistant": [{"errorMessage": "429 from provider"}]}),
        session_jsonl: Some("{\"transcript\":\"private words\"}\n".into()),
        cwd: "/home/me/project".into(),
        home_dir: Some("/home/me".into()),
    }
}

// bug-report.test.ts "removes URL credentials and secret query parameters".
#[test]
fn urls_lose_credentials_and_secret_parameters() {
    assert_eq!(
        redact_url("https://user:pass@proxy.example.com:8080/"),
        "https://proxy.example.com:8080/"
    );
    assert_eq!(
        redact_url("git:https://pat@github.com/org/repo"),
        "git:https://github.com/org/repo"
    );
    assert_eq!(
        redact_url("https://api.example/v1?api-key=abc&model=x"),
        "https://api.example/v1?api-key=%3Credacted%3E&model=x"
    );
    // Bake: a repeated secret keeps its first place, as `set` does.
    assert_eq!(
        redact_url("https://a.example/?token=1&x=2&token=3"),
        "https://a.example/?token=%3Credacted%3E&x=2"
    );
    assert_eq!(redact_url("not a url"), "not a url");
    assert_eq!(
        redact_url("https://example.com/plain?q=1"),
        "https://example.com/plain?q=1"
    );
}

// bug-report.test.ts "redacts nested secret values without hiding token
// counts".
#[test]
fn nested_secrets_are_redacted_and_token_counts_kept() {
    assert_eq!(
        redact_json_value(&json!({
            "apiKey": "sk-123",
            "headers": {"Authorization": "Bearer x", "X-Trace": "1"},
            "compaction": {"reserveTokens": 16_384, "keepRecentTokens": 20_000},
            "baseUrl": "https://me:secret@example.com/",
        })),
        json!({
            "apiKey": "<redacted>",
            "headers": {"Authorization": "<redacted>", "X-Trace": "1"},
            "compaction": {"reserveTokens": 16_384, "keepRecentTokens": 20_000},
            "baseUrl": "https://example.com/",
        })
    );
    assert_eq!(
        redact_json_value(&json!({"token": null, "list": [{"password": 1}]})),
        json!({"token": null, "list": [{"password": "<redacted>"}]})
    );
}

#[test]
fn sensitive_keys_follow_pi_s_word_rule() {
    for key in [
        "apiKey",
        "api_key",
        "API-KEY",
        "clientSecret",
        "accessToken",
        "cookie",
        "Authorization",
        "dbPassword",
        "credential_file",
    ] {
        assert!(is_sensitive_key(key), "{key}");
    }
    for key in [
        "reserveTokens",
        "maxTokens",
        "tokenizer",
        "secretary",
        "credentials",
        "model",
    ] {
        assert!(!is_sensitive_key(key), "{key}");
    }
}

#[test]
fn settings_drop_tracking_ids_and_redact() {
    assert_eq!(
        redact_settings(
            &json!({"trackingId": "t", "deviceId": "d", "apiKey": "k", "theme": "dark"})
        ),
        json!({"apiKey": "<redacted>", "theme": "dark"})
    );
}

#[test]
fn the_environment_lists_bake_variable_names_but_no_values() {
    let environment = collect_environment(
        |name| match name {
            "SHELL" => Some("/bin/bash".into()),
            "TMUX" => Some("/tmp/tmux".into()),
            _ => None,
        },
        [
            "DSH_HOME".to_owned(),
            "BAKE_API_KEY".to_owned(),
            "OPENAI_API_KEY".to_owned(),
        ],
    );
    assert_eq!(
        environment["bakeEnvironmentVariables"],
        json!(["BAKE_API_KEY", "DSH_HOME"])
    );
    assert_eq!(environment["shell"], "bash");
    assert_eq!(environment["terminal"]["tmux"], true);
    assert_eq!(environment["terminal"]["ssh"], false);
    assert_eq!(environment["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn metadata_redacts_the_model_and_settings_and_hides_the_directory() {
    let settings = json!({"cliproxyapi": {"apiKey": "sk-live"}, "deviceId": "device-xyz"});
    let report = bundle(Some("  it broke  "), Some(&settings));
    let metadata = &report.metadata;
    assert_eq!(metadata["id"], "report-1");
    assert_eq!(metadata["hint"], "it broke");
    assert_eq!(metadata["session"]["included"], false);
    assert!(metadata["session"].get("cwd").is_none());
    assert_eq!(
        metadata["model"]["baseUrl"],
        "https://proxy.example/v1?token=%3Credacted%3E&region=eu"
    );
    assert_eq!(
        metadata["model"]["headerNames"],
        json!(["Accept", "X-Api-Key"])
    );
    assert_eq!(
        metadata["settings"]["global"],
        json!({"cliproxyapi": {"apiKey": "<redacted>"}})
    );
    assert_eq!(metadata["provider"]["id"], "proxy");
    let text = metadata.to_string();
    for secret in ["sk-live", "secret-header", "user:pw", "abc", "device-xyz"] {
        assert!(!text.contains(secret), "{secret} leaked");
    }
}

#[test]
fn diagnostics_list_failed_turns_without_their_content() {
    let mut session =
        SessionManager::in_memory(Some("/w"), NewSessionOptions::default(), Vec::new())
            .expect("session");
    let assistant = |stop: &str, extra: Value| {
        let mut value = json!({"role": "assistant", "content": [{"type": "text", "text": "secret answer"}],
            "api": "faux", "provider": "faux", "model": "faux-1", "stopReason": stop,
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "timestamp": 1});
        if let (Value::Object(target), Value::Object(extra)) = (&mut value, extra) {
            target.extend(extra);
        }
        message(value)
    };
    session
        .append_message(message(
            json!({"role": "user", "content": "private question", "timestamp": 1}),
        ))
        .expect("user");
    session
        .append_message(assistant("stop", json!({})))
        .expect("fine");
    let failed = session
        .append_message(assistant("error", json!({"errorMessage": "boom"})))
        .expect("error");
    session
        .append_message(assistant(
            "stop",
            json!({"diagnostics": [{"type": "retry", "attempt": 1}]}),
        ))
        .expect("diagnosed");
    let diagnostics = collect_diagnostics(&session);
    assert_eq!(diagnostics["assistantMessageCount"], 3);
    assert_eq!(diagnostics["entryCount"], 4);
    let listed = diagnostics["assistant"].as_array().expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["entryId"], failed.as_str());
    assert_eq!(listed[0]["errorMessage"], "boom");
    assert_eq!(listed[0]["diagnostics"], json!([]));
    assert_eq!(listed[1]["diagnostics"][0]["type"], "retry");
    assert_eq!(diagnostics["crashes"], json!([]));
    let text = diagnostics.to_string();
    assert!(!text.contains("secret answer") && !text.contains("private question"));
}

#[test]
fn the_issue_body_is_redacted_and_never_holds_the_transcript() {
    let settings = json!({"token": "tok-1", "theme": "dark"});
    let report = bundle(Some("Crash with ``` fences"), Some(&settings));
    let body = issue_body(&report);
    assert!(body.starts_with("Crash with ``` fences\n\n---\nReport ID: `report-1`"));
    assert!(body.contains("<details><summary>report.json</summary>"));
    assert!(body.contains("<details><summary>diagnostics.json</summary>"));
    assert!(body.contains("429 from provider"));
    assert!(body.contains("\"token\": \"<redacted>\""));
    assert!(!body.contains("tok-1"));
    assert!(!body.contains("private words"));
    assert!(!body.contains("user:pw"));
    assert_eq!(issue_title(&report), "Bug report: Crash with ``` fences");
    assert_eq!(issue_title(&bundle(None, None)), "Bug report");
    assert!(issue_body(&bundle(None, None)).starts_with("_No description given._"));
}

#[test]
fn a_long_issue_body_leaves_parts_out_to_fit() {
    let report = bundle(Some("short"), None);
    let full = issue_body(&report);
    let without_diagnostics = issue_body_within(&report, full.len() - 1, |body| body.len());
    assert!(without_diagnostics.contains("<summary>report.json</summary>"));
    assert!(without_diagnostics.contains("diagnostics.json: _Left out"));
    let bare = issue_body_within(&report, 400, |body| body.len());
    assert!(bare.contains("report.json and diagnostics.json: _Left out"));
    assert!(bare.len() <= 400);
    let long = bundle(Some(&"x".repeat(5_000)), None);
    let cut = issue_body_within(&long, 1_000, |body| body.len());
    assert!(cut.len() <= 1_000);
    assert!(cut.contains("…\n\n---\nReport ID"));
}

#[test]
fn the_new_issue_link_is_encoded_and_bounded() {
    let report = bundle(Some(&"é & ? #".repeat(2_000)), None);
    let url = new_issue_url(&report);
    let prefix =
        format!("https://github.com/{BAKE_REPOSITORY}/issues/new?title=Bug%20report%3A%20");
    assert!(url.starts_with(&prefix), "{url}");
    let body = url.split("&body=").nth(1).expect("body");
    assert!(body.len() <= ISSUE_LINK_BODY_LIMIT);
    assert!(!body.contains(['&', '#', '?', ' ']));
}

#[test]
fn files_and_the_recorded_entry_follow_pi() {
    let report = bundle(Some("hint"), None);
    let names: Vec<&str> = report.files().iter().map(|file| file.name).collect();
    assert_eq!(names, ["report.json", "diagnostics.json", "session.jsonl"]);
    assert!(report.files()[0].data.ends_with("}\n"));
    let entry = report.entry_data(Delivery::Issue {
        url: "https://github.com/Justar96/bake/issues/1",
    });
    assert_eq!(
        entry,
        json!({"id": "report-1", "createdAt": report.created_at(), "hint": "hint",
            "sessionIncluded": false, "summaryIncluded": false, "delivery": "issue",
            "url": "https://github.com/Justar96/bake/issues/1"})
    );
    let entry = report.entry_data(Delivery::Zip {
        path: Path::new("/tmp/r.zip"),
    });
    assert_eq!(entry["delivery"], "zip");
    assert_eq!(entry["path"], "/tmp/r.zip");
    assert_eq!(archive_file_name("abc"), "bake-bug-report-abc.zip");
}

#[test]
fn crc_matches_zlib() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

// Pi's zip.ts layout, read back: each local header names its file and
// checks its data, and the end record points at a directory of them all.
#[test]
fn the_archive_reads_back() {
    let when = DosTime::from_parts(2026, 10, 18, 13, 45, 31);
    assert_eq!(when.time, (13 << 11) | (45 << 5) | 15);
    assert_eq!(when.day, (46 << 9) | (10 << 5) | 18);
    let files: [(&str, &[u8]); 2] = [("report.json", b"{}\n"), ("é.txt", b"hello")];
    let archive = zip_archive(&files, when).expect("archive");
    let mut at = 0;
    for (name, data) in files {
        assert_eq!(u32_at(&archive, at), 0x0403_4b50);
        assert_eq!(u16_at(&archive, at + 6), 0x0800);
        assert_eq!(u16_at(&archive, at + 8), 0);
        assert_eq!(u32_at(&archive, at + 14), crc32(data));
        let size = u32_at(&archive, at + 18) as usize;
        let name_len = u16_at(&archive, at + 26) as usize;
        assert_eq!(&archive[at + 30..at + 30 + name_len], name.as_bytes());
        assert_eq!(
            &archive[at + 30 + name_len..at + 30 + name_len + size],
            data
        );
        at += 30 + name_len + size;
    }
    let end = archive.len() - 22;
    assert_eq!(u32_at(&archive, end), 0x0605_4b50);
    assert_eq!(u16_at(&archive, end + 10), 2);
    assert_eq!(u32_at(&archive, end + 16) as usize, at);
    assert_eq!(u32_at(&archive, at), 0x0201_4b50);
}

#[test]
fn an_archive_is_written_once_and_never_over_a_file() {
    let dir = std::env::temp_dir().join(format!("bake-bug-zip-{}", crate::session::id::uuid_v7()));
    std::fs::create_dir(&dir).expect("dir");
    let path = dir.join(archive_file_name("r"));
    let report = bundle(None, None);
    write_archive(&report, &path).expect("written");
    let bytes = std::fs::read(&path).expect("read");
    assert_eq!(u32_at(&bytes, 0), 0x0403_4b50);
    assert!(write_archive(&report, &path).is_err());
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

// Bake: the working directory Pi lists beside an included transcript goes
// into the archive's `report.json` alone; the public issue and its link
// never name it, whichever the transcript choice.
#[test]
fn the_issue_never_names_the_working_directory() {
    for include_session in [false, true] {
        let report = bundle_with(Some("fails"), None, include_session);
        assert_eq!(
            report.metadata["session"].get("cwd").is_some(),
            include_session
        );
        assert!(report.public_metadata()["session"].get("cwd").is_none());
        let body = issue_body(&report);
        let url = new_issue_url(&report);
        for text in [&body, &url] {
            assert!(!text.contains("/home/me"), "{text}");
            assert!(!text.contains("%2Fhome%2Fme"), "{text}");
        }
        let archived = &report.files()[0].data;
        assert_eq!(archived.contains("/home/me/project"), include_session);
    }
}

// Bake: the issue's text names the working directory `<project>` and the
// home directory `~`, in the title, the description, and the JSON.
#[test]
fn the_issue_names_the_project_and_home_by_placeholder() {
    let mut report = bundle(
        Some("/home/me/project/src/a.rs breaks, see /home/me/notes"),
        None,
    );
    report.diagnostics =
        json!({"assistant": [{"errorMessage": "ENOENT /home/me/project/x and C:\\Users\\me\\y"}]});
    report.home_dir = Some("C:\\Users\\me".into());
    let body = issue_body(&report);
    assert!(body.starts_with("<project>/src/a.rs breaks, see /home/me/notes"));
    assert!(body.contains("ENOENT <project>/x and ~\\\\y"), "{body}");
    assert!(!body.contains("Users"));
    assert_eq!(
        issue_title(&report),
        "Bug report: <project>/src/a.rs breaks, see /home/me/notes"
    );
    // A root directory names nothing particular and is kept.
    report.cwd = "/".into();
    report.home_dir = None;
    assert!(issue_body(&report).starts_with("/home/me/project/src/a.rs"));
}

// Bake: an error message and a provider's diagnostics are redacted as the
// metadata is; their stacks stay in the archive and out of the issue.
#[test]
fn diagnostics_are_redacted_and_stacks_stay_in_the_archive() {
    let mut session =
        SessionManager::in_memory(Some("/w"), NewSessionOptions::default(), Vec::new())
            .expect("session");
    session
        .append_message(message(json!({"role": "assistant", "content": [],
            "api": "faux", "provider": "faux", "model": "faux-1", "stopReason": "error",
            "errorMessage": "https://me:pw@proxy.example/v1?api_key=sk-err",
            "diagnostics": [{"type": "error", "error": {"message": "bad", "stack": "at secret (/w/src/x.rs:1)"},
                "data": {"authorization": "Bearer sk-data", "url": "https://proxy.example/?token=sk-q"}}],
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "timestamp": 1})))
        .expect("error");
    let mut report = bundle(Some("x"), None);
    report.diagnostics = collect_diagnostics(&session);
    let archived = report.diagnostics.to_string();
    for secret in ["sk-err", "me:pw", "sk-data", "sk-q"] {
        assert!(!archived.contains(secret), "{secret} in the diagnostics");
    }
    assert!(
        archived.contains("at secret"),
        "the archive keeps the stack"
    );
    let body = issue_body(&report);
    assert!(body.contains("\"message\": \"bad\""));
    assert!(!body.contains("stack") && !body.contains("at secret"));
    for secret in ["sk-err", "me:pw", "sk-data", "sk-q"] {
        assert!(!body.contains(secret), "{secret} in the issue");
    }
}

// Bake: a description far past GitHub's limit is cut in a few measures,
// within the limit and on a character boundary.
#[test]
fn a_huge_description_is_cut_quickly_within_the_limit() {
    let report = bundle(Some(&"é".repeat(200_000)), None);
    let started = std::time::Instant::now();
    let body = issue_body(&report);
    let url = new_issue_url(&report);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let count = body.chars().count();
    assert!(
        count <= ISSUE_BODY_LIMIT && count > ISSUE_BODY_LIMIT - 400,
        "{count}"
    );
    assert!(body.starts_with("éé") && body.contains("é…\n\n---\nReport ID"));
    let encoded = url.split("&body=").nth(1).expect("body");
    assert!(encoded.len() <= ISSUE_LINK_BODY_LIMIT);
    // Every cut is the longest that fits, found in a few dozen measures
    // rather than one per character.
    let measures = std::cell::Cell::new(0);
    let cut = issue_body_within(&report, 1_000, |body| {
        measures.set(measures.get() + 1);
        body.len()
    });
    assert!(cut.len() <= 1_000 && cut.len() > 1_000 - "é".len());
    assert!(measures.get() <= 40, "{} measures", measures.get());
}
