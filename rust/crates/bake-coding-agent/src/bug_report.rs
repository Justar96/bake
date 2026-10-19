//! The report `/bug` files, adapted from Pi v1.3.0
//! `packages/coding-agent/src/core/bug-report.ts` (the redaction,
//! `collectBugReportMetadata`, `collectBugReportDiagnostics`,
//! `bugReportFiles`, and `bugReportArchiveFileName`) and
//! `src/utils/zip.ts`, with the redaction cases of `test/bug-report.test.ts`.
//!
//! Pi uploads its report privately to Radius. Bake's report goes to Bake's
//! repository ([`BAKE_REPOSITORY`]) as a GitHub issue, which is public, so
//! the issue body ([`issue_body`]) holds only the description, the redacted
//! metadata, and the failure diagnostics, without error stacks, and names
//! the working directory as `<project>` and the home directory as `~`; the
//! transcript, when the person includes it, goes only into the local zip
//! archive, with the working directory Pi lists beside it. The metadata keeps
//! Pi's shape, with Bake's facts: the `bake-rs` version and user agent, the
//! `BAKE_`/`DSH_` variable names (never their values), and `settings.json`
//! of the Bake home, which is Bake's only settings scope. Not ported: the
//! model-written summary, crash records (`crashes` stays empty), extension
//! listings, and the `runtime` member, which names Pi's JavaScript engine.
//! The archive stores its files uncompressed, where Pi deflates them; it is
//! still a classic ZIP archive with Pi's UTF-8 names and CRC-32 checks.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use bake_ai::Model;

use crate::session::SessionManager;

/// The repository a bug report is filed in, `owner/name` on GitHub.
pub const BAKE_REPOSITORY: &str = "Justar96/bake";

/// The custom type of the session entry that records a filed report, Pi's
/// `pi.bug-report`. It is not part of the model's context.
pub const BUG_REPORT_ENTRY_TYPE: &str = "bake.bug-report";

const SCHEMA_VERSION: u64 = 1;
const REDACTED: &str = "<redacted>";

/// GitHub's limit on an issue body, in characters.
pub const ISSUE_BODY_LIMIT: usize = 65_536;

/// The longest body a prefilled new-issue link carries, percent-encoded, so
/// the whole link stays within what browsers and GitHub accept.
pub const ISSUE_LINK_BODY_LIMIT: usize = 6_000;

static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[-_])(api[-_]?key|secret|token|password|passwd|credential|authorization|cookie)(?:$|[-_])",
    )
    .expect("a fixed pattern")
});
static CAMEL_BOUNDARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("a fixed pattern"));
static NESTED_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^([a-z][a-z0-9+.-]*:)([a-z][a-z0-9+.-]*://.*)$").expect("a fixed pattern")
});

/// Pi's `isSensitiveKey`: a key naming a credential, in any case, with
/// camelCase words split as `_` separators.
pub fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEY.is_match(&CAMEL_BOUNDARY.replace_all(key, "${1}_${2}"))
}

/// Pi's `redactUrl`: a URL without its user and password, and with the
/// values of secret-looking query parameters replaced; anything that is
/// not a URL, or needs no change, as it was.
pub fn redact_url(value: &str) -> String {
    if let Some(nested) = NESTED_URL.captures(value) {
        return format!("{}{}", &nested[1], redact_url(&nested[2]));
    }
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.to_owned();
    };
    let mut changed = false;
    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        changed = true;
    }
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if pairs.iter().any(|(key, _)| is_sensitive_key(key)) {
        // `URLSearchParams.set` keeps a key's first place and drops its
        // later repeats.
        let mut kept: Vec<(String, String)> = Vec::new();
        for (key, value) in pairs {
            if is_sensitive_key(&key) {
                if !kept.iter().any(|(seen, _)| *seen == key) {
                    kept.push((key, REDACTED.to_owned()));
                }
            } else {
                kept.push((key, value));
            }
        }
        url.query_pairs_mut().clear().extend_pairs(kept);
        changed = true;
    }
    if changed {
        url.to_string()
    } else {
        value.to_owned()
    }
}

