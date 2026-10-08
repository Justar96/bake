//! Development-only Session format primitives: one current-format header
//! record, one event's `sourceEventSeqs` field, one event row's envelope, one
//! strict V3 codec row decode, a scan of a plain current-format log, and
//! request derivation over unseeded, plain current-format logs of known event
//! types, and restoration of a plain current-format log.
//!
//! [`read_header_record`] decodes the first physical record of a current
//! (format 3) Session log into its logical header metadata, or refuses it with
//! the class TypeScript's `parseHeaderRecord` in
//! `packages/session/session-persistence-jsonl/src/format.ts` would report.
//! Where this crate cannot reproduce the TypeScript outcome, it returns
//! [`HeaderRefusal::NativeSubset`] and claims no TypeScript class.
//! [`decode_source_event_seqs`] expands one already parsed field value; it does
//! not admit the row that carries it. [`decode_row_envelope`] decodes one
//! already parsed row's envelope as the released v2 codec's strict decoder
//! does, borrowing its payload unvalidated. [`decode_v3_row`] wraps it in the
//! strict V3 codec's checks; its output is codec output, not a restored
//! event. [`scan_log`] frames, parses, and decodes an in-memory plain log as
//! TypeScript's `scanLog` does, keeping the decoded prefix, the inherited cut,
//! and the committed byte offset. It does not decompress, so it cannot read a
//! default Zstd-compressed Session file. [`replay_requests`] rebuilds the model
//! request before each recorded Assistant settlement in such a log, failed
//! attempts included, as the TypeScript test helper `replayRequests` does,
//! including tool history across request headers, and refuses input outside
//! its subset; its requests are not restored Session state.
//! [`restore_plain_log`] restores such a log as the production read path
//! does: it validates the stored events, builds the closers for an
//! interrupted turn, and folds the Session's messages, with the catalog's
//! `image/offload` projection applied, request header, tool history, and
//! request context into an immutable [`RestoredLog`]. It is not
//! Agent resume. [`restore_zstd_log`] restores default-format compressed bytes
//! with a caller-supplied plaintext budget and physical torn-tail metadata.
//! [`stage_plain_log`] and [`stage_zstd_log`] stop after the scan, so a caller
//! can check the stored identity before [`StagedLog::restore`] runs the rest,
//! and [`zstd_header_record`] decodes only a compressed log's header frame.
//! [`read_generation_header_record`] reads any format's header record as
//! `stat` does, migrated to current metadata or absent.
//! [`migrate_v2_rows`] strictly decodes a released v2 Session's parsed header
//! and rows and runs the v2→v3 migration over them; its output is not an
//! opened Session, since the final check of the transformed log is not run.
//! [`decode_v0_v1_rows`] decodes a released v0 or v1 Session's parsed header
//! and rows as the released physical codec does, without migrating them, and
//! [`migrate_v0_to_v1`] runs the v0→v1 migration over a decoded v0 Session;
//! its output is the edge's, not an opened Session.
//! [`migrate_v1_to_v2_transformed`] runs the released v1→v2 migration's
//! transformed stage over a decoded v1 Session, grouping its Assistant chunks,
//! packed rows expanded, into attempts; it is the stage a chain runs after
//! v0→v1, not production's read of a v1 file.
//! [`migrate_released_v0_history`] reads a decoded v0 Session through all
//! three edges to format v3, reporting the refusal TypeScript's streaming
//! chain reports first; Assistant chunks and a decoded v1 Session are native
//! limits.
//! [`token_usage`] folds a [`RestoredLog`]'s provider-reported token usage,
//! and [`context_pressure`] its context occupancy with the surface's
//! heuristic token total.
//! [`restored_inbox`] and [`consumed_work`] fold a [`RestoredLog`]'s events
//! into its pending inbox and its account of consumed work.
//! [`fork_seed`] selects the events `SessionStore.fork` copies from a
//! [`RestoredLog`] into a child, or refuses as `SessionForkError` does.
//! [`goal_projection`] folds a [`RestoredLog`]'s goal changes and goal rounds
//! into its durable goal state, keeping the first replay failure.
//! [`turn_boundary`] and [`session_title`] fold a [`RestoredLog`]'s turn and
//! step boundaries and its latest title.
//! [`encode_header_line`] and [`encode_event_line`] produce the exact text
//! TypeScript writes for a current header record and one current event row,
//! without the LF, or refuse with `Unadmitted` where TypeScript throws, or
//! with a native limit.
//! None reads or writes a file. The crate is
//! unstable and unshipped; the preview's `session inspect` and `session stat` use it.

