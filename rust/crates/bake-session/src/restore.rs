//! Development-only restoration of a stored plain current-format log, as
//! TypeScript's production read path restores one for `readColdSessionLog` and
//! `SessionStore.prepare`.
//!
//! For a current-format log, the JSONL backend scans the bytes with `scanLog`,
//! accepting a torn or recovered tail, and runs `validateStoredEvents` in
//! `packages/session/session-persistence/src/storage-contract.ts`: one pass
//! refuses an unknown type that is not `ignorable` and a `request/header`
//! whose reason is the retired `fallback`, whichever comes first, and a second
//! pass adopts every event with `adoptSessionEvent`. `readColdSessionLog`
//! appends `interruptedTurnClosers`, and `SessionStore.prepare` restores the
//! result with `Session.fromRestore`, which checks each stored event and
//! closer as Session construction does, without a lossless snapshot, then
//! appends an ordinary `session/end-seed` unless the last event is one.
//! [`restore_plain_log`] runs the same stages in the same order:
//!
//! 1. [`scan_log`], with its documented refusal and native-limit contracts.
//! 2. The unsupported pass over every event.
//! 3. This port's whole-log qualification, which only reports limits.
//! 4. The adoption pass over every event.
//! 5. The closers, built directly from the decoded events: they never pass
//!    the codec, in TypeScript or here.
//! 6. Session construction over the stored events, then the closers, which
//!    folds the surface, request header, and tool history.
//!
//! Neither path runs `restoreReleasedV3Artifact`: a current-format read
//! applies no turn, step, or tool lifecycle validation, and neither does this
//! one. The header was proved by the scan, and the inherited cut is the
//! scan's, which Session construction accepts. Restoration is not Agent
//! resume: it truncates no torn tail, writes no closer or end seed, emits no
//! `resume` header, and checks no path or stored identity. It does not
//! decompress; [`crate::restore_zstd_log`] applies the same post-scan stages
//! to the default compressed format.

use serde_json::{Map, Value};

use crate::repair::interrupted_turn_closers;
use crate::replay::{KNOWN_EVENT_TYPES, ReplayRefusal, admit, adopt, folded, qualify_payload};
use crate::request::RequestFold;
use crate::{
    PathPlatform, ReplayLimit, ScanRefusal, ScannedLog, SeedRejection, UnadmittedEnvelope, scan_log,
};

/// The Session state a stored current-format log restores to.
///
/// The stored log is the scan, kept once: [`ScannedLog::rows`] holds each
/// committed row as parsed, with packed `sourceEventSeqs` ranges, and
/// [`ScannedLog::events`] decodes them, with every range expanded.
/// [`RestoredLog::closers`] follows them. The ordinary end seed that Session
/// construction would append is reported, not stored, because TypeScript
/// stamps it with the current time.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredLog {
    stored: ScannedLog,
    torn: Option<TornTail>,
    closers: Vec<Value>,
    end_seed_appended: bool,
    fold: RequestFold,
    context: Option<Map<String, Value>>,
}

/// Recovery metadata in the physical input and the restored row sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TornTail {
    /// Physical file offset at which a writer would truncate the torn tail.
    pub truncate_to: usize,
    /// Index in [`ScannedLog::rows`] where recovered rows start. Plain input
    /// has no recovered rows, so this equals its stored row count.
    pub recovered_from: usize,
}

impl RestoredLog {
    /// The scanned header, inherited cut, committed byte offset, and stored
    /// events, including complete rows recovered from a torn Zstd frame.
    pub const fn stored(&self) -> &ScannedLog {
        &self.stored
    }

    /// Physical truncation offset and the start of recovered stored rows.
    /// This reader never performs the truncation or rewrites those rows.
    pub const fn torn(&self) -> Option<TornTail> {
        self.torn
    }

    /// The synthetic events `interruptedTurnClosers` appends after the stored
    /// events, as exact JSON with continuing seqs. Empty when the last turn
    /// ended.
    pub fn closers(&self) -> &[Value] {
        &self.closers
    }

    /// Whether Session construction appends an ordinary `session/end-seed`
    /// after the closers: unless the last stored event or closer is one.
    pub const fn end_seed_appended(&self) -> bool {
        self.end_seed_appended
    }

    /// `deriveMessages`: each current surface node's message, as logged.
    pub fn messages(&self) -> Vec<Value> {
        self.fold.messages().cloned().map(Value::Object).collect()
    }

    /// `requestHeader`: the latest header in `canonicalHeader` form, or
    /// `None` before the first.
    pub fn request_header(&self) -> Option<Value> {
        self.fold.request_header()
    }