/// Pi's `redactJsonValue`: a copy in which every non-null value under a
/// sensitive key is `REDACTED` and every string is passed through
/// [`redact_url`].
pub fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, child)| {
                    let child = if !child.is_null() && is_sensitive_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_json_value(child)
                    };
                    (key.clone(), child)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::String(text) => Value::String(redact_url(text)),
        other => other.clone(),
    }
}

/// Pi's `redactSettings`: the settings without their tracking and device
/// ids, redacted.
pub fn redact_settings(settings: &Value) -> Value {
    let mut settings = settings.clone();
    if let Value::Object(object) = &mut settings {
        object.shift_remove("trackingId");
        object.shift_remove("deviceId");
    }
    redact_json_value(&settings)
}

/// Pi's `collectEnvironment` with Bake's facts, read through `env`; `names`
/// are the environment's variable names, of which only the `BAKE_` and
/// `DSH_` ones are listed, and never their values.
pub fn collect_environment(
    env: impl Fn(&str) -> Option<String>,
    names: impl IntoIterator<Item = String>,
) -> Value {
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let set = |name: &str| var(name).is_some();
    let mut listed: Vec<String> = names
        .into_iter()
        .filter(|name| name.starts_with("BAKE_") || name.starts_with("DSH_"))
        .collect();
    listed.sort();
    listed.dedup();
    let (os_release, os_version) = os_release();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "userAgent": bake_ai::http::user_agent(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "osRelease": os_release,
        "osVersion": os_version,
        "shell": var("SHELL").and_then(|shell| {
            shell.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).map(str::to_owned)
        }),
        "terminal": {
            "term": var("TERM"),
            "program": var("TERM_PROGRAM"),
            "programVersion": var("TERM_PROGRAM_VERSION"),
            "colorterm": var("COLORTERM"),
            "tmux": set("TMUX"),
            "ssh": set("SSH_CONNECTION") || set("SSH_CLIENT") || set("SSH_TTY"),
            "ci": set("CI"),
        },
        "bakeEnvironmentVariables": listed,
    })
}

/// The environment this process runs in, [`collect_environment`] over the
/// process environment.
pub fn current_environment() -> Value {
    collect_environment(
        |name| std::env::var(name).ok(),
        std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()),
    )
}

/// The kernel's release and version, Node's `os.release()` and
/// `os.version()`; `None` where they cannot be read.
#[cfg(unix)]
fn os_release() -> (Option<String>, Option<String>) {
    // SAFETY: `uname` fills the zeroed struct it is given and nothing else;
    // its fields are NUL-terminated on success.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return (None, None);
    }
    let field = |raw: &[libc::c_char]| {
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        Some(String::from_utf8_lossy(&bytes).into_owned()).filter(|text| !text.is_empty())
    };
    (field(&name.release), field(&name.version))
}

#[cfg(not(unix))]
fn os_release() -> (Option<String>, Option<String>) {
    (None, None)
}

/// Pi's `describeModel`: what the model is, with its base URL, sampling,
/// and compatibility settings redacted and only the names of its headers.
pub fn describe_model(model: &Model) -> Value {
    let raw = serde_json::to_value(model).unwrap_or(Value::Null);
    let member = |name: &str| raw.get(name).cloned().unwrap_or(Value::Null);
    let redacted = |name: &str| {
        raw.get(name)
            .filter(|value| !value.is_null())
            .map_or(Value::Null, redact_json_value)
    };
    let mut headers: Vec<&String> = model
        .headers
        .as_ref()
        .map(|headers| headers.keys().collect())
        .unwrap_or_default();
    headers.sort();
    json!({
        "provider": model.provider,
        "id": model.id,
        "name": model.name,
        "api": model.api,
        "baseUrl": redact_url(&model.base_url),
        "reasoning": model.reasoning,
        "input": member("input"),
        "contextWindow": model.context_window,
        "maxTokens": model.max_tokens,
        "samplingParams": redacted("samplingParams"),
        "compat": redacted("compat"),
        "thinkingLevelMap": member("thinkingLevelMap"),
        "headerNames": headers,
    })
}

