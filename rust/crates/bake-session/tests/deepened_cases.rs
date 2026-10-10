//! Every Session log in the shared case tables under `conformance/session/`
//! and `conformance/runtime/`, and every runtime capture, deepened one JSON
//! value position at a time by the harness in `deepened/mod.rs` and read
//! through every public `bake-session` entry point that accepts it, on a
//! small stack, so a recursive drop, clone, comparison, format,
//! serialization, or walk of a payload anywhere in the crate fails. Any
//! result, accepted or refused, passes, and every result is formatted with
//! `Debug`, cloned and compared where its type allows, and dropped.
//!
//! A stack overflow aborts the process, so `deepened_cases_sweep` runs the
//! variants in child processes of this test binary, sharded, each printing
//! the variant and entry point it is about to run to its stderr; a crashed
//! child names them, and `deepened_sweep_names_a_crashed_variant` checks
//! that naming and the resumption of a crashed shard. The default sweep runs
//! the harness's deterministic sample of positions so it fits CI; the
//! `#[ignore]`d `deepened_cases_full_sweep` runs every position in both
//! shapes:
//!
//! ```text
//! cargo test --locked -p bake-session --test deepened_cases -- --ignored --exact deepened_cases_full_sweep
//! ``` `deepened_negative_controls` shows that a plain drop,
//! clone, comparison, `to_string`, `Debug`, or one-frame recursive walk of a
//! value as deep as a variant's overflows a variant's stack, so the sweep
//! would see one level of recursion over the payload. A table the harness
//! cannot read fails `every_table_is_known`.

mod deepened;

use bake_session::{
    LogCompression, PathPlatform, PlainAppendLog, PlainLogFile, PromptDecision,
    RelationshipExtensions, RestoredLog, V1CodecRecovery, V1CodecVersion, V1Item,
    check_released_relationships, check_transformed_artifact, consumed_work, content_generation,
    context_pressure, decode_row_envelope, decode_source_event_seqs, decode_v0_v1_items,
    decode_v0_v1_rows, decode_v3_row, dismantle, encode_event_line, encode_header_line,
    first_record, fork_seed, goal_projection, json_text, migrate_released_generation,
    migrate_released_history, migrate_released_zstd_generation, migrate_v0_to_v1,
    migrate_v1_to_v2_decoded, migrate_v1_to_v2_transformed, migrate_v1_to_v2_transformed_items,
    migrate_v2_rows, parse_json, read_generation_header_record, read_header_record,
    released_generation_header, released_zstd_plaintext, replay_requests, replay_restored_requests,
    restore_migrated, restore_plain_log, restore_zstd_log, restored_inbox, scan_log,
    session_log_path, session_title, stage_plain_log, stage_zstd_log, starts_request_series,
    subagent_catalog, subagent_identity, subagent_timing, system_prompt_commits, token_usage,
    tools_changed, turn_boundary, unfinished_work, zstd_header_record,
};
use deepened::{
    BUDGET, Form, Identity, Mode, STACK, Tally, inspect, inspect_twin, log_bytes, nested,
    parse_lines, this_test, zstd_bytes,
};
use serde_json::Value;

const CONTROL_ENV: &str = "BAKE_DEEPENED_CONTROL";

/// Runs each entry point, naming it on stderr first and tallying its outcome.
struct Runner {
    tally: Tally,
    scratch: std::path::PathBuf,
    deep: String,
    /// The inherited event count a writer's header is created with.
    inherited: Option<u64>,
}

impl Runner {
    fn enter(&self, entry: &'static str) {
        deepened::enter(entry);
    }

    fn call<T, E>(
        &mut self,
        entry: &'static str,
        run: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        deepened::call(&mut self.tally, entry, run)
    }

    fn deep_event(&self, seq: u64) -> Value {
        parse_json(&format!(
            r#"{{"type":"x/deepened","seq":{seq},"time":1,"ignorable":true,"data":{{"deep":{}}}}}"#,
            self.deep
        ))
        .expect("the deep event parses")
    }

