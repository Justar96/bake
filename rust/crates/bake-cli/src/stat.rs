//! `bake-rs session stat`: observe one Session's header, read-only, as
//! TypeScript's `JsonlSessionPersistence.stat(id)` does in
//! `packages/session/session-persistence-jsonl/src/index.ts`.
//!
//! The root, layout, and lookup stages are those of
//! [`crate::lookup`]; a Session no project holds is absent rather than
//! refused. Only the selected generation's first record is decoded: the first
//! line of a plain log, or the first frame of a Zstd log, decoded by its own
//! format's codec and migrated to current metadata. A missing file, a header
//! without its complete record, or a malformed header is absent. A corrupt
//! first Zstd frame, retired policy fields, a header version other than the
//! file name's, a newer format, and a stored identity other than the
//! selected file are refused. A lower generation is never read in place of
//! the selected one, and events are never decoded or parsed.
//!
//! `--max-header-bytes` bounds both the bytes read from the file and the
//! decoded header record, LF included; one more byte is read to tell a file
//! that ends at the budget from one that continues. TypeScript reads without
//! a bound, so exceeding it is a native limit. Reads are 8 KiB, and a Zstd
//! first frame is checked only at doubling prefix lengths, so event bytes
//! after the header can be read and discarded within the budget: up to one
//! chunk past a plain header, and about as many bytes as the prefix holding
//! a Zstd header frame. `sizeBytes` comes from a separate metadata read that
//! follows links, after the header read, as in TypeScript; the record is an
//! observation, not an atomic snapshot, and
//! reports no file revision. No file is written, locked, or migrated.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use bake_session::{
    CURRENT_SESSION_FORMAT_VERSION, GenerationHeaderRefusal, HeaderOrigin, PathPlatform,
    RestoreRefusal, SessionHeader, read_generation_header_record, zstd_header_record,
};
use serde_json::{Value, json};

use crate::inspect::{Encoding, Kind, LookupArgs, Refusal, header_limit, open_read_only};
use crate::lookup::{Refused, Selected, Step, Stop, check_identity, refused, resolve, select};

/// The parsed operands of `session stat`.
#[derive(Debug, PartialEq, Eq)]
pub struct StatArgs {
    pub lookup: LookupArgs,
    /// Bounds the physical header read and the decoded header record.
    pub max_header_bytes: u64,
}

/// What the selected generation's header read observed.
enum Observed {
    Found { header: SessionHeader, size: u64 },
    Absent,
}

/// What `readGenerationHeader` makes of one selected generation.
pub(crate) enum Header {
    /// The header, migrated to current metadata, with its stored identity
    /// checked against the selected file.
    Found(SessionHeader),
    /// TypeScript's `undefined`: a missing file, a header without its
    /// complete record, or a malformed header.
    Absent,
    /// A refusal TypeScript throws as `SessionFormatUnsupportedError` or
    /// `SessionPersistenceCorruptionError`: a corrupt first Zstd frame or an
    /// unsupported format. `stat` reports it; discovery skips the artifact.
    Isolated(Stop),
}