mod assistant_stream;
mod boundary;
mod envelope;
mod fork;
mod generation_header;
mod goal;
mod history;
mod inbox;
mod offload;
mod pressure;
mod repair;
mod replay;
mod request;
mod restore;
mod row_encode;
mod scan;
mod source_event_seqs;
mod usage;
mod v0_to_v1;
mod v1_codec;
mod v1_to_v2;
mod v2_to_v3;
mod v3_row;
mod zstd;

pub use boundary::{
    BoundaryLimit, BoundaryRefusal, StepBoundary, StepBoundaryKind, TurnBoundaryState,
    session_title, turn_boundary,
};
pub use envelope::{
    EnvelopeLimit, EnvelopeRefusal, EnvelopeRejection, NumberField, RequiredField,
    UnadmittedEnvelope, decode_row_envelope,
};
pub use fork::{ForkLimit, ForkRefusal, ForkSeed, fork_seed};
pub use generation_header::{GenerationHeaderRefusal, read_generation_header_record};
pub use goal::{
    GoalBlockReason, GoalLimit, GoalPhase, GoalProjection, GoalProjectionState, GoalRefusal,
    GoalSnapshot, goal_projection,
};
pub use history::{HistoryLimit, HistoryLocation, HistoryRefusal, migrate_released_v0_history};
pub use inbox::{
    ConsumedWork, ConsumedWorkCoercion, ConsumedWorkLimit, InboxLimit, InboxRefusal, PendingInbox,
    consumed_work, restored_inbox,
};
pub use offload::OffloadRejection;
pub use pressure::{
    ContextPressureState, ContextPressureView, PressureLimit, PressureRefusal, RequestRoute,
    context_pressure,
};
pub use replay::{ReplayLimit, ReplayRefusal, SeedRejection, replay_requests};
pub use request::Request;
pub use restore::{
    RestoreLimit, RestoreRefusal, RestoredLog, StagedLog, TornTail, Unsupported, restore_plain_log,
    stage_plain_log,
};
pub use row_encode::{EncodeLimit, EncodeRefusal, encode_event_line, encode_header_line};
pub use scan::{FinishRejection, ScanIssue, ScanLimit, ScanRefusal, ScannedLog, scan_log};
pub use source_event_seqs::{
    SourceEventSeqsLimit, SourceEventSeqsRefusal, SourceEventSeqsRejection,
    decode_source_event_seqs,
};
pub use usage::{
    LastTokenUsage, TokenUsageBuckets, TokenUsageState, UsageLimit, UsageRefusal, token_usage,
};
pub use v0_to_v1::{MigratedV1, V0ToV1Location, V0ToV1Refusal, migrate_v0_to_v1};
pub use v1_codec::{
    DecodedV1Rows, V1CodecLimit, V1CodecLocation, V1CodecRecovery, V1CodecRefusal, V1CodecVersion,
    decode_v0_v1_rows,
};
pub use v1_to_v2::{
    MigratedV1ToV2, V1ToV2Limit, V1ToV2Location, V1ToV2Refusal, migrate_v1_to_v2_transformed,
};
pub use v2_to_v3::{MigratedV2, V2ToV3Layer, V2ToV3Location, V2ToV3Refusal, migrate_v2_rows};
pub use v3_row::{
    Coordinate, Endpoint, EventRejection, StructuralRejection, SystemRecord, V3CodecEvent, V3Limit,
    V3NumberField, V3Rejection, V3RowRefusal, V3Unsupported, decode_v3_row,
};
pub use zstd::{ZstdRefusal, restore_zstd_log, stage_zstd_log, zstd_header_record};

use serde_json::{Map, Value};

/// The only Session format version this reader admits.
pub const CURRENT_SESSION_FORMAT_VERSION: u64 = 3;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const REQUIRED_KEYS: [&str; 6] = [
    "type",
    "version",
    "id",
    "createdAt",
    "isSeeded",
    "delegationDepth",
];
const OPTIONAL_KEYS: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];

