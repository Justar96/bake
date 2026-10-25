//! `conpty host`: owns the pseudoconsole, the job its client and every
//! descendant are born in, and the threads that service each channel.
//!
//! Each pipe is serviced on its own named thread, as the pseudoconsole
//! documentation asks: the output is read continuously, through a close
//! included, until conhost breaks the pipe; input is written by its own
//! thread so a full pipe never stalls the others; the driver's commands
//! and the client's events are read by one thread each. The main thread
//! alone writes events, pumps the job's account, and observes exits on the
//! handles it holds: the client's from its creation, and the program's,
//! opened while the client still holds it (so its pid is pinned) and
//! checked to be a member of the job.
//!
//! The end is never read from a close or from a count. A finish closes the
//! pseudoconsole while the output thread drains, waits for the client and
//! program handles, then settles the job on its account
//! ([`Job::settle`]), terminating it only if members remain; every thread
//! is joined within a bound, its blocked read cancelled past that bound.
//! A failure after the start still runs the finish.
//!
//! A start that fails part way ends what it started before it returns
//! (`Starting::abort`), without relying on the job's kill-on-close or on
//! process exit: it terminates the job and waits for the client's own
//! handle, closes the pseudoconsole only once no client is attached (a
//! close before Windows 11 24H2 waits for clients) and only while the
//! output thread, started before anything else, drains it, cancels and
//! joins every started thread, and settles the job on its account. The
//! native fault controls at the end of this file fail the start before
//! each step and check that cleanup.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bake_process::windows_job::{Job, Verdict};
use serde_json::{Value, json};

use crate::protocol::{self, Command, HostArgs};
use crate::sys::{self, PseudoConsole};

/// How long a finish waits for each observable condition before it acts:
/// the client and program to exit after the close, the job to settle by
/// itself, and each thread to stop.
const BOUND: Duration = Duration::from_secs(10);
/// How long the driver has to close the command stream after `closed`.
const STDIN_GRACE: Duration = Duration::from_secs(5);
/// Messages applied per step before the account is pumped again.
const PER_STEP: usize = 256;
const MIN_PAUSE: Duration = Duration::from_millis(1);
const MAX_PAUSE: Duration = Duration::from_millis(50);

enum Msg {
    Output(Vec<u8>),
    OutputEnd(Option<String>),
    Command(Result<Command, String>),
    StdinEnd,
    Client(String),
    ClientEnd,
    ClientConnected(File),
    ClientConnectFailed(String),
    InputFailed(String),
}

/// A job member the driver asked about, held open so its pid stays its own.
struct Pin {
    pid: u32,
    image: String,
    handle: OwnedHandle,
}

/// The program the client started, held from its `spawned` report.
struct Program {
    pid: u32,
    handle: OwnedHandle,
    exit: Option<u32>,
}

#[derive(Default)]
struct Threads {
    output: Option<JoinHandle<()>>,
    input: Option<JoinHandle<()>>,
    commands: Option<JoinHandle<()>>,
    client: Option<JoinHandle<()>>,
    connect: Option<JoinHandle<()>>,
}

pub(crate) struct Host {
    rx: Receiver<Msg>,
    stdout: io::Stdout,
    job: Job,
    console: PseudoConsole,
    client: OwnedHandle,
    client_pid: u32,
    client_exit: Option<u32>,
    program: Option<Program>,
    final_modes: bool,
    released: bool,
    /// The client's command pipe, once it connected.
    to_client: Option<File>,
    /// Commands for the client sent before it connected.
    queued: Vec<Command>,
    input: Option<Sender<Vec<u8>>>,
    pins: Vec<Pin>,
    suspended: Vec<OwnedHandle>,
    finish: Option<bool>,
    output_ended: bool,
    driver_gone: bool,
    failed: bool,
    threads: Threads,
}

pub fn run(args: &HostArgs) -> ExitCode {
    let options = match StartOptions::production() {
        Ok(options) => options,
        Err(error) => return start_failed(&format!("host: {error}")),
    };
    let mut host = match start(args, options) {
        Ok(host) => host,
        Err(failed) => return start_failed(&failed.to_string()),
    };
    let client = host.client_pid;
    host.emit(&protocol::event("started", json!({ "client": client })));
    host.serve();
    if host.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn start_failed(message: &str) -> ExitCode {
    eprintln!("conpty {message}");
    let _ = io::stdout().write_all(protocol::error(message).as_bytes());
    ExitCode::FAILURE
}

fn named<F: FnOnce() + Send + 'static>(name: &str, body: F) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(body)
}