/// What a report's metadata describes.
pub struct MetadataInput<'a> {
    /// The report id; a new UUIDv7 when `None`.
    pub id: Option<String>,
    pub hint: Option<&'a str>,
    pub session_id: &'a str,
    pub cwd: &'a str,
    pub include_session: bool,
    pub message_count: usize,
    pub model: Option<&'a Model>,
    /// Whether the model's provider has a key or other auth configured.
    pub provider_auth_configured: Option<bool>,
    pub thinking_level: &'a str,
    /// The Bake home's `settings.json`, as stored.
    pub global_settings: Option<&'a Value>,
    /// [`collect_environment`]'s facts.
    pub environment: Value,
}

/// Pi's `collectBugReportMetadata`. The working directory is listed only
/// when the transcript is included, as in Pi.
pub fn collect_metadata(input: MetadataInput<'_>) -> Value {
    let hint = input
        .hint
        .map(str::trim)
        .filter(|hint| !hint.is_empty())
        .map(str::to_owned);
    let mut session = Map::new();
    session.insert("id".into(), input.session_id.into());
    session.insert("included".into(), input.include_session.into());
    session.insert("summaryIncluded".into(), false.into());
    session.insert("messageCount".into(), input.message_count.into());
    if input.include_session {
        session.insert("cwd".into(), input.cwd.into());
    }
    let provider = input.model.map(|model| {
        json!({
            "id": model.provider,
            "authConfigured": input.provider_auth_configured,
        })
    });
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "id": input.id.unwrap_or_else(crate::session::id::uuid_v7),
        "createdAt": crate::session::time::now_iso(),
        "hint": hint,
        "environment": input.environment,
        "session": Value::Object(session),
        "model": input.model.map(describe_model),
        "provider": provider,
        "thinkingLevel": input.thinking_level,
        "settings": {
            "global": input.global_settings.map(redact_settings),
        },
    })
}

/// Pi's `collectBugReportDiagnostics`: the assistant turns that failed,
/// were aborted, or carry provider diagnostics, without their content,
/// each redacted ([`redact_json_value`]).
pub fn collect_diagnostics(session: &SessionManager) -> Value {
    let entries = session.entries();
    let mut assistant = Vec::new();
    let mut assistant_messages = 0u64;
    for entry in &entries {
        if entry.entry_type() != "message" {
            continue;
        }
        let Some(message) = entry.get("message").and_then(Value::as_object) else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        assistant_messages += 1;
        let diagnostics = message
            .get("diagnostics")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let stop = message.get("stopReason").and_then(Value::as_str);
        let empty = diagnostics.as_array().is_some_and(Vec::is_empty);
        if empty
            && stop != Some("error")
            && stop != Some("aborted")
            && message.get("errorMessage").is_none_or(Value::is_null)
        {
            continue;
        }
        let mut failed = Map::new();
        failed.insert("entryId".into(), entry.id().into());
        if let Some(timestamp) = entry.get("timestamp") {
            failed.insert("timestamp".into(), timestamp.clone());
        }
        for name in [
            "provider",
            "model",
            "api",
            "stopReason",
            "rawStopReason",
            "errorMessage",
        ] {
            if let Some(value) = message.get(name) {
                failed.insert(name.into(), value.clone());
            }
        }
        failed.insert("diagnostics".into(), diagnostics);
        // An error or a provider's diagnostic can quote a request, so it is
        // redacted as the metadata is.
        assistant.push(redact_json_value(&Value::Object(failed)));
    }
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "sessionId": session.session_id(),
        "entryCount": entries.len(),
        "assistantMessageCount": assistant_messages,
        "assistant": assistant,
        "crashes": [],
    })
}

/// A report ready to file, Pi's `BugReportBundle` without its summary.
#[derive(Debug, Clone)]
pub struct BugReportBundle {
    /// [`collect_metadata`]'s report.
    pub metadata: Value,
    /// [`collect_diagnostics`]'s report.
    pub diagnostics: Value,
    /// The branch as JSONL when the transcript is included; it goes only
    /// into the archive.
    pub session_jsonl: Option<String>,
    /// The session's working directory, which the issue's text names as
    /// [`PROJECT_PLACEHOLDER`]; it is never written into the issue.
    pub cwd: String,
    /// The person's home directory, which the issue's text names as `~`.
    pub home_dir: Option<String>,
}

