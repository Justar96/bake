//! `bake-rs session inspect`: restore one Session log, read-only, and
//! describe it as one JSON record.
//!
//! An explicitly named file's canonical name selects plain or Zstd decoding,
//! and only a current-format name is opened; that form checks no stored
//! identity or generation. The lookup form, in [`crate::lookup`], finds the
//! file by root and id first. Either read is bounded by `--max-bytes` and
//! refused when the opened file's observed metadata changes while it is read.
//! This is a diagnostic, not a production reader: it takes no lease and never
//! truncates, repairs, or writes the log; the lookup form migrates an older
//! generation in memory only.

use std::ffi::OsString;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use bake_session::{
    CURRENT_SESSION_FORMAT_VERSION, EnvelopeLimit, HeaderOrigin, HeaderRefusal, OffloadRejection,
    PathPlatform, Rejection, RestoreLimit, RestoreRefusal, RestoredLog, ScanLimit, ScanRefusal,
    SeedRejection, SourceEventSeqsLimit, StagedLog, SubsetLimit, Unsupported, V3Limit, ZstdRefusal,
    dismantle, stage_plain_log, stage_zstd_log,
};
use serde_json::{Value, json};

/// The largest budget accepted, 2^53 − 1, so every printed count is a safe
/// integer for TypeScript consumers.
pub const MAX_BUDGET: u64 = (1 << 53) - 1;

/// The parsed operands of `session inspect`.
#[derive(Debug, PartialEq, Eq)]
pub struct InspectArgs {
    /// Bounds the file's size and, for Zstd, its cumulative decoded size.
    pub max_bytes: u64,
    /// Bounds each event's expanded `sourceEventSeqs`.
    pub max_source_seqs: u64,
    pub target: Target,
}

/// Which Session log `session inspect` reads.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// One explicitly named log file; its name selects the decoder.
    File(OsString),
    /// The Session with this id in a root, found as the JSONL backend finds it.
    Lookup(LookupArgs),
}

/// The operands of the lookup form.
#[derive(Debug, PartialEq, Eq)]
pub struct LookupArgs {
    pub root: OsString,
    /// Non-empty UTF-8.
    pub id: String,
    pub encoding: Encoding,
    /// Bounds the directory entries the lookup reads.
    pub max_entries: u64,
}

/// What a canonical log name selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    None,
    Zstd,
}

impl Encoding {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zstd => "zstd",
        }
    }
}

/// How a basename parses as a Session log name.
#[derive(Debug, PartialEq, Eq)]
pub enum LogName {
    /// A canonical name of the given format version and encoding.
    Canonical {
        version: u64,
        encoding: Encoding,
    },
    NotCanonical,
}

/// Parse a basename as TypeScript's `parseGenerationLogFilename` does for
/// either encoding: `session.jsonl` is version 0, `session.vN.jsonl` is
/// version N for a safe integer N without a leading zero, and `.zstd` may
/// follow either.
pub fn parse_log_name(name: &str) -> LogName {
    let (stem, encoding) = match name.strip_suffix(".zstd") {
        Some(stem) => (stem, Encoding::Zstd),
        None => (name, Encoding::None),
    };
    let Some(rest) = stem
        .strip_prefix("session")
        .and_then(|rest| rest.strip_suffix(".jsonl"))
    else {
        return LogName::NotCanonical;
    };
    if rest.is_empty() {
        return LogName::Canonical {
            version: 0,
            encoding,
        };
    }
    match rest.strip_prefix(".v").and_then(parse_count) {
        Some(version) => LogName::Canonical { version, encoding },
        None => LogName::NotCanonical,
    }
}

/// A positive decimal integer of at most [`MAX_BUDGET`], spelled with ASCII
/// digits and no sign, separator, or leading zero.
pub fn parse_count(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.first().is_none_or(|first| *first == b'0') || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|count| *count <= MAX_BUDGET)
}