/// Each fallible step of [`start`] after the pseudoconsole exists, in
/// order. A fault injected before a step fails the start there, so a test
/// can prove the cleanup of everything started before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    OutputThread,
    InputThread,
    CommandsThread,
    Job,
    ControlPipes,
    Client,
    ConnectThread,
    EventsThread,
    Ready,
}

impl Step {
    #[cfg(test)]
    pub(crate) const ALL: [Step; 9] = [
        Step::OutputThread,
        Step::InputThread,
        Step::CommandsThread,
        Step::Job,
        Step::ControlPipes,
        Step::Client,
        Step::ConnectThread,
        Step::EventsThread,
        Step::Ready,
    ];
}

/// What [`start`] takes besides the arguments: the client's command line
/// before its control arguments, where the driver's commands come from,
/// and, for the fault controls only, a step to fail before.
pub(crate) struct StartOptions {
    pub(crate) client: Vec<OsString>,
    pub(crate) commands: Box<dyn Read + Send>,
    pub(crate) fault: Option<Step>,
}

impl StartOptions {
    /// This binary as the client; commands on standard input; no fault.
    fn production() -> io::Result<Self> {
        Ok(Self {
            client: vec![std::env::current_exe()?.into_os_string(), "client".into()],
            commands: Box::new(io::stdin()),
            fault: None,
        })
    }
}

/// How a failed start cleaned up: the threads it joined, by name, whether
/// the client it created ended and its job settled, and what could not be
/// cleaned up.
#[derive(Debug)]
pub(crate) struct Aborted {
    pub(crate) joined: Vec<&'static str>,
    pub(crate) client_ended: Option<bool>,
    pub(crate) settled: Option<bool>,
    pub(crate) problems: Vec<String>,
}

/// A start that failed, and its cleanup.
#[derive(Debug)]
pub(crate) struct StartFailed {
    pub(crate) error: io::Error,
    pub(crate) aborted: Aborted,
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let aborted = &self.aborted;
        write!(
            f,
            "host: {}; cleanup joined [{}]",
            self.error,
            aborted.joined.join(", ")
        )?;
        if let Some(ended) = aborted.client_ended {
            write!(f, ", client ended: {ended}")?;
        }
        if let Some(settled) = aborted.settled {
            write!(f, ", job settled: {settled}")?;
        }
        if !aborted.problems.is_empty() {
            write!(f, "; cleanup problems: {}", aborted.problems.join("; "))?;
        }
        Ok(())
    }
}

/// What a start owns so far. Every field is filled as its step succeeds;
/// on any failure [`Self::abort`] ends and awaits all of it before the
/// start returns, so nothing started is left detached.
struct Starting {
    rx: Receiver<Msg>,
    tx: Option<Sender<Msg>>,
    console: PseudoConsole,
    job: Option<Job>,
    client: Option<sys::Created>,
    input: Option<Sender<Vec<u8>>>,
    threads: Threads,
}

