//! `bake-rs session list`: report every materialized Session in an explicit
//! root, read-only, as TypeScript's `JsonlSessionPersistence.list()` does
//! through `listArtifacts` in
//! `packages/session/session-persistence-jsonl/src/index.ts`.
//!
//! The stages run in that backend's order, and an earlier stage's refusal
//! wins:
//!
//! 1. `root`: the root checks of [`crate::lookup`].
//! 2. `layout`: `ensureRootEncoding`, as for a lookup.
//! 3. `discovery`: the root and each real project directory are listed
//!    again, so a flat legacy file or a generation of the other compression
//!    is refused here too, and each real Session directory's highest
//!    canonical generation is selected by name. Directory links are not
//!    followed; a selected file link is.
//! 4. `header`, `identity`: each selected generation's header is read as
//!    `session stat` reads it, before the next Session directory is listed,
//!    and its id and `cwd` must name the selected file. No id is requested.
//!
//! Discovery skips only what TypeScript skips: a missing file, an incomplete
//! or malformed header, a corrupt first Zstd frame, and an unsupported
//! format, which TypeScript throws as `SessionFormatUnsupportedError` or
//! `SessionPersistenceCorruptionError`. Retired header fields, a header
//! version other than the file name's, and a stored identity other than the
//! selected file abort the listing, as does a header id that an earlier
//! admitted header already holds (`duplicate-id`, with no path). A budget or
//! another native limit also aborts, since skipping would silently omit an
//! artifact this preview did not examine. A refused listing reports no
//! sessions.
//!
//! Sizes are read after every header, following links, and an artifact
//! removed by then is left out, as in TypeScript. Sessions follow the
//! byte-order traversal of [`crate::lookup`]; TypeScript promises no order.
//! `--max-header-bytes` bounds each artifact's header read as for `session
//! stat`, and `--max-entries` bounds the entries read across both passes.
//! This process tracks no pending Session, reports no file revision, and
//! writes, locks, or migrates nothing; the record is an observation, not an
//! atomic snapshot of the root.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;

use serde_json::{Value, json};

use crate::inspect::{Encoding, Kind};
use crate::lookup::{Scan, Selected, Step, Stop, check_root, refused, resolve};
use crate::stat::{Header, header_json, read_selected, size};

/// The parsed operands of `session list`.
#[derive(Debug, PartialEq, Eq)]
pub struct ListArgs {
    /// The Session root, a UTF-8 path.
    pub root: OsString,
    pub encoding: Encoding,
    /// Bounds the directory entries read across both passes.
    pub max_entries: u64,
    /// Bounds each artifact's physical header read and decoded header record.
    pub max_header_bytes: u64,
}

/// Run `session list`, returning its record and exit status, or a
/// diagnostic for exit status 1.
pub fn run(args: &ListArgs) -> Result<(Value, u8), String> {
    let root = resolve(&args.root)?;
    let outcome = check_root(&args.root, &root).and_then(|()| discover(&root, args));
    let mut record = json!({
        "schema": "bake/session-list",
        "version": 1,
        "status": "listed",
        "encoding": args.encoding.label(),
        "sessions": [],
    });
    let fields = record.as_object_mut().expect("record object");
    let status = match outcome {
        Ok(sessions) => {
            fields.insert("sessions".into(), Value::Array(sessions));
            0
        }
        Err(Stop::Failure(message)) => return Err(message),
        Err(Stop::Refused(refused)) => {
            fields.insert("status".into(), json!("refused"));
            fields.insert("refusal".into(), refused.to_json());
            3
        }
    };
    Ok((record, status))
}

/// `listArtifacts`, then each admitted artifact's size.
fn discover(root: &Path, args: &ListArgs) -> Step<Vec<Value>> {
    let mut ids = HashSet::new();
    let mut admitted: Vec<(Selected, Value)> = Vec::new();
    Scan::new(root, args.encoding, args.max_entries).discover(|selected| {
        let header =
            match read_selected(root, &selected, args.encoding, args.max_header_bytes, None)? {
                Header::Found(header) => header,
                Header::Absent | Header::Isolated(_) => return Ok(()),
            };
        if !ids.insert(header.id.clone()) {
            return Err(refused(
                "discovery",
                "duplicate-id",
                Kind::Invalid,
                format!(
                    "Session {:?} appears in more than one Session directory",
                    header.id
                ),
                None,
            ));
        }
        admitted.push((selected, header_json(&header)));
        Ok(())
    })?;
    let mut sessions = Vec::with_capacity(admitted.len());
    for (selected, header) in admitted {
        let Some(size) = size(&selected.path(root))? else {
            continue;
        };
        sessions.push(json!({
            "path": selected.relative(),
            "storedVersion": selected.version,
            "header": header,
            "sizeBytes": size,
        }));
    }
    Ok(sessions)
}
