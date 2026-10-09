//! The header read behind TypeScript's `JsonlSessionPersistence.stat`: one
//! selected generation's first record, decoded by its own format's codec and
//! migrated to a current logical header, as `readGenerationHeader` in
//! `packages/session/session-persistence-jsonl/src/index.ts` does through
//! `sessionFormatCatalog.readHeader`. Unlike [`crate::read_header_record`], an
//! unreadable header is absent rather than refused.

use serde_json::{Map, Value};

use crate::json_parse::dismantle_fields;
use crate::{
    CURRENT_SESSION_FORMAT_VERSION, Count, HeaderOrigin, PathPlatform, SessionHeader, SubsetLimit,
    count, dismantle, is_absolute, parse_json,
};

const RETIRED_FIELDS: &str = "session header uses retired policy baseline fields";
const V0_REQUIRED: [&str; 5] = ["type", "version", "id", "createdAt", "delegationDepth"];
const V0_OPTIONAL: [&str; 5] = [
    "cwd",
    "parentSession",
    "seedLength",
    "origin",
    "agentPreset",
];
const V2_REQUIRED: [&str; 6] = [
    "type",
    "version",
    "id",
    "createdAt",
    "isSeeded",
    "delegationDepth",
];
const V2_OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];

/// Why a generation header was refused rather than read or treated as absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerationHeaderRefusal {
    /// TypeScript throws a plain `Error` with exactly this message.
    Rejected(String),
    /// TypeScript throws `SessionFormatUnsupportedError` with this message
    /// before its ` (raw log: <path>)` suffix.
    Unsupported(String),
    /// This crate cannot decide the TypeScript outcome; no TypeScript class is claimed.
    NativeSubset(SubsetLimit),
}

/// Read one selected generation's header as `stat` does.
///
/// `record` is the generation's complete first physical record: plaintext
/// ending in its only LF. `source_version` is the version the selected
/// filename names. `Ok(None)` is TypeScript's absent result: missing or
/// broken framing, a JSON syntax error, a non-object, or a header its own
/// format's codec rejects, including a `cwd` that `platform` does not treat
/// as absolute. Retired policy fields are refused first; then, once the
/// stored version is a valid count, a version other than `source_version`
/// is refused even if the rest is malformed, and a newer version is
/// unsupported. Format v0 to v2 headers are migrated: v0 and v1 are seeded
/// exactly when `seedLength` is present, and an `agentPreset` of `code`
/// below v3 becomes `ptc`. The stored identity and path are not checked.
pub fn read_generation_header_record(
    record: &[u8],
    source_version: u64,
    platform: PathPlatform,
) -> Result<Option<SessionHeader>, GenerationHeaderRefusal> {
    use GenerationHeaderRefusal::NativeSubset;
    let body = match record.split_last() {
        Some((b'\n', body)) if !body.contains(&b'\n') => body,
        _ => return Ok(None),
    };
    let text = std::str::from_utf8(body).map_err(|_| NativeSubset(SubsetLimit::InvalidUtf8))?;
    let fields = match parse_json(text) {
        Ok(Value::Object(fields)) => fields,
        Ok(other) => {
            dismantle(other);
            return Ok(None);
        }
        Err(error) if error.is_syntax() => return Ok(None),
        Err(_) => return Err(NativeSubset(SubsetLimit::JsonParser)),
    };
    let header = decode_fields(&fields, source_version, platform);
    dismantle_fields(fields);
    header
}

fn decode_fields(
    fields: &Map<String, Value>,
    source_version: u64,
    platform: PathPlatform,
) -> Result<Option<SessionHeader>, GenerationHeaderRefusal> {
    use GenerationHeaderRefusal::{NativeSubset, Rejected, Unsupported};
    if fields.contains_key("sandboxMode") || fields.contains_key("approvalPolicy") {
        return Err(Rejected(RETIRED_FIELDS.to_owned()));
    }
    let stored_version = match fields.get("version").and_then(count) {
        None => return Ok(None),
        Some(Count::Undecided) => return Err(NativeSubset(SubsetLimit::FloatLexeme)),
        Some(Count::Safe(version)) => version,
    };
    if stored_version != source_version {
        return Err(Rejected(format!(
            "session generation filename identifies v{source_version}, \
             but its header identifies v{stored_version}"
        )));
    }
    if stored_version > CURRENT_SESSION_FORMAT_VERSION {
        let id = js_string(fields.get("id")).map_err(NativeSubset)?;
        return Err(Unsupported(format!(
            "session \"{id}\" uses log format v{stored_version}, but this harness reads only \
             v{CURRENT_SESSION_FORMAT_VERSION}: the log was written by a newer harness \u{2014} \
             upgrade the harness to open it"
        )));
    }
    decode(fields, stored_version, platform).map_err(NativeSubset)
}

