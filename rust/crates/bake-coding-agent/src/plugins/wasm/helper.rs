//! The helper process: `bake-rs` re-executed with [`HELPER_ARG`]. It sets
//! up its memory limit, then compiles one plugin component and answers
//! calls to it over the [protocol](super::protocol). No native artifact
//! crosses the pipe: the helper receives component bytes and compiles them
//! itself.
//!
//! Threads: `main` owns the engine, the compile, and every guest call, on
//! a current-thread Tokio runtime with only the timer enabled. A reader
//! thread owns standard input; it parses frames, keeps one request in
//! flight, and is the parent-death watchdog: end of input between frames
//! means the parent is gone or closed the pipe, and it exits the process,
//! even while `main` is compiling. Both end with the process.
//!
//! Every exit goes through one gate, taken before a fatal report becomes
//! visible, so a report and the watchdog cannot race to different exit
//! codes.

use std::ffi::OsString;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::engine;
use super::protocol::{
    self, CallRequest, Hello, Kind, PROTOCOL_VERSION, Reply, encode_fatal, encode_loaded,
    encode_started, frame, read_frame_sync,
};
use super::{BUILD, HELPER_ARG, os_limit, validate};

/// The helper's exit codes besides 0, an orderly `Shutdown`.
pub(crate) mod exit {
    /// Its arguments are not the ones the parent sends.
    pub const USAGE: i32 = 64;
    /// Its memory limit could not be established or verified.
    pub const LIMIT: i32 = 70;
    /// The parent broke the protocol; a `Fatal` frame says how.
    pub const PROTOCOL: i32 = 71;
    /// Standard input ended between frames: the parent is gone or closed
    /// the pipe. The watchdog exits with it even mid-compile.
    pub const PARENT_GONE: i32 = 72;
    /// Writing a frame to standard output failed.
    pub const OUTPUT_CLOSED: i32 = 74;
    /// The engine or runtime could not be built.
    pub const INTERNAL: i32 = 73;
}

/// The reader thread's stack. It parses frames and allocates payloads on
/// the heap; a small stack keeps the address-space budget for Wasmtime.
const READER_STACK: usize = 256 * 1024;

/// What the parent passes on the command line after [`HELPER_ARG`].
#[derive(Debug)]
struct Args {
    generation: u64,
    memory_budget: u64,
    max_component: u32,
    call_fuel: u64,
    call_timeout: Duration,
}

fn parse(args: Vec<OsString>) -> Result<Args, String> {
    let mut values = [None::<u64>; 5];
    let names = [
        "--generation",
        "--memory-budget",
        "--max-component",
        "--call-fuel",
        "--call-timeout-ms",
    ];
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_str().ok_or("a non-UTF-8 argument")?.to_owned();
        let slot = names
            .iter()
            .position(|name| *name == arg)
            .ok_or_else(|| format!("unknown argument {arg}"))?;
        let value = args
            .next()
            .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
            .ok_or_else(|| format!("{arg} needs a number"))?;
        if values[slot].replace(value).is_some() {
            return Err(format!("{arg} is repeated"));
        }
    }
    let get = |slot: usize| values[slot].ok_or_else(|| format!("{} is missing", names[slot]));
    Ok(Args {
        generation: get(0)?,
        memory_budget: get(1)?,
        max_component: u32::try_from(get(2)?).map_err(|_| "--max-component is too large")?,
        call_fuel: get(3)?,
        call_timeout: Duration::from_millis(get(4)?),
    })
}

/// Taken, and never released, by whichever thread decides the exit code.
static EXIT: Mutex<()> = Mutex::new(());

fn exit_once(code: i32) -> ! {
    let _held = EXIT.lock().unwrap_or_else(PoisonError::into_inner);
    std::process::exit(code)
}