/// The command's outcome: the record to print and the exit status, or a
/// one-line diagnostic for stderr with exit status 1.
pub enum Outcome {
    Record { json: String, status: u8 },
    Failure(String),
}

/// Run `session inspect` without writing to any stream.
pub fn inspect(args: &InspectArgs) -> Outcome {
    match run(args) {
        Ok((record, status)) => Outcome::Record {
            json: bake_session::json_text(&record),
            status,
        },
        Err(message) => Outcome::Failure(message),
    }
}

fn run(args: &InspectArgs) -> Result<(Value, u8), String> {
    let path = match &args.target {
        Target::File(path) => Path::new(path),
        Target::Lookup(lookup) => return crate::lookup::run(args, lookup),
    };
    let encoding = canonical_encoding(path)?;
    let bytes = read_bounded(path, args.max_bytes)?;
    let restored = stage(&bytes, encoding, args).and_then(StagedLog::restore);
    let mut record = json!({
        "status": if restored.is_ok() { "restored" } else { "refused" },
        "encoding": encoding.label(),
        "formatVersion": CURRENT_SESSION_FORMAT_VERSION,
        "fileBytes": bytes.len(),
    });
    let fields = record.as_object_mut().expect("record object");
    match restored {
        Ok(restored) => {
            fields.extend(restored_fields(&restored));
            Ok((record, 0))
        }
        Err(refusal) => {
            fields.insert("refusal".into(), refusal_fields(&refusal, args));
            Ok((record, 3))
        }
    }
}

/// Decode and scan a current-format log under the command's budgets, without
/// validating its events.
pub(crate) fn stage(
    bytes: &[u8],
    encoding: Encoding,
    args: &InspectArgs,
) -> Result<StagedLog, RestoreRefusal> {
    let source_budget = usize::try_from(args.max_source_seqs).unwrap_or(usize::MAX);
    let platform = PathPlatform::host();
    match encoding {
        Encoding::None => stage_plain_log(bytes, platform, source_budget),
        Encoding::Zstd => stage_zstd_log(bytes, platform, source_budget, plaintext_budget(args)),
    }
}

/// `--max-bytes` as the cumulative decoded plaintext budget.
pub(crate) fn plaintext_budget(args: &InspectArgs) -> usize {
    usize::try_from(args.max_bytes).unwrap_or(usize::MAX)
}

/// The encoding a current-format name selects; any other name is refused
/// before the file is opened.
fn canonical_encoding(path: &Path) -> Result<Encoding, String> {
    let name = path.file_name().and_then(|name| name.to_str());
    match name.map_or(LogName::NotCanonical, parse_log_name) {
        LogName::Canonical { version, encoding } if version == CURRENT_SESSION_FORMAT_VERSION => {
            Ok(encoding)
        }
        LogName::Canonical { version, .. } if version < CURRENT_SESSION_FORMAT_VERSION => {
            Err(format!(
                "{path:?} is a format v{version} Session log; this preview reads only format \
                 {CURRENT_SESSION_FORMAT_VERSION} and does not migrate older logs"
            ))
        }
        LogName::Canonical { version, .. } => Err(format!(
            "{path:?} is a format v{version} Session log, newer than the format \
             {CURRENT_SESSION_FORMAT_VERSION} this preview reads"
        )),
        LogName::NotCanonical => Err(format!(
            "{path:?} is not a canonical Session log name; expected session.v3.jsonl or \
             session.v3.jsonl.zstd"
        )),
    }
}

/// Read at most `max_bytes` from the regular file at `path`, refusing a
/// larger file or one whose observed metadata changes during the read.
pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    read_bounded_or_missing(path, max_bytes).map_err(ReadFailure::into_message)
}

/// Why [`read_bounded_or_missing`] read nothing.
pub(crate) enum ReadFailure {
    /// Opening the path reported that it does not exist, as for a dangling link.
    Missing(String),
    Other(String),
}