impl Starting {
    /// Ends what was started, in the order that cannot block on an
    /// undrained pipe: terminate the job and wait for the client's own
    /// handle, so no client is left for `ClosePseudoConsole` to wait on
    /// before Windows 11 24H2; close the pseudoconsole while the output
    /// thread, which exists from the first thread step on, drains it;
    /// cancel and join every thread; settle the job on its account.
    fn abort(mut self) -> Aborted {
        let mut problems = Vec::new();
        let client_ended = self.client.as_ref().map(|client| {
            if let Some(job) = &self.job
                && let Err(error) = job.terminate()
            {
                problems.push(format!("TerminateJobObject: {error}"));
            }
            let started = Instant::now();
            loop {
                match sys::signalled(client.process.as_handle(), Duration::from_millis(10)) {
                    Ok(true) => break true,
                    Ok(false) if started.elapsed() < BOUND => {
                        if let Some(job) = &self.job {
                            let _ = job.pump();
                        }
                    }
                    Ok(false) => {
                        problems.push("the client did not end after its job was terminated".into());
                        break false;
                    }
                    Err(error) => {
                        problems.push(error.to_string());
                        break false;
                    }
                }
            }
        });
        self.console.close();
        self.input = None;
        self.tx = None;
        // Threads blocked in a read or connect that cannot complete now
        // (the driver's commands, the client's pipes) are cancelled at
        // once; the input thread ends with its channel and the output
        // thread with conhost's end of the pipe.
        let threads = [
            (
                "conpty-commands",
                self.threads.commands.take(),
                Duration::ZERO,
            ),
            (
                "conpty-client-connect",
                self.threads.connect.take(),
                Duration::ZERO,
            ),
            (
                "conpty-client-events",
                self.threads.client.take(),
                Duration::ZERO,
            ),
            ("conpty-input", self.threads.input.take(), BOUND),
            ("conpty-output", self.threads.output.take(), BOUND),
        ];
        let mut joined = Vec::new();
        let rx = &self.rx;
        for (name, thread, budget) in threads {
            let Some(thread) = thread else { continue };
            match sys::join_bounded(thread, budget, |wait| {
                let _ = rx.recv_timeout(wait);
            }) {
                Ok(()) => joined.push(name),
                Err(error) => problems.push(error),
            }
        }
        let settled = self
            .job
            .as_ref()
            .map(|job| match job.terminate_and_settle(BOUND) {
                Ok(()) => true,
                Err(verdict) => {
                    problems.push(format!("the job did not settle: {verdict:?}"));
                    false
                }
            });
        Aborted {
            joined,
            client_ended,
            settled,
            problems,
        }
    }
}

pub(crate) fn start(args: &HostArgs, options: StartOptions) -> Result<Host, StartFailed> {
    let unstarted = |error: io::Error| StartFailed {
        error,
        aborted: Aborted {
            joined: Vec::new(),
            client_ended: None,
            settled: None,
            problems: Vec::new(),
        },
    };
    let (input_read, input_write) = io::pipe().map_err(unstarted)?;
    let (output_read, output_write) = io::pipe().map_err(unstarted)?;
    // Nothing is attached to it yet, so dropping it on an error here does
    // not wait for a client, and its output pipe's read end is dropped
    // with it, so conhost's final write fails instead of blocking.
    let console = PseudoConsole::new(
        args.cols,
        args.rows,
        input_read.as_handle(),
        output_write.as_handle(),
    )
    .map_err(unstarted)?;
    let (tx, rx) = mpsc::channel();
    let mut starting = Starting {
        rx,
        tx: Some(tx),
        console,
        job: None,
        client: None,
        input: None,
        threads: Threads::default(),
    };
    let built = build(&mut starting, args, options, input_write, output_read);
    // The pseudoconsole holds its own copies; closing these lets each read
    // see a broken pipe once conhost is gone.
    drop((input_read, output_write));
    if let Err(error) = built {
        return Err(StartFailed {
            error,
            aborted: starting.abort(),
        });
    }
    if starting.job.is_none() || starting.client.is_none() {
        return Err(StartFailed {
            error: io::Error::other("the start ended without its job or client"),
            aborted: starting.abort(),
        });
    }
    let Starting {
        rx,
        console,
        job: Some(job),
        client: Some(created),
        input,
        threads,
        ..
    } = starting
    else {
        // Checked just above; a start that lost them is refused there.
        return Err(StartFailed {
            error: io::Error::other("the start ended without its job or client"),
            aborted: Aborted {
                joined: Vec::new(),
                client_ended: None,
                settled: None,
                problems: vec!["the start's owned state was lost".into()],
            },
        });
    };
    Ok(Host {
        rx,
        stdout: io::stdout(),
        job,
        console,
        client: created.process,
        client_pid: created.pid,
        client_exit: None,
        program: None,
        final_modes: false,
        released: false,
        to_client: None,
        queued: Vec::new(),
        input,
        pins: Vec::new(),
        suspended: Vec::new(),
        finish: None,
        output_ended: false,
        driver_gone: false,
        failed: false,
        threads,
    })
}

