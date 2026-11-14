//! `bake-eval`: see the crate documentation.

use std::process::ExitCode;

fn main() -> ExitCode {
    match bake_eval::cli::main(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bake-eval: {message}");
            ExitCode::FAILURE
        }
    }
}