impl ReadFailure {
    pub(crate) fn into_message(self) -> String {
        match self {
            Self::Missing(message) | Self::Other(message) => message,
        }
    }
}

/// [`read_bounded`], keeping an absent path apart from every other failure
/// by its I/O error kind. The diagnostics are the same.
pub(crate) fn read_bounded_or_missing(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ReadFailure> {
    let file = open_read_only(path).map_err(|error| {
        if std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            ReadFailure::Other(not_regular(path))
        } else if error.kind() == io::ErrorKind::NotFound {
            ReadFailure::Missing(format!("cannot open {path:?}: {error}"))
        } else {
            ReadFailure::Other(format!("cannot open {path:?}: {error}"))
        }
    })?;
    read_open(path, &file, max_bytes).map_err(ReadFailure::Other)
}

/// Read the opened file within `max_bytes`, refusing an irregular file or
/// one whose observed metadata changes during the read.
fn read_open(path: &Path, file: &File, max_bytes: u64) -> Result<Vec<u8>, String> {
    let read_error = |error: io::Error| format!("cannot read {path:?}: {error}");
    let before = file.metadata().map_err(read_error)?;
    if !before.is_file() {
        return Err(not_regular(path));
    }
    if before.len() > max_bytes {
        return Err(format!(
            "{path:?} is {} bytes, over --max-bytes {max_bytes}",
            before.len()
        ));
    }
    // The buffer grows only as bytes arrive; reading one byte past the budget
    // detects growth.
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    let after = file.metadata().map_err(read_error)?;
    if bytes.len() as u64 != after.len() || Stamp::of(&before) != Stamp::of(&after) {
        return Err(format!("{path:?} changed while it was read"));
    }
    Ok(bytes)
}

fn not_regular(path: &Path) -> String {
    format!("{path:?} is not a regular file")
}

/// Open read-only. On Unix the open does not block, so a FIFO is refused by
/// the regular-file check instead of waiting for a writer.
pub(crate) fn open_read_only(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

/// The observed metadata compared before and after the read. It detects
/// ordinary modification; it is not an atomic snapshot.
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Stamp {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

pub(crate) fn restored_fields(restored: &RestoredLog) -> serde_json::Map<String, Value> {
    let stored = restored.stored();
    let header = stored.header();
    let torn = restored.torn().map(|tail| {
        json!({
            "truncateTo": tail.truncate_to,
            "recoveredFrom": tail.recovered_from,
            "recoveredEventCount": stored.rows().len() - tail.recovered_from,
        })
    });
    let closer_types: Vec<&Value> = restored
        .closers()
        .iter()
        .map(|closer| &closer["type"])
        .collect();
    // Restored messages and the request header may nest as deep as their log
    // rows, so they are dismantled rather than dropped recursively.
    let messages = restored.messages();
    let message_count = messages.len();
    messages.into_iter().for_each(dismantle);
    let request_header = restored.request_header();
    let has_request_header = request_header.is_some();
    request_header.into_iter().for_each(dismantle);
    let fields = json!({
        "header": {
            "id": header.id,
            "createdAt": header.created_at,
            "cwd": header.cwd,
            "parentSession": header.parent_session,
            "isSeeded": header.is_seeded,
            "origin": header.origin.map(|HeaderOrigin::Subagent| "subagent"),
            "delegationDepth": header.delegation_depth,
            "agentPreset": header.agent_preset,
        },
        "storedEventCount": stored.rows().len(),
        "committedPlaintextBytes": stored.committed_bytes(),
        "inheritedEventCount": stored.inherited_event_count(),
        "torn": torn,
        "repair": {
            "closerTypes": closer_types,
            "endSeedAppended": restored.end_seed_appended(),
        },
        "projection": {
            "messageCount": message_count,
            "hasRequestHeader": has_request_header,
            "hasRequestContext": restored.request_context().is_some(),
        },
    });
    let Value::Object(fields) = fields else {
        unreachable!("json! object")
    };
    fields
}

/// How a refusal is classified for the record's `kind`.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// The log is malformed or fails a check the production reader applies.
    Invalid,
    /// The production reader refuses to interpret the log.
    Unsupported,
    /// This preview cannot reproduce the production outcome; none is claimed.
    NativeLimit,
    /// No Session with the requested id exists in the root.
    NotFound,
}

impl Kind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Unsupported => "unsupported",
            Self::NativeLimit => "native-limit",
            Self::NotFound => "not-found",
        }
    }
}