    fn variant(&mut self, form: Form, lines: &[String], tail: &str, identity: Option<&Identity>) {
        let bytes = log_bytes(lines, tail);
        match form {
            Form::Plain(Some(3)) => self.current(&bytes, identity),
            Form::Plain(Some(version)) => self.released(&bytes, lines, version, identity),
            Form::Plain(None) => {
                self.current(&bytes, identity);
                for version in 0..3 {
                    self.released(&bytes, lines, version, identity);
                }
            }
            Form::Events => self.events(lines),
        }
    }

    /// A current-format log: its header, scan, restorations, request
    /// derivation, Zstd framing, append model, and file.
    fn current(&mut self, bytes: &[u8], identity: Option<&Identity>) {
        if let Some(record) = first_record(bytes) {
            let header = self.call("read_header_record", || {
                read_header_record(record, PathPlatform::Posix)
            });
            inspect_twin(header);
            let header = self.call("read_generation_header_record", || {
                read_generation_header_record(record, 3, PathPlatform::Posix)
            });
            inspect_twin(header);
        }
        let scanned = self.call("scan_log", || scan_log(bytes, PathPlatform::Posix, BUDGET));
        let mut next_seq = 0;
        if let Ok(scanned) = &scanned {
            for (at, row) in scanned.rows().iter().enumerate() {
                let seq = u64::try_from(at).expect("a row index");
                let envelope = self.call("decode_row_envelope", || {
                    decode_row_envelope(row, seq, BUDGET)
                });
                inspect_twin(envelope);
                let decoded = self.call("decode_v3_row", || decode_v3_row(row, seq, BUDGET));
                inspect_twin(decoded);
                let line = self.call("encode_event_line", || encode_event_line(row));
                inspect_twin(line);
                let seqs = self.call("decode_source_event_seqs", || {
                    decode_source_event_seqs(row.get("sourceEventSeqs"), seq, BUDGET)
                });
                inspect_twin(seqs);
            }
            self.enter("ScannedLog::events");
            scanned.events().for_each(inspect_twin);
            next_seq = scanned.rows().len();
        }
        self.enter("scan_log result");
        if let Err(refusal) = &scanned {
            std::hint::black_box(refusal.message());
        }
        inspect_twin(scanned);
        let staged = self.call("stage_plain_log", || {
            stage_plain_log(bytes, PathPlatform::Posix, BUDGET)
        });
        if let Ok(staged) = staged {
            let restored = self.call("StagedLog::restore", || staged.restore());
            inspect_twin(restored);
        }
        let restored = self.call("restore_plain_log", || {
            restore_plain_log(bytes, PathPlatform::Posix, BUDGET)
        });
        if let Ok(restored) = restored {
            self.restored(restored);
        }
        let requests = self.call("replay_requests", || {
            replay_requests(bytes, PathPlatform::Posix, BUDGET)
        });
        if let Err(refusal) = &requests {
            std::hint::black_box(refusal.message());
        }
        self.requests(requests);
        let framed = zstd_bytes(bytes);
        let record = self.call("zstd_header_record", || zstd_header_record(&framed, BUDGET));
        inspect_twin(record);
        let restored = self.call("restore_zstd_log", || {
            restore_zstd_log(&framed, PathPlatform::Posix, BUDGET, BUDGET)
        });
        inspect_twin(restored);
        let staged = self.call("stage_zstd_log", || {
            stage_zstd_log(&framed, PathPlatform::Posix, BUDGET, BUDGET)
        });
        if let Ok(staged) = staged {
            let restored = self.call("StagedLog::restore", || staged.restore());
            inspect_twin(restored);
        }
        let deep_event = self.deep_event(u64::try_from(next_seq).expect("a seq"));
        let opened = self.call("PlainAppendLog::open", || {
            PlainAppendLog::open(bytes, PathPlatform::Posix, BUDGET)
        });
        if let Ok(mut log) = opened {
            let appended = self.call("PlainAppendLog::append", || {
                log.append(std::slice::from_ref(&deep_event))
            });
            if let Err(refusal) = &appended {
                std::hint::black_box(refusal.message());
            }
            inspect_twin(appended);
            self.enter("PlainAppendLog::flush");
            log.flush();
            std::hint::black_box(log.id());
            inspect_twin(log.bytes().map(<[u8]>::len));
            self.enter("PlainAppendLog");
            inspect_twin(log);
        } else if let Err(refusal) = &opened {
            std::hint::black_box(refusal.message());
        }
        self.file(bytes, 3, identity, &deep_event);
        dismantle(deep_event);
    }

