//! `conpty client`: the pseudoconsole's first client, the Windows stand-in
//! for the shell the POSIX driver starts on its PTY. It reads the console
//! modes before the program can change them, starts the program on
//! `CONIN$` and `CONOUT$`, answers mode reads and generates console control
//! events while it runs, reads the modes once more after it exits, and
//! exits once the host has pinned the program or closed the control pipe.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::windows::io::AsHandle;
use std::process::{Command as Process, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::protocol::{self, ClientArgs, Command};
use crate::sys;

/// How long the client waits for the host's release after the program has
/// exited. The host releases as soon as it has the program's handle.
const RELEASE: Duration = Duration::from_secs(10);

/// Writes one event line at a time to the host.
#[derive(Clone)]
struct Events(Arc<Mutex<File>>);

impl Events {
    fn send(&self, line: &str) {
        let mut file = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // A host that stopped reading is a host that ended; nothing to tell.
        let _ = file.write_all(line.as_bytes()).and_then(|()| file.flush());
    }
}

pub fn run(args: &ClientArgs) -> ExitCode {
    let pipe = |name: &std::ffi::OsStr, write: bool| {
        OpenOptions::new()
            .read(!write)
            .write(write)
            .open(name)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("open {}: {error}", name.to_string_lossy()),
                )
            })
    };
    // The event pipe first: a failure after it can still be reported.
    let events = match pipe(&args.control_out, true) {
        Ok(events) => Events(Arc::new(Mutex::new(events))),
        // No channel to report on; the host sees this client end unconnected.
        Err(_) => return ExitCode::FAILURE,
    };
    let commands = match pipe(&args.control_in, false) {
        Ok(commands) => commands,
        Err(error) => {
            events.send(&protocol::error(&format!("client: {error}")));
            return ExitCode::FAILURE;
        }
    };
    match serve(args, commands, &events) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            events.send(&protocol::error(&format!("client: {error}")));
            ExitCode::FAILURE
        }
    }
}

fn console(name: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .map_err(|error| io::Error::new(error.kind(), format!("open {name}: {error}")))
}

fn serve(args: &ClientArgs, commands: File, events: &Events) -> io::Result<()> {
    sys::ignore_interrupts()?;
    let (input, mut output) = (console("CONIN$")?, console("CONOUT$")?);
    let (input_mode, output_mode) = sys::console_modes(input.as_handle(), output.as_handle())?;
    events.send(&protocol::modes("initial", input_mode, output_mode, None));
    if args.history {
        output.write_all(protocol::shell_history().as_bytes())?;
        output.flush()?;
    }
    let (program, rest) = args
        .program
        .split_first()
        .ok_or_else(|| io::Error::other("no program"))?;
    let mut child = Process::new(program)
        .args(rest)
        .stdin(Stdio::from(input.try_clone()?))
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output.try_clone()?))
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("start {}: {error}", program.to_string_lossy()),
            )
        })?;
    events.send(&protocol::event(
        "spawned",
        serde_json::json!({ "pid": child.id() }),
    ));

    let (released, release) = mpsc::channel::<()>();
    let (reader_input, reader_output, reader_events) =
        (input.try_clone()?, output.try_clone()?, events.clone());
    let reader = std::thread::Builder::new()
        .name("conpty-client-commands".into())
        .spawn(move || {
            for line in BufReader::new(commands).lines() {
                let Ok(line) = line else { break };
                match protocol::parse_command(&line) {
                    Ok(Command::Modes { id }) => {
                        match sys::console_modes(
                            reader_input.as_handle(),
                            reader_output.as_handle(),
                        ) {
                            Ok((input, output)) => reader_events.send(&protocol::modes(
                                "running",
                                input,
                                output,
                                Some(id),
                            )),
                            Err(error) => {
                                reader_events.send(&protocol::error(&format!("client: {error}")))
                            }
                        }
                    }
                    Ok(Command::Ctrl(event)) => {
                        if let Err(error) = sys::generate(event) {
                            reader_events.send(&protocol::error(&format!("client: {error}")));
                        }
                    }
                    Ok(Command::Release) => {
                        let _ = released.send(());
                    }
                    Ok(other) => reader_events.send(&protocol::error(&format!(
                        "client: {other:?} is not a client command"
                    ))),
                    Err(reason) => {
                        reader_events.send(&protocol::error(&format!("client: {reason}")))
                    }
                }
            }
            // Dropping the sender tells the main thread the host is gone.
        })?;

    // Waits on the program's own handle: it ends by itself, by a command
    // the host relays, or with the job.
    let waited = child.wait();
    let after = sys::console_modes(input.as_handle(), output.as_handle());
    match after {
        Ok((input_mode, output_mode)) => {
            events.send(&protocol::modes("final", input_mode, output_mode, None))
        }
        Err(error) => events.send(&protocol::error(&format!("client: {error}"))),
    }
    // The program's handle, which pins its pid, stays open until the host
    // holds its own.
    match release.recv_timeout(RELEASE) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
        Err(RecvTimeoutError::Timeout) => events.send(&protocol::error(
            "client: the host did not release the program",
        )),
    }
    drop(child);
    // The reader is in a read of the host's command pipe; cancel it.
    sys::join_bounded(reader, Duration::ZERO, std::thread::sleep).map_err(io::Error::other)?;
    waited.map(|_| ())
}