pub(crate) struct Refusal {
    pub(crate) kind: Kind,
    pub(crate) message: String,
    /// An event record's line, counted from 1 after the header record.
    pub(crate) line: Option<u64>,
    pub(crate) seq: Option<u64>,
    /// A physical byte offset in the file.
    pub(crate) offset: Option<usize>,
}

impl Refusal {
    pub(crate) const fn new(kind: Kind, message: String) -> Self {
        Self {
            kind,
            message,
            line: None,
            seq: None,
            offset: None,
        }
    }
}

fn refusal_fields(refusal: &RestoreRefusal, args: &InspectArgs) -> Value {
    let refusal = describe(refusal, args);
    json!({
        "kind": refusal.kind.label(),
        "message": refusal.message,
        "line": refusal.line,
        "seq": refusal.seq,
        "offset": refusal.offset,
    })
}

pub(crate) fn describe(refusal: &RestoreRefusal, args: &InspectArgs) -> Refusal {
    use Kind::{Invalid, NativeLimit};
    match refusal {
        RestoreRefusal::Scan(scan) => describe_scan(scan, args),
        RestoreRefusal::Zstd(zstd) => Refusal {
            offset: match *zstd {
                ZstdRefusal::Magic { offset }
                | ZstdRefusal::ReservedHeaderBit { offset }
                | ZstdRefusal::ReservedBlockType { offset }
                | ZstdRefusal::Frame { start: offset } => Some(offset),
                ZstdRefusal::Empty
                | ZstdRefusal::HeaderFrame
                | ZstdRefusal::CompleteFramesUncommitted => None,
            },
            ..Refusal::new(Invalid, zstd.message())
        },
        RestoreRefusal::NativePlaintextBudget { .. } => Refusal::new(
            NativeLimit,
            format!("decoded plaintext exceeds --max-bytes {}", args.max_bytes),
        ),
        RestoreRefusal::Unsupported { seq, cause } => Refusal {
            seq: Some(*seq),
            ..Refusal::new(
                Kind::Unsupported,
                match cause {
                    Unsupported::UnknownType => {
                        "an event type unknown to this preview is not marked ignorable"
                    }
                    Unsupported::FallbackHeader => {
                        "a request/header event uses the retired fallback reason"
                    }
                }
                .into(),
            )
        },
        RestoreRefusal::Stored { seq, rejection } => Refusal {
            seq: Some(*seq),
            ..Refusal::new(
                Invalid,
                format!("a stored event failed validation ({})", check(*rejection)),
            )
        },
        RestoreRefusal::Restore { seq, rejection } => Refusal {
            seq: Some(*seq),
            ..Refusal::new(
                Invalid,
                format!(
                    "Session construction rejected an event ({})",
                    check(*rejection)
                ),
            )
        },
        RestoreRefusal::NativeSubset { seq, limit } => Refusal {
            seq: Some(*seq),
            ..Refusal::new(NativeLimit, restore_limit(*limit).into())
        },
    }
}

