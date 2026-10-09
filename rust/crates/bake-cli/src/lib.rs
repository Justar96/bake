//! The `bake-rs session` commands as library calls: [`inspect::inspect`],
//! [`stat::run`], and [`list::run`], each returning the record the binary
//! prints, or its diagnostic, and [`record`], the binary's rendering of a
//! record result as `JSON.stringify` writes it, so a Session id or message
//! holding a lone surrogate prints as a `\udxxx` escape. The binary in `main.rs` parses arguments and writes these
//! records; it is the supported interface, and this library exists so tests
//! can run the commands in-process.

pub mod inspect;
pub mod list;
pub mod lookup;
pub mod stat;

use serde_json::Value;

use inspect::Outcome;

/// A Session id quoted as `JSON.stringify` quotes it, lone surrogates
/// escaped, spelled for a message that a record's `json_text` writes.
pub(crate) fn quoted_id(id: &str) -> String {
    bake_session::js_string::quote(id)
}

/// A JSON record and its exit status, or a diagnostic for exit status 1.
pub fn record(result: Result<(Value, u8), String>) -> Outcome {
    match result {
        Ok((record, status)) => Outcome::Record {
            json: bake_session::json_text(&record),
            status,
        },
        Err(message) => Outcome::Failure(message),
    }
}
