//! Runs one synthetic conformance input from stdin in the current directory.
//!
//! Exit 0 prints one observation line, exit 2 rejects invalid input before
//! any write, and exit 1 reports an I/O failure; diagnostics go to stderr.

use std::io;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    match bake_conformance::run(io::stdin().lock(), io::stdout().lock(), Path::new(".")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("bake-conformance-runner: {failure}");
            ExitCode::from(failure.exit_code())
        }
    }
}
