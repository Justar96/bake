//! Research probe for Windows configuration trust. Not product code; see
//! `README.md`. Subcommands:
//!
//! - `dump <path>...`: read-only. For each path, opens the object itself
//!   (no reparse following) and prints its facts, owner, DACL, label, and
//!   the verdict of the proposed rules at each place.
//! - `walk <home> [<file>]`: read-only. Runs the proposed walk from the
//!   volume root to `<home>` (and `<home>\<file>`) and prints each step and
//!   the outcome.
//! - `mechanisms <base> <ledger>`: creates the ledger file and one private
//!   directory `<base>\bake-trust-probe-<pid>-<random>`, both exclusively,
//!   runs the mechanism checks inside that directory, and then deletes
//!   exactly the objects recorded there. It changes ACLs only on objects it
//!   created and never touches anything else.
//!
//! Exit status: see [`report::exit`].

mod owned;
mod report;
mod sd;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    std::process::exit(win::main());
    #[cfg(not(windows))]
    {
        eprintln!("this probe runs on Windows only; `cargo test` covers the pure rules");
        std::process::exit(report::exit::USAGE);
    }
}