/// What the public issue writes in place of the session's working
/// directory.
pub const PROJECT_PLACEHOLDER: &str = "<project>";

/// `value` without any `stack` member, at any depth.
fn without_stacks(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "stack")
                .map(|(key, child)| (key.clone(), without_stacks(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_stacks).collect()),
        other => other.clone(),
    }
}

/// `text` with every occurrence of `path`, as written and as a JSON string
/// escapes it, replaced by `with`. A root path names nothing particular
/// and is left alone.
fn replace_path(text: &str, path: &str, with: &str) -> String {
    let path = path.trim_end_matches(['/', '\\']);
    if path.is_empty() {
        return text.to_owned();
    }
    let text = text.replace(path, with);
    let escaped = path.replace('\\', "\\\\");
    if escaped == path {
        text
    } else {
        text.replace(&escaped, with)
    }
}

/// One file of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportFile {
    pub name: &'static str,
    pub content_type: &'static str,
    pub data: String,
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned());
    text.push('\n');
    text
}

impl BugReportBundle {
    /// The report id.
    pub fn id(&self) -> &str {
        self.metadata["id"].as_str().unwrap_or("")
    }

    /// When the report was made.
    pub fn created_at(&self) -> &str {
        self.metadata["createdAt"].as_str().unwrap_or("")
    }

    /// The description, trimmed, when one was given.
    pub fn hint(&self) -> Option<&str> {
        self.metadata["hint"].as_str()
    }

    /// The metadata the public issue shows: without the working directory,
    /// which belongs with the transcript in the archive alone.
    pub fn public_metadata(&self) -> Value {
        let mut metadata = self.metadata.clone();
        if let Some(session) = metadata.get_mut("session").and_then(Value::as_object_mut) {
            session.shift_remove("cwd");
        }
        metadata
    }

    /// The diagnostics the public issue shows: without error stacks, which
    /// stay in the archive.
    pub fn public_diagnostics(&self) -> Value {
        without_stacks(&self.diagnostics)
    }

    /// `text` as the public issue shows it: the working directory as
    /// [`PROJECT_PLACEHOLDER`] and the home directory as `~`.
    pub fn redact_paths(&self, text: &str) -> String {
        let text = replace_path(text, &self.cwd, PROJECT_PLACEHOLDER);
        match &self.home_dir {
            Some(home) => replace_path(&text, home, "~"),
            None => text,
        }
    }

    /// Pi's `bugReportFiles`: `report.json` and `diagnostics.json`, and
    /// `session.jsonl` when the transcript is included.
    pub fn files(&self) -> Vec<BugReportFile> {
        let mut files = vec![
            BugReportFile {
                name: "report.json",
                content_type: "application/json",
                data: pretty(&self.metadata),
            },
            BugReportFile {
                name: "diagnostics.json",
                content_type: "application/json",
                data: pretty(&self.diagnostics),
            },
        ];
        if let Some(jsonl) = &self.session_jsonl {
            files.push(BugReportFile {
                name: "session.jsonl",
                content_type: "application/x-ndjson",
                data: jsonl.clone(),
            });
        }
        files
    }

    /// The data of the session entry that records this report as filed:
    /// Pi's `BugReportSessionEntryData`, with `delivery` `"issue"` and the
    /// issue's `url`, or `"zip"` and the archive's `path`.
    pub fn entry_data(&self, delivery: Delivery<'_>) -> Value {
        let mut data = Map::new();
        data.insert("id".into(), self.id().into());
        data.insert("createdAt".into(), self.created_at().into());
        data.insert(
            "hint".into(),
            self.hint().map_or(Value::Null, |hint| hint.into()),
        );
        data.insert(
            "sessionIncluded".into(),
            self.metadata["session"]["included"].clone(),
        );
        data.insert("summaryIncluded".into(), false.into());
        match delivery {
            Delivery::Issue { url } => {
                data.insert("delivery".into(), "issue".into());
                data.insert("url".into(), url.into());
            }
            Delivery::Zip { path } => {
                data.insert("delivery".into(), "zip".into());
                data.insert("path".into(), path.display().to_string().into());
            }
        }
        Value::Object(data)
    }
}

