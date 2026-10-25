//! Research probe for Windows configuration trust. Not product code; see
//! `../../design.md`. Subcommands:
//!
//! - `dump <path>...`: read-only. For each path, opens the object itself
//!   (no reparse following) and prints its facts, owner, DACL, label, and
//!   the verdict of the proposed rules at each place.
//! - `walk <home> [<file>]`: read-only. Runs the proposed walk from the
//!   volume root to `<home>` (and `<home>\<file>`) and prints each step.
//! - `mechanisms <base>`: creates one private directory
//!   `<base>\bake-trust-probe-<random>` and runs the mechanism experiments
//!   in it, changing ACLs only on objects it created, then deletes exactly
//!   the objects it created. Never touches anything else.

mod sd;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    std::process::exit(win::main());
    #[cfg(not(windows))]
    {
        eprintln!("this probe runs on Windows only; `cargo test` covers the pure evaluator");
        std::process::exit(2);
    }
}