    /// `toolHistory`: the `ToolHistoryProjection` snapshot.
    pub fn tool_history(&self) -> Value {
        self.fold.tool_history_json()
    }

    /// `requestContext`: the latest `request/context` data, or `None` before
    /// the first.
    pub const fn request_context(&self) -> Option<&Map<String, Value>> {
        self.context.as_ref()
    }
}

/// Why a plain or compressed current-format log restored nothing.
///
/// `Scan` and `Zstd` have their respective reader contracts. `Unsupported`,
/// `Stored`, and `Restore` claim the TypeScript refusal layer and seq, not
/// its message. Both native-only variants make no TypeScript outcome claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreRefusal {
    /// `scanLog` throws; see [`ScanRefusal`].
    Scan(ScanRefusal),
    /// Compressed framing or decoding refused the log, as the production
    /// Zstd reader does. Plaintext scan failures use `Scan`.
    Zstd(crate::ZstdRefusal),
    /// The cumulative decoded bytes would exceed the caller's budget.
    /// Native-only: no TypeScript outcome or refusal precedence is claimed.
    /// No event seq exists for a refusal while decoding the header.
    NativePlaintextBudget { max_plaintext_bytes: usize },
    /// `validateStoredEvents` throws `SessionFormatUnsupportedError` for row
    /// `seq`, the first such row.
    Unsupported { seq: u64, cause: Unsupported },
    /// `validateStoredEvents` throws `SessionPersistenceCorruptionError`
    /// because adoption rejects row `seq`, the first it rejects, even when Session
    /// construction would refuse an earlier row.
    Stored { seq: u64, rejection: SeedRejection },
    /// Session construction rejects event `seq`, a stored event or a closer.
    Restore { seq: u64, rejection: SeedRejection },
    /// This port cannot reproduce the outcome; nothing is claimed. `seq`
    /// names the row.
    NativeSubset { seq: u64, limit: RestoreLimit },
}

/// Why `validateStoredEvents` refuses to interpret a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A type outside the 60 known types without `ignorable`.
    UnknownType,
    /// A `request/header` whose `reason` is `fallback`.
    FallbackHeader,
}

/// Input this port does not restore, whatever TypeScript does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreLimit {
    /// `image/offload`, which needs its message projection.
    EventType,
    /// A known type carrying `ignorable`. TypeScript admits most of them, but
    /// refuses a tool update after its other checks.
    Ignorable,
    /// A surface message, request header, tool update, or `request/context`
    /// payload holds a number other than a safe integer and not -0, even
    /// where no output carries it, such as usage.
    Number,
    /// One of those payloads nests arrays and objects more than 64
    /// containers deep.
    Depth,
    /// An Assistant settlement's `turn` or `step` is a positive number with a
    /// fraction or exponent, or a closer would copy an open turn or step that
    /// is not a safe count.
    Coordinate,
    /// A header's `config` holds a member outside `LlmCallConfig`.
    ConfigMember,
    /// A header's `tools` is present but not an array of objects.
    ToolSchema,
    /// `request/context` data is not an object, which `requestContext`
    /// spreads into one.
    Context,
    /// Computing the closers would throw or copy a non-string call id: `null`
    /// data in a `turn/start`, `step/start`, or `tool/call`, a `null` content
    /// block, or a pending tool call whose id is not a string.
    Repair,
}

/// Restore a stored plain current-format log, as the production read path
/// does. `log` may end with a torn or recovered tail, which is left out, and
/// `source_budget` bounds each row's expanded `sourceEventSeqs` as in
/// [`scan_log`].
pub fn restore_plain_log(
    log: &[u8],
    platform: PathPlatform,
    source_budget: usize,
) -> Result<RestoredLog, RestoreRefusal> {
    let stored = scan_log(log, platform, source_budget).map_err(RestoreRefusal::Scan)?;
    let torn = (stored.committed_bytes() < log.len()).then_some(TornTail {
        truncate_to: stored.committed_bytes(),
        recovered_from: stored.rows().len(),
    });
    restore_scanned(stored, torn)
}

