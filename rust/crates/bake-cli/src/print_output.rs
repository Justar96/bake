//! Bounded print output with an independently scheduled stop path.
//!
//! Session observers write synchronously. A dedicated writer owns the OS
//! writes; at most one 8 KiB chunk waits behind its current chunk. The signal
//! owner has its own executor so a blocked observer cannot prevent shutdown,
//! even when the agent runtime has only one worker. Both threads are joined.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;

use bake_coding_agent::cli::Interrupt;
use bake_coding_agent::print_mode::SharedWriter;
use tokio::sync::oneshot;

const CHUNK_BYTES: usize = 8 * 1024;

enum Operation {
    Bytes {
        stream: usize,
        bytes: Vec<u8>,
    },
    Flush {
        stream: usize,
        sent: mpsc::Sender<Result<(), Failure>>,
    },
    Finish,
}

#[derive(Clone)]
struct Failure {
    kind: io::ErrorKind,
    message: String,
}

impl Failure {
    fn into_error(self) -> io::Error {
        io::Error::new(self.kind, self.message)
    }
}

type Failures = Arc<Mutex<[Option<Failure>; 2]>>;

struct Producer {
    stream: usize,
    sender: mpsc::SyncSender<Operation>,
    failures: Failures,
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "print output stopped")
}

impl Write for Producer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(error) =
            self.failures.lock().unwrap_or_else(PoisonError::into_inner)[self.stream].clone()
        {
            return Err(error.into_error());
        }
        let mut count = bytes.len().min(CHUNK_BYTES);
        // Preserve UTF-8 boundaries for WriteConsoleW on Windows. Other
        // streams still accept arbitrary bytes, as std::io::Write does.
        if let Err(error) = std::str::from_utf8(&bytes[..count])
            && error.error_len().is_none()
            && error.valid_up_to() > 0
        {
            count = error.valid_up_to();
        }
        if count == 0 {
            return Ok(0);
        }
        self.sender
            .send(Operation::Bytes {
                stream: self.stream,
                bytes: bytes[..count].to_vec(),
            })
            .map_err(|_| stopped())?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let (sent, received) = mpsc::channel();
        self.sender
            .send(Operation::Flush {
                stream: self.stream,
                sent,
            })
            .map_err(|_| stopped())?;
        received
            .recv()
            .map_err(|_| stopped())?
            .map_err(Failure::into_error)
    }
}

/// Owns the writer until normal drain or cancellation has completed.
pub struct Output {
    sender: mpsc::SyncSender<Operation>,
    cancelled: Arc<AtomicBool>,
    wake: Arc<platform::Wake>,
    thread: Option<JoinHandle<()>>,
    done: Option<oneshot::Receiver<()>>,
    failures: Failures,
}

