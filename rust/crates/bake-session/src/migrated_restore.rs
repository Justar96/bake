//! Development-only restoration of a released v0, v1, or v2 Session after its
//! migration to format v3, as TypeScript's production read path restores one
//! for `readColdSessionLog` and `SessionStore.prepare`.
//!
//! For a historical generation, the JSONL backend's read `open` decodes the
//! source file through the format chain, runs the catalog's final check of the
//! transformed artifact (`restoreReleasedV3Artifact`), checks the stored
//! identity, and runs `validateStoredEvents` over the migrated events in
//! memory. A read publishes nothing: the source file stays as it was and no
//! current generation is written. `readColdSessionLog` appends
//! `interruptedTurnClosers`, and `SessionStore.prepare` restores the result
//! with `Session.fromRestore`.
//!
//! [`restore_migrated`] takes the migration's output, a [`MigratedV2`] from
//! [`migrate_v2_rows`](crate::migrate_v2_rows) or
//! [`migrate_released_history`](crate::migrate_released_history), encodes
//! its header and events as the current writer would with
//! [`encode_header_line`] and [`encode_event_line`], and restores those bytes
//! with [`scan_log`] and the stages of [`StagedLog::restore`].
//! TypeScript neither encodes nor scans the migrated events, so a refusal of
//! either is a native limit. The encoder writes every row in a form the scan
//! decodes back to the same event, so on admitted input the restored state is
//! the one TypeScript folds from the artifact in memory.
//!
//! The final check is not ported. A Session it refuses, or whose stored id
//! or `cwd` names another path, is outside this function's domain: the
//! result claims nothing about it.
//! Nothing is read from or written to a file.

use crate::{
    EncodeRefusal, MigratedV2, PathPlatform, RestoreLimit, RestoreRefusal, RestoredLog,
    ScanRefusal, StagedLog, encode_event_line, encode_header_line, scan_log,
};

/// Why a migrated Session restored nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigratedRestoreRefusal {
    /// `validateStoredEvents` or Session construction refused the migrated
    /// events or the closers: [`RestoreRefusal::Unsupported`],
    /// [`RestoreRefusal::Stored`], or [`RestoreRefusal::Restore`]. TypeScript
    /// refuses the read or the restore too, but its final check runs first
    /// and may refuse with its own class and message, so only that
    /// TypeScript refuses is claimed.
    Refused(RestoreRefusal),
    /// This crate does not reproduce the TypeScript outcome; nothing is claimed.
    NativeSubset(MigratedRestoreLimit),
}

/// Input whose TypeScript outcome this port does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigratedRestoreLimit {
    /// The migrated header (`event` is `None`) or the migrated event at that
    /// index did not encode. TypeScript restores the artifact in memory and
    /// never encodes it on a read.
    Encode {
        event: Option<usize>,
        refusal: EncodeRefusal,
    },
    /// The scan refused the encoded log, which TypeScript never scans on a
    /// read: an expanded `sourceEventSeqs` list beyond the caller's budget,
    /// or any other scan refusal of rows the encoder admitted.
    Scan(ScanRefusal),
    /// The scan accepted the encoded log but did not read it back whole: it
    /// stopped before the last encoded event, as it does at the first event
    /// whose `seq` is not its index, or found an inherited cut other than the
    /// migration's. Output of [`migrate_v2_rows`](crate::migrate_v2_rows) and
    /// [`migrate_released_history`](crate::migrate_released_history) never
    /// does this; a hand-built [`MigratedV2`] may.
    Rescan,
    /// A limit of the restoration stages at row `seq`; see [`RestoreLimit`].
    Restore { seq: u64, limit: RestoreLimit },
}

impl MigratedRestoreLimit {
    /// The limit's name in `conformance/session/migrated-restore-cases.json`:
    /// `encode`, `scan`, `rescan`, or a restoration limit prefixed with
    /// `restore/` and named as `conformance/session/restore-cases.json` names
    /// it.
    pub fn name(&self) -> String {
        match self {
            Self::Encode { .. } => "encode".to_owned(),
            Self::Scan(_) => "scan".to_owned(),
            Self::Rescan => "rescan".to_owned(),
            Self::Restore { limit, .. } => format!("restore/{}", restore_limit_name(*limit)),
        }
    }
}