fn describe_scan(scan: &ScanRefusal, args: &InspectArgs) -> Refusal {
    use Kind::{Invalid, NativeLimit};
    let message = scan.message();
    match scan {
        ScanRefusal::Header(header) => match header {
            HeaderRefusal::UnsupportedVersion { newer } => Refusal::new(
                Kind::Unsupported,
                format!(
                    "the header records {} Session format than this preview's format \
                     {CURRENT_SESSION_FORMAT_VERSION}",
                    if *newer { "a newer" } else { "an older" }
                ),
            ),
            HeaderRefusal::Rejected(rejection) => Refusal::new(
                Invalid,
                match rejection {
                    Rejection::Framing => "empty or header-less session log",
                    Rejection::Json => "corrupt session log: header line is not valid JSON",
                    Rejection::NotObject => "corrupt session log: first line is not a JSON object",
                    Rejection::RetiredPolicyFields => {
                        "session header uses retired policy baseline fields"
                    }
                    Rejection::NotSessionHeader => {
                        "corrupt session log: first line is not a session header"
                    }
                }
                .into(),
            ),
            HeaderRefusal::NativeSubset(limit) => {
                Refusal::new(NativeLimit, header_limit(*limit).into())
            }
        },
        ScanRefusal::Structural { line, .. } => Refusal {
            line: Some(*line),
            ..Refusal::new(
                Invalid,
                message.unwrap_or_else(|| "an event record is structurally invalid".into()),
            )
        },
        ScanRefusal::Unsupported { line, .. } => Refusal {
            line: Some(*line),
            ..Refusal::new(
                Kind::Unsupported,
                message.unwrap_or_else(|| "an event record is not supported".into()),
            )
        },
        ScanRefusal::Corrupt { line, .. } => Refusal {
            line: Some(*line),
            ..Refusal::new(
                Invalid,
                message.unwrap_or_else(|| "corrupt session log: invalid committed event".into()),
            )
        },
        ScanRefusal::Finish(rejection) => Refusal::new(Invalid, rejection.message().into()),
        ScanRefusal::NativeSubset { line, limit } => Refusal {
            line: Some(*line),
            ..Refusal::new(NativeLimit, scan_limit(*limit, args))
        },
    }
}

/// Why this preview cannot decide a session header's outcome.
pub(crate) const fn header_limit(limit: SubsetLimit) -> &'static str {
    match limit {
        SubsetLimit::InvalidUtf8 => "this preview requires valid UTF-8 in the session header",
        SubsetLimit::JsonParser => {
            "the session header holds a number beyond the double range, which this preview \
             does not read"
        }
        SubsetLimit::FloatLexeme => {
            "a session header count is written with a fraction or an exponent, or does not fit \
             in 64 bits, which this preview does not read"
        }
        SubsetLimit::VersionDiagnostic => {
            "this preview cannot report a header of another format version whose id is an \
             object or array"
        }
    }
}

fn scan_limit(limit: ScanLimit, args: &InspectArgs) -> String {
    let number = "an event's seq, time, or count is written with a fraction or an exponent, \
                  or does not fit in 64 bits, which this preview does not read";
    match limit {
        ScanLimit::InvalidUtf8 => "this preview requires valid UTF-8 in event records",
        ScanLimit::JsonParser => {
            "an event record holds a number beyond the double range, which this preview does \
             not read"
        }
        ScanLimit::NumberLexeme => "this preview reads numbers of at most 768 integer digits",
        ScanLimit::EventCount => "this preview reads at most 2^53 - 1 events",
        ScanLimit::Codec(codec) => match codec {
            V3Limit::Envelope(EnvelopeLimit::Source(SourceEventSeqsLimit::OutputBudget)) => {
                return format!(
                    "an event's sourceEventSeqs expand past --max-source-seqs {}",
                    args.max_source_seqs
                );
            }
            V3Limit::Envelope(
                EnvelopeLimit::FloatLexeme(_)
                | EnvelopeLimit::Source(SourceEventSeqsLimit::FloatLexeme { .. }),
            )
            | V3Limit::FloatLexeme(_) => number,
            V3Limit::Envelope(EnvelopeLimit::NegativeZeroSeq) => "an event's seq is -0",
            V3Limit::Envelope(EnvelopeLimit::SeqDiagnostic) | V3Limit::ObsoleteSeqDiagnostic => {
                "this preview cannot report an event whose seq is an array, an object, or \
                 not a safe integer"
            }
            V3Limit::SystemPayload => {
                "a system/message payload is outside the shape this preview reads"
            }
        },
    }
    .into()
}

