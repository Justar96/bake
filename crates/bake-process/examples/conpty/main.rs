//! `conpty`: a standalone Windows pseudoconsole host for terminal exercises.
//! Build with `cargo build -p bake-process --example conpty`.
//! A test helper, never part of a release archive.
//!
//! `conpty host --cols N --rows N [--cwd DIR] [--history] -- PROGRAM ARGS...`
//! creates a pseudoconsole and a kill-on-close [`Job`] and starts this same
//! binary as `conpty client` inside both, attached to the pseudoconsole as
//! its first client, the way the POSIX driver starts a shell on the PTY.
//! The client reads the console modes before it starts `PROGRAM` on the
//! console, then while it runs on request, and again once it has exited,
//! so the console's mode state is read by a client of the pseudoconsole
//! itself, never by the host's own (redirected) console. Only the host's
//! standard streams face the driver: commands in, events and the
//! pseudoconsole's output out ([`protocol`]).
//!
//! What the mode reads prove: `GetConsoleMode` of the input buffer
//! (`CONIN$`) and of the screen buffer (`CONOUT$`) the client opened before
//! the program started. These are the console's state, shared by every
//! client, so a read after the program exits shows what it left for the
//! next program on that console, as `tcgetattr` on the PTY does on POSIX.
//! They say nothing about the VT modes (bracketed paste, mouse, alternate
//! screen) conhost keeps for its own renderer; those are seen, if at all,
//! only in the output it forwards.
//!
//! [`Job`]: bake_process::windows_job::Job

mod protocol;

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod sys;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    run(&args)
}

#[cfg(windows)]
fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let usage = || {
        eprintln!(
            "usage: conpty host --cols N --rows N [--cwd DIR] [--history] -- PROGRAM ARGS..."
        );
        ExitCode::from(2)
    };
    let Some((role, rest)) = args.split_first() else {
        return usage();
    };
    match role.to_str() {
        Some("host") => match protocol::host_args(rest) {
            Ok(parsed) => host::run(&parsed),
            Err(reason) => {
                eprintln!("conpty host: {reason}");
                usage()
            }
        },
        Some("client") => match protocol::client_args(rest) {
            Ok(parsed) => client::run(&parsed),
            Err(reason) => {
                eprintln!("conpty client: {reason}");
                ExitCode::from(2)
            }
        },
        _ => usage(),
    }
}

/// Elsewhere there is no pseudoconsole: the driver uses a POSIX PTY.
#[cfg(not(windows))]
fn run(_args: &[std::ffi::OsString]) -> ExitCode {
    eprintln!(
        "conpty: the pseudoconsole host runs only on Windows; the PTY driver uses a POSIX PTY here"
    );
    ExitCode::from(2)
}