impl Output {
    pub fn start() -> io::Result<Self> {
        let (mut writer, wake) = platform::Writer::new()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let (completed, done) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        let failures = Arc::new(Mutex::new([None, None]));
        let errors = Arc::clone(&failures);
        let thread = std::thread::Builder::new()
            .name("bake-print-output".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let Ok(operation) = receiver.recv() else {
                        break;
                    };
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match operation {
                        Operation::Bytes { stream, bytes } => {
                            let failed = errors.lock().unwrap_or_else(PoisonError::into_inner)
                                [stream]
                                .is_some();
                            if !failed && let Err(error) = writer.write(stream, &bytes, &stop) {
                                errors.lock().unwrap_or_else(PoisonError::into_inner)[stream] =
                                    Some(Failure {
                                        kind: error.kind(),
                                        message: error.to_string(),
                                    });
                            }
                        }
                        Operation::Flush { stream, sent } => {
                            let error = errors.lock().unwrap_or_else(PoisonError::into_inner)
                                [stream]
                                .clone();
                            let _ = sent.send(error.map_or(Ok(()), Err));
                        }
                        Operation::Finish => break,
                    }
                }
                // Restore inherited descriptor flags before completion is observed.
                drop(writer);
                drop(receiver);
                let _ = completed.send(());
            })?;
        let wake = match platform::Wake::attach(wake, &thread) {
            Ok(wake) => wake,
            Err(error) => {
                let _ = sender.send(Operation::Finish);
                let _ = thread.join();
                return Err(error);
            }
        };
        Ok(Self {
            sender,
            cancelled,
            wake: Arc::new(wake),
            thread: Some(thread),
            done: Some(done),
            failures,
        })
    }

    pub fn stdout(&self) -> SharedWriter {
        self.producer(0)
    }

    pub fn stderr(&self) -> SharedWriter {
        self.producer(1)
    }

    fn producer(&self, stream: usize) -> SharedWriter {
        Arc::new(Mutex::new(Producer {
            stream,
            sender: self.sender.clone(),
            failures: Arc::clone(&self.failures),
        }))
    }

    fn cancellation(&self) -> Cancellation {
        Cancellation {
            sender: self.sender.clone(),
            cancelled: Arc::clone(&self.cancelled),
            wake: Arc::clone(&self.wake),
        }
    }

    pub async fn finish(mut self) -> io::Result<()> {
        let _ = self.sender.send(Operation::Finish);
        let mut completed = true;
        if let Some(done) = self.done.take() {
            completed = done.await.is_ok();
        }
        if let Some(thread) = self.thread.take() {
            completed &= thread.join().is_ok();
        }
        if !completed {
            return Err(io::Error::other("print output worker failed"));
        }
        let errors = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        match errors.iter().flatten().next() {
            Some(error) => Err(error.clone().into_error()),
            None => Ok(()),
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.cancellation().cancel();
            let _ = thread.join();
        }
    }
}

struct Cancellation {
    sender: mpsc::SyncSender<Operation>,
    cancelled: Arc<AtomicBool>,
    wake: Arc<platform::Wake>,
}

impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        // Wake a receiver between writes as well as a thread in OS I/O.
        let _ = self.sender.try_send(Operation::Finish);
        self.wake.cancel();
    }
}

/// Registers signals before startup and forwards their exit code only after
/// blocked output producers can resume. Its executor never runs agent work.
pub struct Signals {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    exit_code: Arc<AtomicI32>,
}

impl Signals {
    pub fn start(output: &Output, interrupt: fn() -> Interrupt) -> io::Result<(Self, Interrupt)> {
        let cancel = output.cancellation();
        let (stop, finished) = oneshot::channel();
        let (sent, received) = oneshot::channel();
        let (ready, registered) = mpsc::channel();
        let exit_code = Arc::new(AtomicI32::new(0));
        let signal_code = Arc::clone(&exit_code);
        let thread = std::thread::Builder::new()
            .name("bake-print-signals".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let interrupt = interrupt();
                    let _ = ready.send(Ok(()));
                    if let Some(code) = stop_request(interrupt, finished).await {
                        signal_code.store(code, Ordering::Release);
                        cancel.cancel();
                        let _ = sent.send(code);
                    }
                });
            })?;
        let owner = Self {
            stop: Some(stop),
            thread: Some(thread),
            exit_code,
        };
        registered.recv().map_err(|_| stopped())??;
        Ok((owner, Box::pin(async { received.await.unwrap_or(1) })))
    }

    pub fn finish(mut self) -> Option<i32> {
        self.join();
        match self.exit_code.load(Ordering::Acquire) {
            0 => None,
            code => Some(code),
        }
    }

    fn join(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn stop_request(interrupt: Interrupt, finished: oneshot::Receiver<()>) -> Option<i32> {
    tokio::select! {
        biased;
        code = interrupt => Some(code),
        _ = finished => None,
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_ready_stop_request_precedes_normal_completion() {
        let (sent, finished) = oneshot::channel();
        sent.send(()).expect("normal completion ready");
        let interrupt: Interrupt = Box::pin(async { 143 });
        assert_eq!(stop_request(interrupt, finished).await, Some(143));
    }
}
