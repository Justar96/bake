//! The deepened case variants of `bake-session`'s `deepened_cases`, written
//! beneath a Session root this test owns and read in-process by the
//! `bake-rs session` commands: `inspect` of the file, `inspect --root --id`,
//! `stat`, and `list`, for the plain file and for the same log Zstd-framed,
//! on the harness's small stack. Any record, refusal, or diagnostic passes;
//! a recursion over a deep payload crashes the shard, which names the
//! variant and command. The default sweep runs the harness's sample of
//! positions; the `#[ignore]`d `deepened_cli_full_sweep` runs every
//! position in both shapes. A writer's header and events, which no file holds,
//! are not written.

#[path = "../../bake-session/tests/deepened/mod.rs"]
mod deepened;

use std::ffi::OsString;

use bake_cli::inspect::{self, Encoding, InspectArgs, LookupArgs, Outcome, Target};
use bake_cli::list::{self, ListArgs};
use bake_cli::record;
use bake_cli::stat::{self, StatArgs};
use bake_session::session_log_path;
use deepened::{Form, Mode, Tally, VariantInput, enter, log_bytes, zstd_bytes};

/// Bounds no deepened log meets.
const MAX_BYTES: u64 = 1 << 26;
const MAX_SOURCE_SEQS: u64 = 1 << 20;
const MAX_ENTRIES: u64 = 1 << 16;

/// Run one command, announced, and tally whether it printed a record with
/// exit status 0.
fn command(tally: &mut Tally, entry: &'static str, run: impl FnOnce() -> Outcome) {
    enter(entry);
    let outcome = run();
    let accepted = matches!(outcome, Outcome::Record { status: 0, .. });
    tally.entry(entry).or_default()[usize::from(!accepted)] += 1;
    eprintln!("R");
    drop(outcome);
}

/// The file name a writer of `version` gives a log; a log naming no
/// released generation is kept where a current writer would put it.
fn file_name(version: Option<u64>, encoding: Encoding) -> String {
    let plain = match version {
        Some(0) => "session.jsonl",
        Some(1) => "session.v1.jsonl",
        Some(2) => "session.v2.jsonl",
        _ => "session.v3.jsonl",
    };
    match encoding {
        Encoding::None => plain.to_owned(),
        Encoding::Zstd => format!("{plain}.zstd"),
    }
}

fn lookup(root: &OsString, id: &str, encoding: Encoding) -> LookupArgs {
    LookupArgs {
        root: root.clone(),
        id: id.to_owned(),
        encoding,
        max_entries: MAX_ENTRIES,
    }
}

fn run_commands(input: VariantInput) -> Tally {
    let mut tally = Tally::new();
    let (Form::Plain(version), Some(identity)) = (input.form, input.identity) else {
        return tally;
    };
    let plain = log_bytes(&input.lines, &input.tail);
    for (encoding, bytes) in [
        (Encoding::None, plain.clone()),
        (Encoding::Zstd, zstd_bytes(&plain)),
    ] {
        let root = input.scratch.join("root");
        let Some(path) = session_log_path(&root, identity.cwd.as_deref(), &identity.id) else {
            return tally;
        };
        let file = path
            .parent()
            .expect("a Session directory")
            .join(file_name(version, encoding));
        let written = std::fs::create_dir_all(file.parent().expect("a Session directory"))
            .and_then(|()| std::fs::write(&file, &bytes));
        if written.is_ok() {
            let root_text = root.clone().into_os_string();
            command(&mut tally, "session inspect FILE", || {
                inspect::inspect(&InspectArgs {
                    max_bytes: MAX_BYTES,
                    max_source_seqs: MAX_SOURCE_SEQS,
                    target: Target::File(file.clone().into_os_string()),
                })
            });
            command(&mut tally, "session inspect --root --id", || {
                inspect::inspect(&InspectArgs {
                    max_bytes: MAX_BYTES,
                    max_source_seqs: MAX_SOURCE_SEQS,
                    target: Target::Lookup(lookup(&root_text, &identity.id, encoding)),
                })
            });
            command(&mut tally, "session stat", || {
                record(stat::run(&StatArgs {
                    lookup: lookup(&root_text, &identity.id, encoding),
                    max_header_bytes: MAX_BYTES,
                }))
            });
            command(&mut tally, "session list", || {
                record(list::run(&ListArgs {
                    root: root_text.clone(),
                    encoding,
                    max_entries: MAX_ENTRIES,
                    max_header_bytes: MAX_BYTES,
                }))
            });
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    tally
}

/// One shard of the sweep, run by `deepened_cli_sweep` in a child process.
#[test]
#[ignore = "a child of deepened_cli_sweep"]
fn deepened_cli_shard() {
    deepened::shard(run_commands);
}

const COMMANDS: &[&str] = &[
    "session inspect FILE",
    "session inspect --root --id",
    "session stat",
    "session list",
];

#[test]
fn deepened_cli_sweep() {
    deepened::sweep("deepened_cli_shard", Mode::Sample, COMMANDS);
}

/// Every position in both shapes, as `bake-session`'s full sweep.
#[test]
#[ignore = "the full sweep; the default sweep runs a sample"]
fn deepened_cli_full_sweep() {
    deepened::sweep("deepened_cli_shard", Mode::Full, COMMANDS);
}