/// The released codec for `version`, then the header migrations. A check
/// that fails makes the header absent even when a count is undecided.
fn decode(
    fields: &Map<String, Value>,
    version: u64,
    platform: PathPlatform,
) -> Result<Option<SessionHeader>, SubsetLimit> {
    let historical = version < 2;
    let (required, optional): (&[&str], &[&str]) = if historical {
        (&V0_REQUIRED, &V0_OPTIONAL)
    } else {
        (&V2_REQUIRED, &V2_OPTIONAL)
    };
    let keys_valid = required.iter().all(|key| fields.contains_key(*key))
        && fields
            .keys()
            .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()));
    if !keys_valid || fields["type"] != "session" {
        return Ok(None);
    }
    let Value::String(id) = &fields["id"] else {
        return Ok(None);
    };
    let strings = ["cwd", "parentSession", "agentPreset"].map(|key| match fields.get(key) {
        None => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    });
    let [Some(cwd), Some(parent_session), Some(agent_preset)] = strings else {
        return Ok(None);
    };
    if cwd
        .as_deref()
        .is_some_and(|cwd| !is_absolute(cwd, platform))
    {
        return Ok(None);
    }
    let origin = match fields.get("origin") {
        None => None,
        Some(origin) if origin == "subagent" => Some(HeaderOrigin::Subagent),
        Some(_) => return Ok(None),
    };
    let is_seeded = if historical {
        // JSON null is present, so it fails the count check.
        fields.contains_key("seedLength")
    } else {
        let Value::Bool(is_seeded) = fields["isSeeded"] else {
            return Ok(None);
        };
        is_seeded
    };
    let mut counts = vec![
        count(&fields["createdAt"]),
        count(&fields["delegationDepth"]),
    ];
    if historical {
        counts.extend(fields.get("seedLength").map(count));
    }
    if counts.iter().any(Option::is_none) {
        return Ok(None);
    }
    let counts = counts
        .into_iter()
        .map(|checked| match checked {
            Some(Count::Safe(number)) => Ok(number),
            _ => Err(SubsetLimit::FloatLexeme),
        })
        .collect::<Result<Vec<u64>, _>>()?;
    let agent_preset = match agent_preset {
        Some(preset) if version < 3 && preset == "code" => Some("ptc".to_owned()),
        preset => preset,
    };
    Ok(Some(SessionHeader {
        id: id.clone(),
        created_at: counts[0],
        cwd,
        parent_session,
        is_seeded,
        origin,
        delegation_depth: counts[1],
        agent_preset,
    }))
}

/// JavaScript's `String(value)` for a newer header's `id`, as the refusal formats it.
fn js_string(value: Option<&Value>) -> Result<String, SubsetLimit> {
    match value {
        None => Ok("undefined".to_owned()),
        Some(Value::Null) => Ok("null".to_owned()),
        Some(Value::Bool(flag)) => Ok(flag.to_string()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Number(number)) => {
            // Every u64 and i64 is below 1e21, where JavaScript and Rust both
            // print the nearest double's shortest digits without an exponent.
            let nearest = if let Some(number) = number.as_u64() {
                number as f64
            } else if let Some(number) = number.as_i64() {
                number as f64
            } else {
                return Err(SubsetLimit::FloatLexeme);
            };
            Ok(format!("{nearest}"))
        }
        Some(Value::Object(_) | Value::Array(_)) => Err(SubsetLimit::VersionDiagnostic),
    }
}