/// serde_json 1.0.151 error codes for input that violates the JSON grammar.
/// `JSON.parse` rejects the same input, so these map to [`Rejection::Json`]
/// for a header and to an unparsable record for [`scan_log`]; any other parse
/// error is a native limit. A unit test requires
/// a shared header case witnessing each entry under both runtimes.
const JSON_SYNTAX_ERRORS: [&str; 15] = [
    "EOF while parsing a list",
    "EOF while parsing an object",
    "EOF while parsing a string",
    "EOF while parsing a value",
    "expected `:`",
    "expected `,` or `]`",
    "expected `,` or `}`",
    "expected ident",
    "expected value",
    "invalid escape",
    "invalid number",
    "control character (\\u0000-\\u001F) found while parsing a string",
    "key must be a string",
    "trailing comma",
    "trailing characters",
];

/// Logical metadata of an admitted current header, as TypeScript's `fromHeaderLine` builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHeader {
    pub id: String,
    /// At most 2^53 − 1.
    pub created_at: u64,
    /// Absolute for the [`PathPlatform`] the record was read with.
    pub cwd: Option<String>,
    pub parent_session: Option<String>,
    /// Admitted either way; the inherited cut is an event-level property.
    pub is_seeded: bool,
    pub origin: Option<HeaderOrigin>,
    /// At most 2^53 − 1.
    pub delegation_depth: u64,
    pub agent_preset: Option<String>,
}

/// The only header origin a current Session records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderOrigin {
    Subagent,
}

/// Which flavor of Node's `path.isAbsolute` decides whether `cwd` is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPlatform {
    Posix,
    Win32,
}

impl PathPlatform {
    /// The flavor the TypeScript runtime uses on this host.
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Win32
        } else {
            Self::Posix
        }
    }
}

/// Why a header record was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRefusal {
    /// TypeScript throws `SessionFormatUnsupportedError`; `newer` when the version exceeds the current one.
    UnsupportedVersion { newer: bool },
    /// TypeScript throws a plain `Error` with the matching message.
    Rejected(Rejection),
    /// This crate cannot decide the TypeScript outcome; no TypeScript class is claimed.
    NativeSubset(SubsetLimit),
}

/// The plain-error refusals of TypeScript's `parseHeaderRecord`, in check order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// "empty or header-less session log"
    Framing,
    /// "corrupt session log: header line is not valid JSON"
    Json,
    /// "corrupt session log: first line is not a JSON object"
    NotObject,
    /// "session header uses retired policy baseline fields"
    RetiredPolicyFields,
    /// "corrupt session log: first line is not a session header"
    NotSessionHeader,
}

/// Input whose TypeScript outcome this crate does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubsetLimit {
    /// Node decodes invalid UTF-8 with replacement characters; this crate does not.
    InvalidUtf8,
    /// serde_json refused input that is not proven invalid for `JSON.parse`,
    /// such as lone surrogate escapes, deep nesting, or out-of-range numbers.
    JsonParser,
    /// A number serde_json stores as a non-negative `f64` decides the outcome:
    /// a fraction or exponent spelling, including `0.0`, or an integer above
    /// `u64::MAX`. This crate does not claim JavaScript's rounding of it.
    FloatLexeme,
    /// A foreign version's `id` is an object or array. TypeScript formats it
    /// with `String(id)`, which can throw; this reader does not reproduce
    /// JavaScript's object conversion for diagnostics.
    VersionDiagnostic,
}

/// The bytes up to and including the first LF, the record TypeScript's
/// `scanLog` passes to its header parser; `None` when the log has no LF.
pub fn first_record(log: &[u8]) -> Option<&[u8]> {
    let end = log.iter().position(|&byte| byte == b'\n')?;
    Some(&log[..=end])
}

/// Decode exactly one LF-terminated header record.
///
/// Checks run in TypeScript's order: framing, JSON, object, a numeric version
/// other than the current one, retired policy fields, then the header shape.
/// Duplicate keys keep their last value, as `JSON.parse` does, and JSON `null`
/// is a present value of the wrong type, never an omitted field.
pub fn read_header_record(
    record: &[u8],
    platform: PathPlatform,
) -> Result<SessionHeader, HeaderRefusal> {
    let body = match record.split_last() {
        Some((b'\n', body)) if !body.contains(&b'\n') => body,
        _ => return Err(HeaderRefusal::Rejected(Rejection::Framing)),
    };
    let text = std::str::from_utf8(body)
        .map_err(|_| HeaderRefusal::NativeSubset(SubsetLimit::InvalidUtf8))?;
    let Value::Object(fields) =
        serde_json::from_str(text).map_err(|error| parse_refusal(&error))?
    else {
        return Err(HeaderRefusal::Rejected(Rejection::NotObject));
    };
    refuse_foreign_version(&fields)?;
    if fields.contains_key("sandboxMode") || fields.contains_key("approvalPolicy") {
        return Err(HeaderRefusal::Rejected(Rejection::RetiredPolicyFields));
    }
    header_line(&fields, platform)
}