const fn restore_limit(limit: RestoreLimit) -> &'static str {
    match limit {
        RestoreLimit::Number => "a projected payload holds a number other than a safe integer",
        RestoreLimit::Coordinate => "a turn or step coordinate is not a safe count",
        RestoreLimit::ToolSchema => "a request header's tools are not an array of objects",
        RestoreLimit::Context => "request/context data is not an object",
        RestoreLimit::Repair => "the closers for an interrupted turn cannot be built",
        RestoreLimit::Projection => {
            "an image/offload target's content holds a null block or a tool-result block whose \
             content is not an array"
        }
    }
}

/// The Session construction check, named as the shared restoration cases name it.
const fn check(rejection: SeedRejection) -> &'static str {
    match rejection {
        SeedRejection::LosslessJson => "lossless-json",
        SeedRejection::MessageIdentity => "message-identity",
        SeedRejection::MessageRole => "message-role",
        SeedRejection::MessageSource => "message-source",
        SeedRejection::MessageContent => "message-content",
        SeedRejection::ModelSource => "model-source",
        SeedRejection::ToolSource => "tool-source",
        SeedRejection::ToolResultBlock => "tool-result-block",
        SeedRejection::ToolCallId => "tool-call-id",
        SeedRejection::Settlement => "settlement",
        SeedRejection::HeaderProviderModel => "header-provider-model",
        SeedRejection::HeaderReasoningEffort => "header-reasoning-effort",
        SeedRejection::HeaderAdapterDefaults => "header-adapter-defaults",
        SeedRejection::HeaderReason => "header-reason",
        SeedRejection::HeaderStartsSeries => "header-starts-series",
        SeedRejection::ToolUpdateData => "tool-update-data",
        SeedRejection::ToolUpdateRequired => "tool-update-required",
        SeedRejection::ProjectionRequired => "projection-required",
        SeedRejection::ImageOffload(rejection) => match rejection {
            OffloadRejection::Data => "image-offload-data",
            OffloadRejection::Target => "image-offload-target",
            OffloadRejection::DuplicateTarget { .. } => "image-offload-duplicate",
            OffloadRejection::NotCurrent { .. } => "image-offload-not-current",
            OffloadRejection::TargetType { .. } => "image-offload-target-type",
            OffloadRejection::ImageIndexes => "image-offload-indexes",
            OffloadRejection::AlreadyOffloaded { .. } => "image-offload-already-offloaded",
            OffloadRejection::MissingIndex { .. } => "image-offload-missing-index",
        },
        SeedRejection::NonSurfaceMarker => "non-surface-marker",
        SeedRejection::ReplaceStart => "replace-start",
        SeedRejection::ReplaceEnd => "replace-end",
        SeedRejection::ReplaceOrder => "replace-order",
        SeedRejection::ReplaceSources => "replace-sources",
        SeedRejection::ToolResultSpan => "tool-result-span",
        SeedRejection::ToolResultTarget => "tool-result-target",
        SeedRejection::ToolResultRest => "tool-result-rest",
        SeedRejection::SystemHead => "system-head",
        SeedRejection::ToolUpdateHeader => "tool-update-header",
        SeedRejection::ToolUpdateStale => "tool-update-stale",
        SeedRejection::ToolUpdateBaseline => "tool-update-baseline",
        SeedRejection::ToolUpdateChange => "tool-update-change",
        SeedRejection::ToolUpdateAnchor => "tool-update-anchor",
    }
}