const fn restore_limit_name(limit: RestoreLimit) -> &'static str {
    match limit {
        RestoreLimit::Number => "number",
        RestoreLimit::Depth => "depth",
        RestoreLimit::Coordinate => "coordinate",
        RestoreLimit::ConfigMember => "config-member",
        RestoreLimit::ToolSchema => "tool-schema",
        RestoreLimit::Context => "context",
        RestoreLimit::Repair => "repair",
        RestoreLimit::Projection => "projection",
    }
}

/// Restore a migrated Session as the production read path restores the
/// historical file it came from.
///
/// The header is encoded with the migration's inherited cut, each event as
/// one row, every record followed by LF, and the bytes are scanned with
/// `platform` and `source_budget` as in
/// [`stage_plain_log`](crate::stage_plain_log). The scan must read back every
/// encoded event and the migration's inherited cut, or the result is the
/// `rescan` limit; its header, inherited cut, and rows are then the restored
/// log's stored state, with no torn tail. [`encode_header_line`] checks `cwd`
/// with this host's path rules, so a `cwd` only `platform` admits is the `encode` limit. The
/// input is never modified.
pub fn restore_migrated(
    migrated: &MigratedV2,
    platform: PathPlatform,
    source_budget: usize,
) -> Result<RestoredLog, MigratedRestoreRefusal> {
    let encode = |event: Option<usize>| {
        move |refusal| {
            MigratedRestoreRefusal::NativeSubset(MigratedRestoreLimit::Encode { event, refusal })
        }
    };
    let mut log = encode_header_line(&migrated.header, Some(migrated.inherited_event_count))
        .map_err(encode(None))?
        .into_bytes();
    log.push(b'\n');
    for (index, event) in migrated.events.iter().enumerate() {
        log.extend_from_slice(
            encode_event_line(event)
                .map_err(encode(Some(index)))?
                .as_bytes(),
        );
        log.push(b'\n');
    }
    let stored = scan_log(&log, platform, source_budget).map_err(|refusal| {
        MigratedRestoreRefusal::NativeSubset(MigratedRestoreLimit::Scan(refusal))
    })?;
    // The scan drops every row after one it rejects without refusing, so a
    // shortened or re-cut read would restore another Session.
    if stored.committed_bytes() != log.len()
        || stored.rows().len() != migrated.events.len()
        || stored.inherited_event_count() != migrated.inherited_event_count
    {
        return Err(MigratedRestoreRefusal::NativeSubset(
            MigratedRestoreLimit::Rescan,
        ));
    }
    StagedLog::new(stored, None)
        .restore()
        .map_err(|refusal| match refusal {
            RestoreRefusal::NativeSubset { seq, limit } => {
                MigratedRestoreRefusal::NativeSubset(MigratedRestoreLimit::Restore { seq, limit })
            }
            refusal => MigratedRestoreRefusal::Refused(refusal),
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn restore_migrated_refuses_events_the_scan_does_not_read_back() {
        let header = json!({
            "version": 3,
            "id": "x",
            "createdAt": 0,
            "isSeeded": false,
            "delegationDepth": 0,
        });
        let turn_start =
            |seq: u64| json!({"type": "turn/start", "seq": seq, "time": 0, "data": {"turn": 1}});
        let rescan = Err(MigratedRestoreRefusal::NativeSubset(
            MigratedRestoreLimit::Rescan,
        ));
        let gap = MigratedV2 {
            header: header.clone(),
            events: vec![turn_start(1)],
            inherited_event_count: 0,
        };
        assert_eq!(restore_migrated(&gap, PathPlatform::Posix, 16), rescan);
        let contiguous = MigratedV2 {
            header,
            events: vec![turn_start(0)],
            inherited_event_count: 0,
        };
        let restored = restore_migrated(&contiguous, PathPlatform::Posix, 16)
            .expect("a contiguous migrated log restores");
        assert_eq!(restored.stored().rows().len(), 1);
    }
}