fn parse_refusal(error: &serde_json::Error) -> HeaderRefusal {
    if is_syntax_error(error) {
        HeaderRefusal::Rejected(Rejection::Json)
    } else {
        HeaderRefusal::NativeSubset(SubsetLimit::JsonParser)
    }
}

/// Whether serde_json refused input that `JSON.parse` also rejects, by its
/// [`JSON_SYNTAX_ERRORS`] code. Any other parse error decides nothing.
fn is_syntax_error(error: &serde_json::Error) -> bool {
    let message = error.to_string();
    let code = message
        .split_once(" at line ")
        .map_or(message.as_str(), |(code, _)| code);
    JSON_SYNTAX_ERRORS.contains(&code)
}

/// TypeScript's `refuseForeignFormatVersion`: only a numeric version is compared.
fn refuse_foreign_version(fields: &Map<String, Value>) -> Result<(), HeaderRefusal> {
    let Some(Value::Number(version)) = fields.get("version") else {
        return Ok(());
    };
    let newer = if let Some(version) = version.as_u64() {
        if version == CURRENT_SESSION_FORMAT_VERSION {
            return Ok(());
        }
        version > CURRENT_SESSION_FORMAT_VERSION
    } else if version.is_i64() || version.as_f64().is_some_and(f64::is_sign_negative) {
        // A negative lexeme's JavaScript value is negative or -0, never 3.
        false
    } else {
        return Err(HeaderRefusal::NativeSubset(SubsetLimit::FloatLexeme));
    };
    if matches!(fields.get("id"), Some(Value::Object(_) | Value::Array(_))) {
        return Err(HeaderRefusal::NativeSubset(SubsetLimit::VersionDiagnostic));
    }
    Err(HeaderRefusal::UnsupportedVersion { newer })
}

enum Count {
    Safe(u64),
    Undecided,
}

/// A non-negative safe integer other than -0; `None` when the value fails.
fn count(value: &Value) -> Option<Count> {
    let Value::Number(number) = value else {
        return None;
    };
    if let Some(number) = number.as_u64() {
        return (number <= MAX_SAFE_INTEGER).then_some(Count::Safe(number));
    }
    if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
        return None;
    }
    Some(Count::Undecided)
}