/// The steps after the pseudoconsole, each recorded in `starting` as soon
/// as it succeeds. The output thread comes first, so a reader drains the
/// pseudoconsole before anything can close it.
fn build(
    starting: &mut Starting,
    args: &HostArgs,
    options: StartOptions,
    input_write: io::PipeWriter,
    output_read: io::PipeReader,
) -> io::Result<()> {
    let StartOptions {
        client,
        commands,
        fault,
    } = options;
    let check = |step: Step| {
        if fault == Some(step) {
            Err(io::Error::other(format!("injected fault before {step:?}")))
        } else {
            Ok(())
        }
    };
    let tx = starting
        .tx
        .clone()
        .ok_or_else(|| io::Error::other("the start's channel is closed"))?;

    check(Step::OutputThread)?;
    starting.threads.output = Some({
        let tx = tx.clone();
        let mut pipe = output_read;
        named("conpty-output", move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if tx.send(Msg::Output(buffer[..read].to_vec())).is_err() {
                            return;
                        }
                    }
                    // conhost closing its end is the expected end.
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(error) => {
                        let _ = tx.send(Msg::OutputEnd(Some(format!(
                            "read the pseudoconsole output: {error}"
                        ))));
                        return;
                    }
                }
            }
            let _ = tx.send(Msg::OutputEnd(None));
        })?
    });

    check(Step::InputThread)?;
    let (input, queued) = mpsc::channel::<Vec<u8>>();
    starting.threads.input = Some({
        let tx = tx.clone();
        let mut pipe = input_write;
        named("conpty-input", move || {
            for data in queued {
                if let Err(error) = pipe.write_all(&data).and_then(|()| pipe.flush()) {
                    let _ = tx.send(Msg::InputFailed(format!(
                        "write the pseudoconsole input: {error}"
                    )));
                    return;
                }
            }
        })?
    });
    starting.input = Some(input);

    check(Step::CommandsThread)?;
    starting.threads.commands = Some({
        let tx = tx.clone();
        named("conpty-commands", move || {
            for line in BufReader::new(commands).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if tx
                    .send(Msg::Command(protocol::parse_command(&line)))
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(Msg::StdinEnd);
        })?
    });

    check(Step::Job)?;
    let job = starting.job.insert(
        Job::new().map_err(|error| io::Error::new(error.kind(), format!("Job::new: {error}")))?,
    );

    // Named, not inherited: a pseudoconsole client is started as Windows
    // Terminal starts one, inheriting no handle.
    check(Step::ControlPipes)?;
    let (commands_server, commands_name) = sys::named_pipe(false)?;
    let (events_server, events_name) = sys::named_pipe(true)?;

    check(Step::Client)?;
    let mut argv = client;
    argv.extend([
        "--control-in".into(),
        commands_name,
        "--control-out".into(),
        events_name,
    ]);
    if args.history {
        argv.push("--history".into());
    }
    argv.push("--".into());
    argv.extend(args.program.iter().cloned());
    let units: Vec<Vec<u16>> = argv.iter().map(|arg| arg.encode_wide().collect()).collect();
    if units.iter().any(|arg| arg.contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a NUL in an argument",
        ));
    }
    starting.client = Some(sys::create_client(
        bake_process::command_line::build_command_line(&units),
        args.cwd.as_deref(),
        &starting.console,
        job.as_handle(),
    )?);

    // Each connect waits for the client to open its end, or for a cancel
    // when the client ended first.
    check(Step::ConnectThread)?;
    starting.threads.connect = Some({
        let tx = tx.clone();
        named("conpty-client-connect", move || {
            let _ = tx.send(match sys::connect(commands_server.as_handle()) {
                Ok(()) => Msg::ClientConnected(File::from(commands_server)),
                Err(error) => {
                    Msg::ClientConnectFailed(format!("the client's command pipe: {error}"))
                }
            });
        })?
    });

    check(Step::EventsThread)?;
    starting.threads.client = Some(named("conpty-client-events", move || {
        if let Err(error) = sys::connect(events_server.as_handle()) {
            let _ = tx.send(Msg::ClientConnectFailed(format!(
                "the client's event pipe: {error}"
            )));
            return;
        }
        for line in BufReader::new(File::from(events_server)).lines() {
            let Ok(line) = line else { break };
            if tx.send(Msg::Client(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::ClientEnd);
    })?);

    check(Step::Ready)?;
    // The host keeps no sender: once every thread has ended, the channel
    // reports it.
    starting.tx = None;
    Ok(())
}

impl Host {
    fn emit(&mut self, line: &str) {
        if self.driver_gone {
            return;
        }
        if self
            .stdout
            .write_all(line.as_bytes())
            .and_then(|()| self.stdout.flush())
            .is_err()
        {
            // The driver is gone; finish without it.
            self.driver_gone = true;
            self.failed = true;
            self.finish.get_or_insert(true);
        }
    }

    fn report(&mut self, message: &str) {
        self.failed = true;
        let line = protocol::error(&format!("host: {message}"));
        eprint!("conpty {line}");
        self.emit(&line);
    }

    /// Applies what arrives within `wait`, then whatever else is queued, up
    /// to [`PER_STEP`] messages.
    fn step(&mut self, wait: Duration) {
        let first = match self.rx.recv_timeout(wait) {
            Ok(msg) => msg,
            Err(RecvTimeoutError::Timeout) => return,
            // Every thread has ended; nothing more can arrive.
            Err(RecvTimeoutError::Disconnected) => {
                std::thread::sleep(wait);
                return;
            }
        };
        self.handle(first);
        for _ in 1..PER_STEP {
            let Ok(msg) = self.rx.try_recv() else { break };
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Output(bytes) => self.emit(&protocol::output(&bytes)),
            Msg::OutputEnd(error) => {
                self.output_ended = true;
                if let Some(error) = error {
                    self.report(&error);
                }
                self.emit(&protocol::event("eof", json!({})));
            }
            Msg::Command(Ok(command)) => {
                if self.finish.is_none() {
                    self.command(command);
                }
            }
            Msg::Command(Err(reason)) => self.report(&format!("refused a command: {reason}")),
            // The driver stopped commanding without a close: finish as one.
            Msg::StdinEnd => {
                self.finish.get_or_insert(true);
            }
            Msg::Client(line) => self.client_line(&line),
            Msg::ClientEnd => {}
            Msg::ClientConnected(pipe) => {
                self.to_client = Some(pipe);
                for command in std::mem::take(&mut self.queued) {
                    self.send_client(&command);
                }
            }
            // A client that ended before it connected also ends here; its
            // own end is reported from its handle.
            Msg::ClientConnectFailed(error) => {
                if self.client_exit.is_none() && self.finish.is_none() {
                    self.report(&error);
                }
            }
            Msg::InputFailed(error) => self.report(&error),
        }
    }

    fn client_line(&mut self, line: &str) {
        let Some(object) = protocol::client_event(line) else {
            self.report(&format!("the client sent {}", protocol::bounded(line)));
            return;
        };
        match object.get("event").and_then(Value::as_str) {
            Some("spawned") => self.pin_program(object.get("pid").and_then(Value::as_u64)),
            Some("modes") if object.get("phase").and_then(Value::as_str) == Some("final") => {
                self.final_modes = true
            }
            Some("error") => self.failed = true,
            _ => {}
        }
        self.emit(&format!("{}\n", Value::Object(object)));
        if self.final_modes && self.program.is_some() && !self.released {
            self.released = true;
            self.send_client(&Command::Release);
        }
    }

    /// Opens the program while the client still holds its handle, so the
    /// pid cannot have been reused, and checks it is in the job.
    fn pin_program(&mut self, pid: Option<u64>) {
        let Some(pid) = pid.and_then(|pid| u32::try_from(pid).ok()) else {
            self.report("the client reported no program pid");
            return;
        };
        match sys::open_member(pid, self.job.as_handle()) {
            Ok(Some(handle)) => {
                self.program = Some(Program {
                    pid,
                    handle,
                    exit: None,
                })
            }
            Ok(None) => self.report(&format!("program {pid} is not a member of the job")),
            Err(error) => self.report(&format!("open program {pid}: {error}")),
        }
    }

    fn send_client(&mut self, command: &Command) {
        if self.to_client.is_none()
            && self
                .threads
                .connect
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        {
            self.queued.push(command.clone());
            return;
        }
        let sent = match &mut self.to_client {
            Some(pipe) => pipe
                .write_all(protocol::command_line(command).as_bytes())
                .and_then(|()| pipe.flush()),
            None => Err(io::Error::other("the client's command pipe is closed")),
        };
        if let Err(error) = sent {
            self.report(&format!("send {command:?} to the client: {error}"));
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Write(data) => match (&self.input, self.console.is_open()) {
                (Some(input), true) => {
                    if input.send(data).is_err() {
                        self.report("the input thread has stopped");
                    }
                }
                (None, _) => self.report("write: the input thread has stopped"),
                // Bytes for a closed pseudoconsole are dropped, as a closed
                // terminal's are: the emulator may still answer a query.
                (Some(_), false) => {}
            },
            Command::Resize { cols, rows } => {
                if let Err(error) = self.console.resize(cols, rows) {
                    self.report(&error.to_string());
                }
            }
            Command::Modes { .. } | Command::Ctrl(_) => self.send_client(&command),
            Command::Kill => match &self.program {
                Some(program) => {
                    if let Err(error) = sys::terminate(program.handle.as_handle(), 1) {
                        self.report(&error.to_string());
                    }
                }
                None => self.report("kill: no program is running"),
            },
            Command::Suspend { id } => self.suspend(id),
            Command::Resume { id } => {
                let suspended = std::mem::take(&mut self.suspended);
                let threads = suspended.len();
                sys::resume(suspended);
                self.emit(&protocol::event(
                    "resumed",
                    json!({ "id": id, "threads": threads }),
                ));
            }
            Command::Members { id } => self.members(id),
            Command::Unpin => self.pins.clear(),
            Command::Hangup => self.console.close(),
            Command::Close => {
                self.finish.get_or_insert(true);
            }
            Command::Release => self.report("release is a client command"),
        }
    }

    fn suspend(&mut self, id: u64) {
        let Some(program) = self
            .program
            .as_ref()
            .filter(|program| program.exit.is_none())
        else {
            self.report("suspend: no program is running");
            return;
        };
        if !self.suspended.is_empty() {
            self.report("suspend: the program is already suspended");
            return;
        }
        match sys::suspend(program.pid) {
            Ok(threads) => {
                let count = threads.len();
                self.suspended = threads;
                self.emit(&protocol::event(
                    "suspended",
                    json!({ "id": id, "threads": count }),
                ));
            }
            Err(error) => self.report(&error.to_string()),
        }
    }

    /// Pins every current member not yet pinned and lists every pin, with
    /// the client and program, each with whether its handle is unsignalled.
    fn members(&mut self, id: u64) {
        let pids = match sys::job_pids(self.job.as_handle()) {
            Ok(pids) => pids,
            Err(error) => return self.report(&error.to_string()),
        };
        let program = self.program.as_ref().map(|program| program.pid);
        for pid in pids {
            if self.pins.iter().any(|pin| pin.pid == pid) || Some(pid) == program {
                continue;
            }
            match sys::open_member(pid, self.job.as_handle()) {
                Ok(Some(handle)) => {
                    let image = sys::image(handle.as_handle());
                    self.pins.push(Pin { pid, image, handle });
                }
                // Gone since the listing, or reused outside the job.
                Ok(None) => {}
                Err(error) => return self.report(&error.to_string()),
            }
        }
        let alive = |handle: &OwnedHandle| {
            matches!(
                sys::signalled(handle.as_handle(), Duration::ZERO),
                Ok(false)
            )
        };
        let mut members = vec![
            json!({ "pid": self.client_pid, "role": "client", "image": sys::image(self.client.as_handle()), "alive": alive(&self.client) }),
        ];
        if let Some(program) = &self.program {
            members.push(json!({ "pid": program.pid, "role": "program", "image": sys::image(program.handle.as_handle()), "alive": alive(&program.handle) }));
        }
        for pin in &self.pins {
            members.push(json!({ "pid": pin.pid, "role": "member", "image": pin.image, "alive": alive(&pin.handle) }));
        }
        self.emit(&protocol::event(
            "members",
            json!({ "id": id, "members": members }),
        ));
    }

    /// Pumps the job's account and records exits the held handles show.
    fn observe(&mut self, pause: Duration) -> Duration {
        let next = match self.job.pump() {
            Ok(step) if !step.empty => Duration::ZERO,
            Ok(step) if step.taken > 0 => MIN_PAUSE,
            // Nothing is lost on an error: the settle drains and fails closed.
            _ => (pause * 2).clamp(MIN_PAUSE, MAX_PAUSE),
        };
        if self.client_exit.is_none()
            && matches!(
                sys::signalled(self.client.as_handle(), Duration::ZERO),
                Ok(true)
            )
        {
            match sys::exit_code(self.client.as_handle()) {
                Ok(code) => {
                    self.client_exit = Some(code);
                    self.emit(&protocol::event("client-exit", json!({ "code": code })));
                }
                Err(error) => self.report(&error.to_string()),
            }
        }
        let exited = self
            .program
            .as_ref()
            .filter(|program| program.exit.is_none())
            .and_then(|program| {
                matches!(
                    sys::signalled(program.handle.as_handle(), Duration::ZERO),
                    Ok(true)
                )
                .then(|| sys::exit_code(program.handle.as_handle()))
            });
        match exited {
            Some(Ok(code)) => {
                if let Some(program) = &mut self.program {
                    program.exit = Some(code);
                }
                self.emit(&protocol::event("exit", json!({ "code": code })));
            }
            Some(Err(error)) => self.report(&error.to_string()),
            None => {}
        }
        next
    }

    fn serve(&mut self) {
        let mut pause = MIN_PAUSE;
        while self.finish.is_none() {
            self.step(pause);
            pause = self.observe(pause);
            // The client ended: by itself after the program, or with the
            // pseudoconsole. Nothing is left to drive.
            if self.client_exit.is_some() {
                self.finish.get_or_insert(false);
            }
        }
        let force = self.finish.unwrap_or(true);
        self.finish_all(force);
    }

    /// Waits until `done` holds, stepping and observing meanwhile; false
    /// once `bound` has passed.
    fn until(&mut self, bound: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let started = Instant::now();
        let mut pause = MIN_PAUSE;
        while !done(self) {
            if started.elapsed() >= bound {
                return false;
            }
            self.step(pause.min(Duration::from_millis(10)));
            pause = self.observe(pause);
        }
        true
    }

    fn finish_all(&mut self, force: bool) {
        sys::resume(std::mem::take(&mut self.suspended));
        let mut terminated = false;
        if force {
            terminated = true;
            if let Err(error) = self.job.terminate() {
                self.report(&format!("TerminateJobObject: {error}"));
            }
        }
        // The output thread drains whatever the close writes.
        self.console.close();
        let ended = |host: &Self| {
            host.client_exit.is_some()
                && host
                    .program
                    .as_ref()
                    .is_none_or(|program| program.exit.is_some())
        };
        if !self.until(BOUND, ended) {
            self.report("the client or program did not exit after the pseudoconsole closed");
            terminated = true;
            let _ = self.job.terminate();
            let _ = self.until(BOUND, ended);
        }
        // Members that outlived the program end with the job; only the
        // account says when they have.
        let started = Instant::now();
        let verdict = loop {
            match self.job.settle() {
                Verdict::Pending(_) if started.elapsed() < BOUND => {
                    self.step(Duration::from_millis(5))
                }
                other => break other,
            }
        };
        let verdict = match verdict {
            Verdict::Pending(_) => {
                terminated = true;
                match self.job.terminate_and_settle(BOUND) {
                    Ok(()) => Verdict::Settled,
                    Err(verdict) => verdict,
                }
            }
            other => other,
        };
        match verdict {
            Verdict::Settled => self.emit(&protocol::event(
                "settled",
                json!({ "terminated": terminated }),
            )),
            other => self.report(&format!("the job did not settle: {other:?}")),
        }
        self.input = None;
        self.join("input", |threads| threads.input.take());
        self.join("output", |threads| threads.output.take());
        self.join("client-connect", |threads| threads.connect.take());
        self.to_client = None;
        self.join("client-events", |threads| threads.client.take());
        // Drain what the joined threads sent last.
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        if !self.output_ended {
            self.report("the output pipe did not end");
        }
        self.pins.clear();
        self.emit(&protocol::event("closed", json!({})));
        let grace = if self.driver_gone {
            Duration::ZERO
        } else {
            STDIN_GRACE
        };
        if let Some(thread) = self.threads.commands.take() {
            let rx = &self.rx;
            // Commands after `closed` are not applied.
            if let Err(error) = sys::join_bounded(thread, grace, |wait| {
                let _ = rx.recv_timeout(wait);
            }) {
                self.report(&error);
            }
        }
    }

    fn join(&mut self, name: &str, take: impl FnOnce(&mut Threads) -> Option<JoinHandle<()>>) {
        let Some(thread) = take(&mut self.threads) else {
            return;
        };
        let mut pending = Vec::new();
        let rx = &self.rx;
        let joined = sys::join_bounded(thread, BOUND, |wait| {
            if let Ok(msg) = rx.recv_timeout(wait) {
                pending.push(msg);
            }
        });
        for msg in pending {
            self.handle(msg);
        }
        if let Err(error) = joined {
            self.report(&format!("{name}: {error}"));
        }
    }
}

// Native fault controls for a partial start: a fault before each step must
// leave every thread started so far joined, a created client ended through
// its own handle, and its job settled on its account, all before `start`
// returns. The client is an interactive `cmd.exe /d /k`, which never opens
// the control pipes and stays attached to the pseudoconsole, so the
// connect threads are cancelled from a blocked connect and the close
// happens only after the client is gone. The commands source is a pipe
// whose writer the test holds open, so that thread is cancelled from a
// blocked read.
#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> OsString {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::path::Path::new(&root)
            .join(r"System32\cmd.exe")
            .into_os_string()
    }

    /// The threads started before `step`, in the order `abort` joins them.
    fn started_before(step: Step) -> Vec<&'static str> {
        let order = [
            (Step::OutputThread, "conpty-output"),
            (Step::InputThread, "conpty-input"),
            (Step::CommandsThread, "conpty-commands"),
            (Step::ConnectThread, "conpty-client-connect"),
            (Step::EventsThread, "conpty-client-events"),
        ];
        let position = |step: Step| Step::ALL.iter().position(|known| *known == step);
        let mut names: Vec<&'static str> = order
            .iter()
            .filter(|(thread, _)| position(*thread) < position(step))
            .map(|(_, name)| *name)
            .collect();
        let join_order = [
            "conpty-commands",
            "conpty-client-connect",
            "conpty-client-events",
            "conpty-input",
            "conpty-output",
        ];
        names.sort_by_key(|name| join_order.iter().position(|known| known == name));
        names
    }

    #[test]
    fn a_fault_at_each_start_step_cleans_up_everything_started_before_it() {
        let position = |step: Step| Step::ALL.iter().position(|known| *known == step);
        for step in Step::ALL {
            let (commands, held) = io::pipe().expect("commands pipe");
            let args = HostArgs {
                cols: 80,
                rows: 24,
                cwd: None,
                history: false,
                program: vec!["unused".into()],
            };
            let options = StartOptions {
                client: vec![cmd(), "/d".into(), "/k".into()],
                commands: Box::new(commands),
                fault: Some(step),
            };
            let began = Instant::now();
            let Err(failed) = start(&args, options) else {
                panic!("{step:?}: a start with an injected fault succeeded");
            };
            let elapsed = began.elapsed();
            drop(held);
            let aborted = &failed.aborted;
            assert!(
                failed
                    .error
                    .to_string()
                    .contains(&format!("injected fault before {step:?}")),
                "{step:?}: {failed}"
            );
            assert!(aborted.problems.is_empty(), "{step:?}: {aborted:?}");
            assert_eq!(aborted.joined, started_before(step), "{step:?}");
            let client_created = position(step) > position(Step::Client);
            assert_eq!(
                aborted.client_ended,
                client_created.then_some(true),
                "{step:?}"
            );
            let job_created = position(step) > position(Step::Job);
            assert_eq!(aborted.settled, job_created.then_some(true), "{step:?}");
            // Every wait is bounded; a cleanup that needed its bounds failed above.
            assert!(elapsed < 3 * BOUND, "{step:?}: cleanup took {elapsed:?}");
        }
    }
}