/// Run `session stat`, returning its record and exit status, or a
/// diagnostic for exit status 1.
pub fn run(args: &StatArgs) -> Result<(Value, u8), String> {
    let lookup = &args.lookup;
    let root = resolve(&lookup.root)?;
    let mut selected_at = None;
    let outcome = select(&root, lookup).and_then(|selected| {
        let Some(selected) = selected else {
            return Ok(Observed::Absent);
        };
        selected_at = Some((selected.relative(), selected.version));
        let header = match read_selected(
            &root,
            &selected,
            lookup.encoding,
            args.max_header_bytes,
            Some(&lookup.id),
        )? {
            Header::Found(header) => header,
            Header::Absent => return Ok(Observed::Absent),
            Header::Isolated(stop) => return Err(stop),
        };
        Ok(match size(&selected.path(&root))? {
            Some(size) => Observed::Found { header, size },
            None => Observed::Absent,
        })
    });
    let (path, stored_version) = selected_at.unzip();
    let mut record = json!({
        "schema": "bake/session-stat",
        "version": 1,
        "status": null,
        "encoding": lookup.encoding.label(),
        "path": path,
        "storedVersion": stored_version,
        "header": null,
        "sizeBytes": null,
    });
    let fields = record.as_object_mut().expect("record object");
    let status = match outcome {
        Ok(Observed::Found { header, size }) => {
            fields.insert("header".into(), header_json(&header));
            fields.insert("sizeBytes".into(), json!(size));
            ("found", 0)
        }
        Ok(Observed::Absent) => ("absent", 0),
        Err(Stop::Failure(message)) => return Err(message),
        Err(Stop::Refused(refused)) => {
            fields.insert("refusal".into(), refused.to_json());
            ("refused", 3)
        }
    };
    fields.insert("status".into(), json!(status.0));
    Ok((record, status.1))
}

/// `readGenerationHeader` for the `selected` generation under the resolved
/// `root`: read its first record within `max_header_bytes`, decode and
/// migrate it, and check its stored identity, against the `expected` id when
/// one was requested. Refusals name the `header` or `identity` stage; a
/// budget or native limit always refuses, since its TypeScript outcome is
/// unknown.
pub(crate) fn read_selected(
    root: &Path,
    selected: &Selected,
    encoding: Encoding,
    max_header_bytes: u64,
    expected: Option<&str>,
) -> Step<Header> {
    let relative = selected.relative();
    let path = selected.path(root);
    let record = match read_header(&path, encoding, max_header_bytes, &relative)? {
        Record::Complete(record) => record,
        Record::Absent => return Ok(Header::Absent),
        Record::Corrupt(stop) => return Ok(Header::Isolated(stop)),
    };
    let header =
        match read_generation_header_record(&record, selected.version, PathPlatform::host()) {
            Ok(Some(header)) => header,
            Ok(None) => return Ok(Header::Absent),
            Err(refusal @ GenerationHeaderRefusal::Unsupported(_)) => {
                return Ok(Header::Isolated(header_refusal(
                    refusal,
                    selected.version,
                    &relative,
                )));
            }
            Err(refusal) => return Err(header_refusal(refusal, selected.version, &relative)),
        };
    check_identity(root, &path, selected, expected, &header, &relative)?;
    Ok(Header::Found(header))
}

/// The artifact's size from a metadata read that follows links, or `None`
/// when it no longer exists.
pub(crate) fn size(path: &Path) -> Step<Option<u64>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Stop::Failure(format!("cannot stat {path:?}: {error}"))),
    }
}

/// The physical read size; TypeScript's header readers also read 8 KiB.
const CHUNK: usize = 8192;

/// What the physical header read found.
enum Record {
    /// The first record, LF included.
    Complete(Vec<u8>),
    /// The file is missing or ends before the record is complete.
    Absent,
    /// The first Zstd frame failed to decode or holds other than one line.
    Corrupt(Stop),
}