/// An absent field, or a present string; `None` for any other value, including `null`.
fn optional_string(fields: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match fields.get(key) {
        None => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}

/// TypeScript's `isHeaderLine` guard. A failing conjunct rejects even when
/// another one is undecided.
fn header_line(
    fields: &Map<String, Value>,
    platform: PathPlatform,
) -> Result<SessionHeader, HeaderRefusal> {
    let not_header = HeaderRefusal::Rejected(Rejection::NotSessionHeader);
    let keys_valid = REQUIRED_KEYS.iter().all(|key| fields.contains_key(*key))
        && fields.keys().all(|key| {
            REQUIRED_KEYS.contains(&key.as_str()) || OPTIONAL_KEYS.contains(&key.as_str())
        });
    if !keys_valid
        || fields["type"] != "session"
        // A numeric version is already known to be the current one.
        || !fields["version"].is_number()
    {
        return Err(not_header);
    }
    let Value::String(id) = &fields["id"] else {
        return Err(not_header);
    };
    let created_at = count(&fields["createdAt"]).ok_or(not_header)?;
    let delegation_depth = count(&fields["delegationDepth"]).ok_or(not_header)?;
    let cwd = optional_string(fields, "cwd").ok_or(not_header)?;
    if cwd
        .as_deref()
        .is_some_and(|cwd| !is_absolute(cwd, platform))
    {
        return Err(not_header);
    }
    let parent_session = optional_string(fields, "parentSession").ok_or(not_header)?;
    let Value::Bool(is_seeded) = fields["isSeeded"] else {
        return Err(not_header);
    };
    let origin = match fields.get("origin") {
        None => None,
        Some(origin) if origin == "subagent" => Some(HeaderOrigin::Subagent),
        Some(_) => return Err(not_header),
    };
    let agent_preset = optional_string(fields, "agentPreset").ok_or(not_header)?;
    let (Count::Safe(created_at), Count::Safe(delegation_depth)) = (created_at, delegation_depth)
    else {
        return Err(HeaderRefusal::NativeSubset(SubsetLimit::FloatLexeme));
    };
    Ok(SessionHeader {
        id: id.clone(),
        created_at,
        cwd,
        parent_session,
        is_seeded,
        origin,
        delegation_depth,
        agent_preset,
    })
}

/// Node's `path.posix.isAbsolute` or `path.win32.isAbsolute`, not
/// `std::path::Path::is_absolute`. The Win32 drive form is ASCII-only, so
/// testing UTF-8 bytes matches Node's test of UTF-16 code units.
fn is_absolute(path: &str, platform: PathPlatform) -> bool {
    let bytes = path.as_bytes();
    match platform {
        PathPlatform::Posix => bytes.first() == Some(&b'/'),
        PathPlatform::Win32 => {
            let separator = |byte: &u8| matches!(byte, b'/' | b'\\');
            bytes.first().is_some_and(separator)
                || matches!(bytes, [drive, b':', next, ..] if drive.is_ascii_alphabetic() && separator(next))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../conformance/session/header-cases.json"
        );
        let text = std::fs::read_to_string(path).expect("read header-cases.json");
        serde_json::from_str(&text).expect("parse header-cases.json")
    }

    fn parse_error_code(record: &str) -> String {
        let error =
            serde_json::from_str::<Value>(record).expect_err("serde_json refuses the record");
        let message = error.to_string();
        message
            .split_once(" at line ")
            .map_or(message.clone(), |(code, _)| code.to_owned())
    }

    /// Error codes of the shared cases with `record` text and the given expectation.
    fn codes_where(expect: impl Fn(&Value) -> bool) -> Vec<(String, String)> {
        let table = table();
        let cases = table["cases"].as_array().expect("cases array");
        cases
            .iter()
            .filter(|case| expect(case))
            .map(|case| {
                let record = case["record"].as_str().expect("witness uses record text");
                (
                    case["id"].as_str().expect("case id").to_owned(),
                    parse_error_code(record),
                )
            })
            .collect()
    }

    #[test]
    fn every_syntax_error_entry_has_a_shared_json_parse_witness() {
        // The TypeScript spec runs these same cases through JSON.parse in the real scanner.
        let witnesses = codes_where(|case| {
            case["ts"] == serde_json::json!({"outcome": "rejected", "reason": "json"})
        });
        for (id, code) in &witnesses {
            assert!(
                JSON_SYNTAX_ERRORS.contains(&code.as_str()),
                "{id}: {code:?} is not allow-listed"
            );
        }
        for entry in JSON_SYNTAX_ERRORS {
            assert!(
                witnesses.iter().any(|(_, code)| code == entry),
                "no shared case witnesses {entry:?}"
            );
        }
    }

    #[test]
    fn parse_errors_on_input_json_parse_accepts_are_not_allow_listed() {
        let mut codes: Vec<String> = codes_where(|case| case["rust"]["limit"] == "json-parser")
            .into_iter()
            .map(|(_, code)| code)
            .collect();
        codes.sort();
        codes.dedup();
        assert_eq!(
            codes,
            [
                "number out of range",
                "recursion limit exceeded",
                "unexpected end of hex escape"
            ]
        );
        for code in &codes {
            assert!(!JSON_SYNTAX_ERRORS.contains(&code.as_str()));
        }
    }

    #[test]
    fn absolute_paths_match_node_on_every_host() {
        let table = table();
        let rows = table["absolutePaths"]
            .as_array()
            .expect("absolutePaths array");
        assert!(!rows.is_empty());
        for row in rows {
            let path = row["path"].as_str().expect("path");
            assert_eq!(
                is_absolute(path, PathPlatform::Posix),
                row["posix"],
                "posix {path:?}"
            );
            assert_eq!(
                is_absolute(path, PathPlatform::Win32),
                row["win32"],
                "win32 {path:?}"
            );
        }
    }
}