    /// A released v0, v1, or v2 log: its header, every codec and edge, the
    /// chain, the final check, migrated restoration, and a write `open`.
    fn released(
        &mut self,
        bytes: &[u8],
        lines: &[String],
        version: u64,
        identity: Option<&Identity>,
    ) {
        if let Some(record) = first_record(bytes) {
            let header = self.call("read_generation_header_record", || {
                read_generation_header_record(record, version, PathPlatform::Posix)
            });
            inspect_twin(header);
        }
        let header = self.call("released_generation_header", || {
            released_generation_header(bytes, version, PathPlatform::Posix)
        });
        inspect_twin(header);
        let migrated = self.call("migrate_released_generation", || {
            migrate_released_generation(bytes, version, BUDGET)
        });
        self.migrated(migrated);
        let plaintext = self.call("released_zstd_plaintext", || {
            released_zstd_plaintext(&zstd_bytes(bytes), BUDGET)
        });
        if let Ok(plaintext) = &plaintext {
            let migrated = self.call("migrate_released_zstd_generation", || {
                migrate_released_zstd_generation(plaintext, version, BUDGET)
            });
            self.migrated(migrated);
        }
        self.enter("released_zstd_plaintext result");
        inspect_twin(plaintext);
        if let Some(mut rows) = parse_lines(lines) {
            let header = rows.remove(0);
            self.released_rows(&header, &rows, version);
            dismantle(header);
            rows.into_iter().for_each(dismantle);
        }
        let deep_event = self.deep_event(0);
        self.file(bytes, version, identity, &deep_event);
        dismantle(deep_event);
    }

    fn released_rows(&mut self, header: &Value, rows: &[Value], version: u64) {
        if version == 2 {
            let migrated = self.call("migrate_v2_rows", || {
                migrate_v2_rows(header, rows, PathPlatform::Posix, BUDGET)
            });
            self.migrated(migrated);
            let checked = self.call("check_released_relationships", || {
                check_released_relationships(header, 0, rows, &RelationshipExtensions::default())
            });
            inspect_twin(checked);
            return;
        }
        let codec = if version == 0 {
            V1CodecVersion::V0
        } else {
            V1CodecVersion::V1
        };
        for recovery in [V1CodecRecovery::Strict, V1CodecRecovery::Recoverable] {
            let decoded = self.call("decode_v0_v1_rows", || {
                decode_v0_v1_rows(header, rows, codec, recovery, PathPlatform::Posix, BUDGET)
            });
            if let Ok(decoded) = &decoded {
                if version == 0 {
                    let migrated = self.call("migrate_v0_to_v1", || migrate_v0_to_v1(decoded));
                    inspect_twin(migrated);
                } else {
                    let migrated = self.call("migrate_v1_to_v2_transformed", || {
                        migrate_v1_to_v2_transformed(decoded)
                    });
                    inspect_twin(migrated);
                }
            }
            self.enter("decode_v0_v1_rows result");
            inspect_twin(decoded);
            let items = self.call("decode_v0_v1_items", || {
                decode_v0_v1_items(header, rows, codec, recovery, PathPlatform::Posix, BUDGET)
            });
            if let Ok(items) = &items {
                self.enter("ReleasedChunkRun::stream and expand");
                for item in &items.items {
                    if let V1Item::AssistantChunkRun(run) = item {
                        dismantle(run.stream());
                        run.expand().into_iter().for_each(dismantle);
                    }
                }
                if version == 1 {
                    let migrated = self.call("migrate_v1_to_v2_transformed_items", || {
                        migrate_v1_to_v2_transformed_items(items)
                    });
                    inspect_twin(migrated);
                    let migrated = self.call("migrate_v1_to_v2_decoded", || {
                        migrate_v1_to_v2_decoded(items)
                    });
                    inspect_twin(migrated);
                }
                let migrated = self.call("migrate_released_history", || {
                    migrate_released_history(items)
                });
                self.migrated(migrated);
            }
            self.enter("decode_v0_v1_items result");
            inspect_twin(items);
        }
    }