/// Standard output, shared by `main` and the reader's fatal reports.
type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, kind: Kind, payload: &[u8]) {
    let written = {
        let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
        out.write_all(&frame(kind, payload))
            .and_then(|()| out.flush())
    };
    // Nobody reads the replies. Stdout is released first: `fatal` takes
    // the exit gate before stdout, so the reverse order could deadlock.
    if written.is_err() {
        exit_once(exit::OUTPUT_CLOSED);
    }
}

/// Reports a protocol error and exits, holding the exit gate before the
/// report is visible and stdout so no other frame interleaves.
fn fatal(out: &Out, code: i32, message: &str) -> ! {
    let _held = EXIT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = out
        .write_all(&frame(Kind::Fatal, &encode_fatal(code as u8, message)))
        .and_then(|()| out.flush());
    std::process::exit(code)
}

enum Message {
    Load(Vec<u8>),
    Call(CallRequest),
}

/// Runs the helper over `args`, the arguments after [`HELPER_ARG`], and
/// never returns. `bake-rs` calls it before anything else runs, so the
/// limit is in place before the process starts a thread or reads input.
pub fn helper_main(args: impl IntoIterator<Item = OsString>) -> ! {
    let args = match parse(args.into_iter().collect()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{HELPER_ARG}: {message}");
            exit_once(exit::USAGE);
        }
    };
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: plain call before any other thread exists. One malloc arena:
    // each extra glibc arena reserves 64 MiB of the address-space budget.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 1);
    }
    let limit = match os_limit::establish(args.memory_budget) {
        Ok(limit) => limit,
        Err(message) => {
            eprintln!("{HELPER_ARG}: memory limit unavailable: {message}");
            exit_once(exit::LIMIT);
        }
    };
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let (engine, linker, runtime) = match (|| {
        let engine = engine::engine().map_err(|error| format!("{error:#}"))?;
        let linker = engine::linker(&engine).map_err(|error| format!("{error:#}"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((engine, linker, runtime))
    })() {
        Ok(parts) => parts,
        Err(message) => {
            eprintln!("{HELPER_ARG}: {message}");
            exit_once(exit::INTERNAL);
        }
    };

    // Lockstep: one request in flight. The reader refuses a second, so the
    // capacity-1 channel never blocks the watchdog.
    let in_flight = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = sync_channel::<Message>(1);
    let spawned = std::thread::Builder::new()
        .name("plugin-reader".to_owned())
        .stack_size(READER_STACK)
        .spawn({
            let out = Arc::clone(&out);
            let in_flight = Arc::clone(&in_flight);
            let max_component = args.max_component;
            move || reader(&out, &sender, &in_flight, max_component)
        });
    if spawned.is_err() {
        eprintln!("{HELPER_ARG}: cannot start the reader thread");
        exit_once(exit::INTERNAL);
    }

    send(
        &out,
        Kind::Hello,
        &Hello {
            protocol: PROTOCOL_VERSION,
            build: BUILD.to_owned(),
            generation: args.generation,
            limit,
        }
        .encode(),
    );

    let mut plugin: Option<engine::Plugin> = None;
    let mut loaded = false;
    let mut last_id = 0u64;
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Load(payload) => {
                if loaded {
                    fatal(&out, exit::PROTOCOL, "a second Load");
                }
                loaded = true;
                let result = load(&out, &engine, &linker, &payload, args.max_component);
                drop(payload);
                let reply = match result {
                    Ok((pre, micros)) => {
                        plugin = Some(engine::Plugin::new(pre, args.call_fuel, args.call_timeout));
                        Ok(micros)
                    }
                    Err(failure) => Err(failure),
                };
                in_flight.store(false, Ordering::SeqCst);
                send(&out, Kind::Loaded, &encode_loaded(&reply));
            }
            Message::Call(request) => {
                let Some(plugin) = plugin.as_mut() else {
                    fatal(&out, exit::PROTOCOL, "a Call without a loaded component");
                };
                if request.generation != args.generation {
                    fatal(&out, exit::PROTOCOL, "a Call for another generation");
                }
                if request.id <= last_id {
                    fatal(&out, exit::PROTOCOL, "a Call id that does not increase");
                }
                last_id = request.id;
                send(&out, Kind::Started, &encode_started(request.id));
                let (instance, outcome, logs, dropped_logs) =
                    runtime.block_on(plugin.call(&request));
                let reply = Reply {
                    id: request.id,
                    generation: args.generation,
                    instance,
                    outcome,
                    logs,
                    dropped_logs,
                };
                in_flight.store(false, Ordering::SeqCst);
                send(&out, Kind::Reply, &reply.encode());
            }
        }
    }
    // The reader ended without exiting the process; only a panic gets here.
    exit_once(exit::INTERNAL)
}