/// How a report was filed.
#[derive(Debug, Clone, Copy)]
pub enum Delivery<'a> {
    Issue { url: &'a str },
    Zip { path: &'a Path },
}

/// Pi's `bugReportArchiveFileName`, with Bake's prefix.
pub fn archive_file_name(id: &str) -> String {
    format!("bake-bug-report-{id}.zip")
}

/// The issue's title: `Bug report: ` and the description's first line,
/// its paths redacted ([`BugReportBundle::redact_paths`]), at most 80
/// characters of it, or `Bug report`.
pub fn issue_title(bundle: &BugReportBundle) -> String {
    let hint = bundle.hint().map(|hint| bundle.redact_paths(hint));
    let first = hint
        .as_deref()
        .and_then(|hint| hint.lines().map(str::trim).find(|line| !line.is_empty()));
    match first {
        Some(line) if line.chars().count() > 80 => {
            let cut: String = line.chars().take(79).collect();
            format!("Bug report: {cut}…")
        }
        Some(line) => format!("Bug report: {line}"),
        None => "Bug report".to_owned(),
    }
}

/// A fenced code block that no backtick run in `text` can close.
fn fenced(language: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{}\n{fence}", text.trim_end())
}

fn details(name: &str, text: &str) -> String {
    format!(
        "<details><summary>{name}</summary>\n\n{}\n\n</details>",
        fenced("json", text)
    )
}

/// The issue's body: the description, the report id and version, and the
/// redacted `report.json` ([`BugReportBundle::public_metadata`]) and
/// `diagnostics.json` ([`BugReportBundle::public_diagnostics`]), each
/// folded, with paths redacted ([`BugReportBundle::redact_paths`]); never
/// the transcript. When the whole would exceed `limit` as `measure` counts
/// it, the diagnostics, then the metadata, are left out with a note saying
/// so, and last the description is shortened.
pub fn issue_body_within(
    bundle: &BugReportBundle,
    limit: usize,
    measure: impl Fn(&str) -> usize,
) -> String {
    let description = bundle.hint().map_or_else(
        || "_No description given._".to_owned(),
        |hint| bundle.redact_paths(hint),
    );
    let environment = &bundle.metadata["environment"];
    let footer = bundle.redact_paths(&format!(
        "---\nReport ID: `{}` · bake-rs {} · {}/{}\nFiled with `/bug`. The session transcript is not included.",
        bundle.id(),
        environment["version"].as_str().unwrap_or("unknown"),
        environment["platform"].as_str().unwrap_or("unknown"),
        environment["arch"].as_str().unwrap_or("unknown"),
    ));
    let report = details(
        "report.json",
        &bundle.redact_paths(&pretty(&bundle.public_metadata())),
    );
    let diagnostics = details(
        "diagnostics.json",
        &bundle.redact_paths(&pretty(&bundle.public_diagnostics())),
    );
    const LEFT_OUT: &str =
        "_Left out to fit the issue; the zip archive `/bug` exports holds the whole report._";
    let candidates = [
        vec![
            description.clone(),
            footer.clone(),
            report.clone(),
            diagnostics,
        ],
        vec![
            description.clone(),
            footer.clone(),
            report,
            format!("diagnostics.json: {LEFT_OUT}"),
        ],
        vec![
            description.clone(),
            footer.clone(),
            format!("report.json and diagnostics.json: {LEFT_OUT}"),
        ],
    ];
    for parts in &candidates {
        let body = parts.join("\n\n");
        if measure(&body) <= limit {
            return body;
        }
    }
    // The longest prefix of the description, in whole characters, that
    // fits, found by halving: each probe measures one body, so even a
    // description far past the limit costs a few dozen measures.
    let tail = format!("\n\n{footer}\n\nreport.json and diagnostics.json: {LEFT_OUT}");
    let cut = |chars: usize| {
        let end = description
            .char_indices()
            .nth(chars)
            .map_or(description.len(), |(at, _)| at);
        format!("{}…{tail}", &description[..end])
    };
    let (mut fits, mut over) = (0, description.chars().count());
    while fits < over {
        let middle = fits + (over - fits).div_ceil(2);
        if measure(&cut(middle)) <= limit {
            fits = middle;
        } else {
            over = middle - 1;
        }
    }
    cut(fits)
}

/// [`issue_body_within`] at GitHub's [`ISSUE_BODY_LIMIT`] characters.
pub fn issue_body(bundle: &BugReportBundle) -> String {
    issue_body_within(bundle, ISSUE_BODY_LIMIT, |body| body.chars().count())
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A link that opens GitHub's new-issue form on [`BAKE_REPOSITORY`] with
/// the title and a body that fits [`ISSUE_LINK_BODY_LIMIT`] encoded bytes.
pub fn new_issue_url(bundle: &BugReportBundle) -> String {
    let body = issue_body_within(bundle, ISSUE_LINK_BODY_LIMIT, |body| {
        percent_encode(body).len()
    });
    format!(
        "https://github.com/{BAKE_REPOSITORY}/issues/new?title={}&body={}",
        percent_encode(&issue_title(bundle)),
        percent_encode(&body)
    )
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32 (IEEE), as `zlib.crc32`.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = CRC_TABLE[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// An MS-DOS time and date, Pi's `dosDateTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DosTime {
    pub time: u16,
    pub day: u16,
}

impl DosTime {
    /// The time and date now, in the system's time zone where it is known.
    pub fn now() -> Self {
        let now = jiff::Zoned::now();
        Self::from_parts(
            i32::from(now.year()),
            now.month() as u16,
            now.day() as u16,
            now.hour() as u16,
            now.minute() as u16,
            now.second() as u16,
        )
    }

    /// The fields of a local date and time; years before 1980 are 1980.
    pub fn from_parts(
        year: i32,
        month: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
    ) -> Self {
        let year = u16::try_from((year.max(1980) - 1980).min(127)).unwrap_or(0);
        Self {
            time: (hour << 11) | (minute << 5) | (second >> 1),
            day: (year << 9) | (month << 5) | day,
        }
    }
}

/// Pi's `createZipArchive` with stored entries: local headers and data,
/// the central directory, and its end record. `None` when an entry or the
/// archive is too large for a classic ZIP archive.
pub fn zip_archive(files: &[(&str, &[u8])], when: DosTime) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in files {
        let name = name.as_bytes();
        let size = u32::try_from(data.len()).ok()?;
        let name_len = u16::try_from(name.len()).ok()?;
        let offset = u32::try_from(out.len()).ok()?;
        let checksum = crc32(data);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&when.time.to_le_bytes());
        out.extend_from_slice(&when.day.to_le_bytes());
        out.extend_from_slice(&checksum.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);

        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&0x0800u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&when.time.to_le_bytes());
        directory.extend_from_slice(&when.day.to_le_bytes());
        directory.extend_from_slice(&checksum.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_len.to_le_bytes());
        // Extra and comment lengths, disk number, and attributes.
        directory.extend_from_slice(&[0u8; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name);
    }
    let count = u16::try_from(files.len()).ok()?;
    let directory_size = u32::try_from(directory.len()).ok()?;
    let directory_offset = u32::try_from(out.len()).ok()?;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    Some(out)
}

/// Pi's `writeBugReportArchive`: the report's files as a zip archive at
/// `path`, which must not exist yet. Blocking.
pub fn write_archive(bundle: &BugReportBundle, path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let files = bundle.files();
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|file| (file.name, file.data.as_bytes()))
        .collect();
    let archive = zip_archive(&entries, DosTime::now()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the report is too large for a zip archive",
        )
    })?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&archive)?;
    file.sync_all()
}

#[cfg(test)]
mod tests;