pub(crate) fn restore_scanned(
    stored: ScannedLog,
    torn: Option<TornTail>,
) -> Result<RestoredLog, RestoreRefusal> {
    let envelopes: Vec<UnadmittedEnvelope<'_>> = stored
        .events()
        .map(|event| event.envelope().clone())
        .collect();
    for envelope in &envelopes {
        let seq = envelope.seq;
        if !KNOWN_EVENT_TYPES.contains(&envelope.event_type) && !envelope.ignorable {
            let cause = Unsupported::UnknownType;
            return Err(RestoreRefusal::Unsupported { seq, cause });
        }
        if envelope.event_type == "request/header" && envelope.data["reason"] == "fallback" {
            let cause = Unsupported::FallbackHeader;
            return Err(RestoreRefusal::Unsupported { seq, cause });
        }
    }
    for envelope in &envelopes {
        qualify(envelope).map_err(|limit| native(envelope.seq, limit))?;
    }
    for envelope in &envelopes {
        adopt(envelope).map_err(|rejection| RestoreRefusal::Stored {
            seq: envelope.seq,
            rejection,
        })?;
    }
    let closers =
        interrupted_turn_closers(&envelopes).map_err(|(seq, limit)| native(seq, limit))?;
    let closer_envelopes: Vec<UnadmittedEnvelope<'_>> =
        closers.iter().map(closer_envelope).collect();
    let mut fold = RequestFold::new(stored.header().id.clone());
    for envelope in envelopes.iter().chain(&closer_envelopes) {
        let seq = envelope.seq;
        admit(envelope)
            .and_then(|fact| fold.append(fact).map_err(|refusal| folded(seq, refusal)))
            .map_err(restore_refusal)?;
    }
    let end_seed_appended = closer_envelopes
        .last()
        .or(envelopes.last())
        .is_none_or(|last| last.event_type != "session/end-seed");
    let context = envelopes
        .iter()
        .rev()
        .find(|envelope| envelope.event_type == "request/context")
        .map(|envelope| {
            envelope
                .data
                .as_object()
                .expect("qualified context")
                .clone()
        });
    drop(closer_envelopes);
    drop(envelopes);
    Ok(RestoredLog {
        stored,
        torn,
        closers,
        end_seed_appended,
        fold,
        context,
    })
}

/// This port's whole-log qualification: `image/offload`, `ignorable` on a
/// known type, and `request/context` data, which restoration projects but
/// Session construction never checks.
fn qualify(envelope: &UnadmittedEnvelope<'_>) -> Result<(), RestoreLimit> {
    let event_type = envelope.event_type;
    if event_type == "image/offload" {
        return Err(RestoreLimit::EventType);
    }
    if envelope.ignorable && KNOWN_EVENT_TYPES.contains(&event_type) {
        return Err(RestoreLimit::Ignorable);
    }
    if event_type == "request/context" {
        if !envelope.data.is_object() {
            return Err(RestoreLimit::Context);
        }
        qualify_payload(envelope.data).map_err(limit)?;
    }
    Ok(())
}

/// A closer's envelope, read from the exact JSON [`interrupted_turn_closers`]
/// built.
fn closer_envelope(closer: &Value) -> UnadmittedEnvelope<'_> {
    UnadmittedEnvelope {
        event_type: closer["type"].as_str().expect("closer type"),
        seq: closer["seq"].as_u64().expect("closer seq"),
        time: closer["time"].as_i64().expect("closer time"),
        ignorable: false,
        source_event_seqs: closer.get("sourceEventSeqs").map(|seqs| {
            seqs.as_array()
                .expect("closer sources")
                .iter()
                .map(|seq| seq.as_u64().expect("closer source"))
                .collect()
        }),
        surface_op: closer.get("surfaceOp"),
        data: &closer["data"],
    }
}

const fn native(seq: u64, limit: RestoreLimit) -> RestoreRefusal {
    RestoreRefusal::NativeSubset { seq, limit }
}

/// A Session construction outcome of the shared admission.
fn restore_refusal(refusal: ReplayRefusal) -> RestoreRefusal {
    match refusal {
        ReplayRefusal::Seed { seq, rejection } => RestoreRefusal::Restore { seq, rejection },
        ReplayRefusal::NativeSubset { seq, limit: replay } => native(seq, limit(replay)),
        other => unreachable!("admission refuses only seed checks and limits: {other:?}"),
    }
}

/// The restoration limit for one of the shared admission's.
const fn limit(limit: ReplayLimit) -> RestoreLimit {
    match limit {
        ReplayLimit::EventType => RestoreLimit::EventType,
        ReplayLimit::Ignorable => RestoreLimit::Ignorable,
        ReplayLimit::Number => RestoreLimit::Number,
        ReplayLimit::Depth => RestoreLimit::Depth,
        ReplayLimit::Coordinate | ReplayLimit::RepeatedCoordinate => RestoreLimit::Coordinate,
        ReplayLimit::ConfigMember => RestoreLimit::ConfigMember,
        ReplayLimit::ToolSchema => RestoreLimit::ToolSchema,
    }
}