fn load(
    out: &Out,
    engine: &wasmtime::Engine,
    linker: &wasmtime::component::Linker<engine::HostState>,
    payload: &[u8],
    max_component: u32,
) -> Result<(engine::Prepared, u64), (protocol::LoadFailure, String)> {
    use protocol::LoadFailure;
    let (api, component) = protocol::decode_load(payload)
        .map_err(|error| (LoadFailure::Invalid, error.to_string()))?;
    if component.len() > max_component as usize {
        return Err((
            LoadFailure::Invalid,
            format!("the component is over {max_component} bytes"),
        ));
    }
    validate::check_component(component, api).map_err(|message| (LoadFailure::Invalid, message))?;
    send(out, Kind::Compiling, &[]);
    let started = Instant::now();
    let pre = engine::prepare(engine, linker, component)?;
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    Ok((pre, micros))
}

fn reader(out: &Out, sender: &SyncSender<Message>, in_flight: &AtomicBool, max_component: u32) {
    let mut stdin = std::io::stdin().lock();
    loop {
        let (kind, payload) =
            match read_frame_sync(&mut stdin, |kind| protocol::helper_cap(kind, max_component)) {
                Ok(Some(frame)) => frame,
                // The watchdog: the parent is gone, even mid-compile.
                Ok(None) => exit_once(exit::PARENT_GONE),
                Err(error) => fatal(out, exit::PROTOCOL, &error.to_string()),
            };
        let message = match kind {
            Kind::Load => Message::Load(payload),
            Kind::Call => match CallRequest::decode(&payload) {
                Ok(request) => Message::Call(request),
                Err(error) => fatal(out, exit::PROTOCOL, &error.to_string()),
            },
            // An orderly stop; the parent sends it only while idle.
            Kind::Shutdown => exit_once(0),
            _ => fatal(out, exit::PROTOCOL, "unexpected frame kind"),
        };
        if in_flight.swap(true, Ordering::SeqCst) {
            fatal(out, exit::PROTOCOL, "a request while another is in flight");
        }
        if sender.send(message).is_err() {
            exit_once(exit::INTERNAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_exact() {
        let full = [
            "--generation",
            "3",
            "--memory-budget",
            "100",
            "--max-component",
            "10",
            "--call-fuel",
            "5",
            "--call-timeout-ms",
            "250",
        ];
        let args = parse(os(&full)).unwrap();
        assert_eq!(
            (
                args.generation,
                args.memory_budget,
                args.max_component,
                args.call_fuel,
                args.call_timeout
            ),
            (3, 100, 10, 5, Duration::from_millis(250))
        );
        assert!(
            parse(os(&full[..8]))
                .unwrap_err()
                .contains("--call-timeout-ms is missing")
        );
        let mut repeated = full.to_vec();
        repeated.extend(["--generation", "4"]);
        assert!(parse(os(&repeated)).unwrap_err().contains("repeated"));
        assert!(
            parse(os(&["--generation", "x"]))
                .unwrap_err()
                .contains("needs a number")
        );
        assert!(
            parse(os(&["--other", "1"]))
                .unwrap_err()
                .contains("unknown argument")
        );
    }
}