    fn migrated<E: std::fmt::Debug + Clone + PartialEq>(
        &mut self,
        migrated: Result<bake_session::MigratedV2, E>,
    ) {
        if let Ok(migrated) = &migrated {
            let checked = self.call("check_transformed_artifact", || {
                check_transformed_artifact(migrated, PathPlatform::Posix)
            });
            inspect_twin(checked);
            let restored = self.call("restore_migrated", || {
                restore_migrated(migrated, PathPlatform::Posix, BUDGET)
            });
            if let Ok(restored) = restored {
                self.restored(restored);
            }
        }
        self.enter("migrated result");
        inspect_twin(migrated);
    }

    fn requests<E: std::fmt::Debug + Clone + PartialEq>(
        &mut self,
        requests: Result<Vec<bake_session::Request>, E>,
    ) {
        self.enter("Request::to_json and the requests result");
        if let Ok(requests) = &requests {
            for request in requests {
                let json = request.to_json();
                std::hint::black_box(json_text(&json));
                dismantle(json);
            }
        }
        inspect_twin(requests);
    }

    /// Every projection and fold of a restored log.
    fn restored(&mut self, restored: RestoredLog) {
        self.enter("RestoredLog accessors");
        restored.messages().into_iter().for_each(dismantle);
        let header = restored.request_header();
        dismantle(restored.tool_history());
        inspect_twin(restored.closers());
        let requests = self.call("replay_restored_requests", || {
            replay_restored_requests(&restored)
        });
        if let Err(refusal) = &requests {
            std::hint::black_box(refusal.message());
        }
        self.requests(requests);
        for boundary in [None, Some(0), Some(1)] {
            let seed = self.call("fork_seed", || fork_seed(&restored, boundary));
            if let Err(refusal) = &seed {
                std::hint::black_box(refusal.message());
            }
            inspect_twin(seed);
        }
        let usage = self.call("token_usage", || token_usage(&restored));
        inspect_twin(usage);
        let pressure = self.call("context_pressure", || context_pressure(&restored));
        match &pressure {
            Ok(pressure) => inspect_twin(pressure.view()),
            Err(refusal) => {
                std::hint::black_box(refusal.message());
            }
        }
        inspect_twin(pressure);
        let goal = self.call("goal_projection", || goal_projection(&restored));
        inspect_twin(goal);
        let inbox = self.call("restored_inbox", || restored_inbox(&restored));
        if let Err(refusal) = &inbox {
            std::hint::black_box(refusal.message());
        }
        inspect_twin(inbox);
        let consumed = self.call("consumed_work", || consumed_work(&restored));
        inspect_twin(consumed);
        self.enter("unfinished_work");
        inspect_twin(unfinished_work(&restored));
        let boundary = self.call("turn_boundary", || turn_boundary(&restored));
        inspect_twin(boundary);
        let title = self.call("session_title", || session_title(&restored));
        if let Ok(title) = title {
            dismantle(title);
        }
        self.enter("subagent_identity");
        inspect_twin(subagent_identity(&restored));
        self.enter("subagent_timing");
        inspect_twin(subagent_timing(&restored));
        let catalog = self.call("subagent_catalog", || subagent_catalog(&restored));
        inspect_twin(catalog);
        self.enter("system_prompt_commits");
        for (in_history, starts_series) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let decision = PromptDecision {
                in_history,
                starts_series,
            };
            inspect_twin(system_prompt_commits(
                &restored,
                "deepened prompt",
                decision,
            ));
        }
        self.enter("content_generation");
        let generation = content_generation(&restored);
        let mut tools = Vec::new();
        if let Some(Value::Object(mut fields)) = header {
            if let Some(Value::Array(items)) = fields.shift_remove("tools") {
                tools = items;
            }
            fields.into_iter().for_each(|(_, value)| dismantle(value));
        } else if let Some(other) = header {
            dismantle(other);
        }
        self.enter("tools_changed");
        std::hint::black_box(tools_changed(&restored, &tools));
        std::hint::black_box(tools_changed(&restored, &[]));
        self.enter("starts_request_series");
        std::hint::black_box(starts_request_series(
            false, generation, &restored, false, &tools,
        ));
        tools.into_iter().for_each(dismantle);
        self.enter("RestoredLog");
        inspect_twin(restored);
    }

    /// The log written beneath a Session root as `version`'s writer names
    /// it, opened for writing, appended to, and flushed; then the same log
    /// framed as a Zstd writer frames it, its body frame torn by its last
    /// byte so the rows before it are recovered, opened, appended to, and
    /// flushed in a Zstd root.
    fn file(
        &mut self,
        bytes: &[u8],
        version: u64,
        identity: Option<&Identity>,
        deep_event: &Value,
    ) {
        let Some(identity) = identity else {
            return;
        };
        let root = self.scratch.join("root");
        let Some(path) = session_log_path(&root, identity.cwd.as_deref(), &identity.id) else {
            return;
        };
        let directory = path.parent().expect("a Session directory");
        let name = match version {
            0 => "session.jsonl",
            1 => "session.v1.jsonl",
            2 => "session.v2.jsonl",
            _ => "session.v3.jsonl",
        };
        if std::fs::create_dir_all(directory).is_err()
            || std::fs::write(directory.join(name), bytes).is_err()
        {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let opened = self.call("PlainLogFile::open", || {
            PlainLogFile::open(&root, &identity.id, BUDGET)
        });
        match opened {
            Ok(file) => self.file_writes(file, std::slice::from_ref(deep_event)),
            Err(refusal) => {
                std::hint::black_box(refusal.message());
                inspect(refusal);
            }
        }
        std::fs::remove_dir_all(&root).expect("remove the variant's Session root");
        let mut framed = zstd_bytes(bytes);
        if first_record(bytes).is_some_and(|record| record.len() < bytes.len()) {
            framed.pop();
        }
        if std::fs::create_dir_all(directory).is_err()
            || std::fs::write(directory.join(format!("{name}.zstd")), framed).is_err()
        {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let compression = LogCompression::Zstd {
            max_plaintext_bytes: BUDGET,
        };
        let opened = self.call("PlainLogFile::open_compressed", || {
            PlainLogFile::open_compressed(&root, &identity.id, BUDGET, compression)
        });
        match opened {
            Ok(file) => self.file_writes(file, std::slice::from_ref(deep_event)),
            Err(refusal) => {
                std::hint::black_box(refusal.message());
                inspect(refusal);
            }
        }
        std::fs::remove_dir_all(&root).expect("remove the variant's Session root");
    }

    /// Append `events` to a write model's file and flush it.
    fn file_writes(&mut self, mut file: PlainLogFile, events: &[Value]) {
        std::hint::black_box(file.path());
        let appended = self.call("PlainLogFile::append", || file.append(events));
        if let Err(refusal) = &appended {
            std::hint::black_box(refusal.message());
        }
        inspect(appended);
        let flushed = self.call("PlainLogFile::flush", || file.flush());
        if let Err(refusal) = &flushed {
            std::hint::black_box(refusal.message());
        }
        inspect(flushed);
        self.enter("PlainLogFile");
        inspect(file);
    }

    /// A writer's header and events: the row encoder, the append model,
    /// a created file, and the relationship check.
    fn events(&mut self, lines: &[String]) {
        let Some(mut rows) = parse_lines(lines) else {
            return;
        };
        let header = rows.remove(0);
        for inherited in [None, Some(0), Some(1)] {
            let line = self.call("encode_header_line", || {
                encode_header_line(&header, inherited)
            });
            inspect_twin(line);
        }
        for event in &rows {
            let line = self.call("encode_event_line", || encode_event_line(event));
            inspect_twin(line);
        }
        for event in &rows {
            let seqs = self.call("decode_source_event_seqs", || {
                decode_source_event_seqs(
                    event.get("sourceEventSeqs"),
                    event.get("seq").and_then(Value::as_u64).unwrap_or(0),
                    BUDGET,
                )
            });
            inspect_twin(seqs);
        }
        let inherited = self.inherited;
        let created = self.call("PlainAppendLog::create", || {
            PlainAppendLog::create(&header, inherited)
        });
        if let Ok(mut log) = created {
            let appended = self.call("PlainAppendLog::append", || log.append(&rows));
            if let Err(refusal) = &appended {
                std::hint::black_box(refusal.message());
            }
            inspect_twin(appended);
            self.enter("PlainAppendLog::flush");
            log.flush();
            self.enter("PlainAppendLog");
            inspect_twin(log);
        }
        for extensions in [
            RelationshipExtensions::default(),
            RelationshipExtensions {
                step_events: vec!["x/deepened".to_owned()],
                preserved_source_title_request_text: true,
                legacy_interrupted_turn_restart: true,
            },
        ] {
            for inherited in [0, 1] {
                let checked = self.call("check_released_relationships", || {
                    check_released_relationships(&header, inherited, &rows, &extensions)
                });
                inspect_twin(checked);
            }
        }
        let root = self.scratch.join("root");
        let created = self.call("PlainLogFile::create", || {
            PlainLogFile::create(&root, &header, inherited)
        });
        match created {
            Ok(file) => self.file_writes(file, &rows),
            Err(refusal) => {
                std::hint::black_box(refusal.message());
                inspect(refusal);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        dismantle(header);
        rows.into_iter().for_each(dismantle);
    }
}

/// One shard of the sweep, run by `deepened_cases_sweep` in a child process.
#[test]
#[ignore = "a child of deepened_cases_sweep"]
fn deepened_shard() {
    deepened::shard(|input| {
        let mut runner = Runner {
            tally: Tally::new(),
            scratch: input.scratch,
            deep: input.other_deep,
            inherited: input.inherited,
        };
        runner.variant(
            input.form,
            &input.lines,
            &input.tail,
            input.identity.as_ref(),
        );
        runner.tally
    });
}

/// The entry points every sweep must reach with an accepted result.
const ACCEPTED_ENTRIES: &[&str] = &[
    "scan_log",
    "restore_plain_log",
    "replay_requests",
    "replay_restored_requests",
    "restore_zstd_log",
    "stage_plain_log",
    "StagedLog::restore",
    "PlainAppendLog::open",
    "PlainAppendLog::append",
    "PlainLogFile::open",
    "PlainLogFile::open_compressed",
    "PlainLogFile::append",
    "PlainLogFile::flush",
    "PlainAppendLog::create",
    "PlainLogFile::create",
    "encode_header_line",
    "encode_event_line",
    "fork_seed",
    "token_usage",
    "context_pressure",
    "goal_projection",
    "restored_inbox",
    "consumed_work",
    "turn_boundary",
    "session_title",
    "subagent_catalog",
    "migrate_released_generation",
    "migrate_released_zstd_generation",
    "released_zstd_plaintext",
    "released_generation_header",
    "read_generation_header_record",
    "read_header_record",
    "decode_v0_v1_rows",
    "decode_v0_v1_items",
    "migrate_v0_to_v1",
    "migrate_v1_to_v2_transformed",
    "migrate_v1_to_v2_transformed_items",
    "migrate_v1_to_v2_decoded",
    "migrate_released_history",
    "migrate_v2_rows",
    "check_transformed_artifact",
    "restore_migrated",
    "check_released_relationships",
    "decode_row_envelope",
    "decode_v3_row",
    "zstd_header_record",
    "decode_source_event_seqs",
];

#[test]
fn deepened_cases_sweep() {
    deepened::sweep("deepened_shard", Mode::Sample, ACCEPTED_ENTRIES);
}

/// Every position in both shapes; see the module comment.
#[test]
#[ignore = "the full sweep; the default sweep runs a sample"]
fn deepened_cases_full_sweep() {
    deepened::sweep("deepened_shard", Mode::Full, ACCEPTED_ENTRIES);
}

#[test]
fn deepened_sweep_names_a_crashed_variant() {
    deepened::check_crash_reporting("deepened_shard");
}

#[test]
fn every_table_is_known() {
    deepened::check_tables_known();
}

/// What a caller could do with a deep value that recurses over it.
fn control(name: &str, value: Value) {
    fn walk(value: &Value) -> usize {
        match value {
            Value::Array(items) => items.iter().map(walk).sum::<usize>() + 1,
            Value::Object(fields) => fields.values().map(walk).sum::<usize>() + 1,
            _ => 1,
        }
    }
    match name {
        "drop" => drop(value),
        "clone" => {
            let copy = value.clone();
            std::mem::forget(copy);
            std::mem::forget(value);
        }
        "compare" => {
            let other = parse_json(&json_text(&value)).expect("json");
            std::hint::black_box(value == other);
            dismantle(other);
            dismantle(value);
        }
        "to_string" => {
            std::hint::black_box(value.to_string());
            dismantle(value);
        }
        "debug" => {
            inspect(&value);
            dismantle(value);
        }
        "walk" => {
            std::hint::black_box(walk(&value));
            dismantle(value);
        }
        "dismantle" => dismantle(value),
        other => panic!("unknown control {other}"),
    }
}

/// One negative control, run by `deepened_negative_controls` in a child.
#[test]
#[ignore = "a child of deepened_negative_controls"]
fn deepened_control() {
    let Ok(spec) = std::env::var(CONTROL_ENV) else {
        return;
    };
    let (name, shape) = spec.split_once(':').expect("NAME:SHAPE");
    let value = parse_json(&nested(shape == "objects")).expect("deep JSON");
    let name = name.to_owned();
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || control(&name, value))
        .expect("spawn")
        .join()
        .expect("the control returns");
}

#[test]
fn deepened_negative_controls() {
    for shape in ["arrays", "objects"] {
        for name in [
            "drop",
            "clone",
            "compare",
            "to_string",
            "debug",
            "walk",
            "dismantle",
        ] {
            let output = this_test("deepened_control")
                .env(CONTROL_ENV, format!("{name}:{shape}"))
                .output()
                .expect("run a control");
            let stderr = String::from_utf8_lossy(&output.stderr);
            if name == "dismantle" {
                assert!(
                    output.status.success(),
                    "{name} {shape} fits the stack: {stderr}"
                );
            } else {
                assert!(
                    !output.status.success(),
                    "{name} {shape} must overflow a {STACK}-byte stack"
                );
                assert!(
                    stderr.contains("overflowed its stack"),
                    "{name} {shape}: {stderr}"
                );
            }
        }
    }
}