/// The selected generation's first record.
fn read_header(path: &Path, encoding: Encoding, max_bytes: u64, relative: &str) -> Step<Record> {
    let failure = |action: &str, error: io::Error| {
        Stop::Failure(format!("cannot {action} {path:?}: {error}"))
    };
    let file = match open_read_only(path) {
        Ok(file) => file,
        // A dangling link or a file removed after the listing.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Record::Absent),
        Err(error) => return Err(failure("open", error)),
    };
    if !file
        .metadata()
        .map_err(|error| failure("read", error))?
        .is_file()
    {
        return Err(Stop::Failure(format!("{path:?} is not a regular file")));
    }
    let budget = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let over_budget = || {
        refused(
            "header",
            "header-budget",
            Kind::NativeLimit,
            format!("the header record is larger than --max-header-bytes {max_bytes}"),
            Some(relative.to_owned()),
        )
    };
    // A first frame cut off by the read so far is `None` here.
    let probe = |prefix: &[u8]| match zstd_header_record(prefix, budget) {
        Ok(record) => Ok(record.map(Record::Complete)),
        Err(RestoreRefusal::NativePlaintextBudget { .. }) => Err(over_budget()),
        Err(RestoreRefusal::Zstd(zstd)) => Ok(Some(Record::Corrupt(refused(
            "header",
            "corrupt-header-frame",
            Kind::Invalid,
            zstd.message(),
            Some(relative.to_owned()),
        )))),
        Err(_) => unreachable!("the header frame read refuses only Zstd or budget"),
    };
    let mut prefix = Vec::new();
    let mut chunk = [0u8; CHUNK];
    // Each Zstd probe rescans the first frame from its start, so probes run
    // at doubling prefix lengths, at the budget, and at the end of the file.
    let mut next_probe = CHUNK;
    loop {
        let room = budget - prefix.len();
        if room == 0 {
            // The prefix was probed at the budget; one byte tells the end of
            // the file from more input.
            let count =
                read_some(&file, &mut chunk[..1]).map_err(|error| failure("read", error))?;
            return if count == 0 {
                Ok(Record::Absent)
            } else {
                Err(over_budget())
            };
        }
        let count = read_some(&file, &mut chunk[..room.min(CHUNK)])
            .map_err(|error| failure("read", error))?;
        if count == 0 {
            return match encoding {
                Encoding::None => Ok(Record::Absent),
                Encoding::Zstd => Ok(probe(&prefix)?.unwrap_or(Record::Absent)),
            };
        }
        let start = prefix.len();
        prefix.extend_from_slice(&chunk[..count]);
        match encoding {
            Encoding::None => {
                if let Some(at) = chunk[..count].iter().position(|byte| *byte == b'\n') {
                    prefix.truncate(start + at + 1);
                    return Ok(Record::Complete(prefix));
                }
            }
            Encoding::Zstd if prefix.len() >= next_probe || prefix.len() == budget => {
                next_probe = prefix.len().saturating_mul(2);
                if let Some(record) = probe(&prefix)? {
                    return Ok(record);
                }
            }
            Encoding::Zstd => {}
        }
    }
}

fn read_some(mut file: &File, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match file.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

/// A header refusal at the `header` stage. Messages carry no path.
fn header_refusal(refusal: GenerationHeaderRefusal, version: u64, relative: &str) -> Stop {
    let (reason, kind, message) = match refusal {
        GenerationHeaderRefusal::Rejected(message) => (
            if message.starts_with("session generation filename identifies") {
                Some("generation-header-mismatch")
            } else if message.starts_with("session header uses retired policy") {
                Some("retired-header-fields")
            } else {
                None
            },
            Kind::Invalid,
            message,
        ),
        GenerationHeaderRefusal::Unsupported(message) => (
            (version > CURRENT_SESSION_FORMAT_VERSION).then_some("newer-format"),
            Kind::Unsupported,
            message,
        ),
        GenerationHeaderRefusal::NativeSubset(limit) => {
            (None, Kind::NativeLimit, header_limit(limit).into())
        }
    };
    Stop::Refused(Box::new(Refused {
        stage: "header",
        reason,
        refusal: Refusal::new(kind, message),
        path: Some(relative.to_owned()),
    }))
}

/// The current logical header, with `null` for each absent optional field.
pub(crate) fn header_json(header: &SessionHeader) -> Value {
    json!({
        "version": CURRENT_SESSION_FORMAT_VERSION,
        "id": header.id,
        "createdAt": header.created_at,
        "cwd": header.cwd,
        "parentSession": header.parent_session,
        "isSeeded": header.is_seeded,
        "origin": header.origin.map(|HeaderOrigin::Subagent| "subagent"),
        "delegationDepth": header.delegation_depth,
        "agentPreset": header.agent_preset,
    })
}
