//! The host side of one VM: its store state and the module's twelve imports,
//! six WASI preview 1 functions and six `env` callbacks.
//!
//! The imports follow `quickjs-wasi` 3.6.2's `dist/index.js` and
//! `dist/wasi-shim.js` (MIT, Copyright (c) 2026 Vercel, Inc.) with Pi
//! v1.3.0's `runtime/vm.ts` overrides (MIT; see `NOTICE`): engine output is
//! discarded, deterministic runs read a virtual clock and a seeded generator,
//! and scripts run in UTC. Every pointer and length the guest passes is
//! checked against its memory; a bad one ends the run as
//! [`Failure::Protocol`]. Nothing here panics on guest input.
//!
//! `env.host_call` is the prelude's bridge, decoded as Pi's `vm.ts` and
//! `execution.ts` decode its messages: calls, output items, and the script's
//! end. The end unwinds the guest with a trap, so nothing the script queued
//! after it runs, as Pi's host stops the VM on `done`.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::mpsc::UnboundedSender;
use wasmtime::{Caller, Engine, Linker, ResourceLimiter, StoreLimits, StoreLimitsBuilder};

use crate::guest::{
    GuestError, GuestExports, GuestResult, guest_slice, guest_slice_mut, write_bytes,
};
use crate::js::utf16_len;
use crate::prelude::{MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS};
use crate::types::{
    Completion, During, Event, Failure, HostCall, MAX_PENDING_CALLS, Outcome, OutputItem,
    ScriptError, StoreWrites,
};

/// WASI `errno` values.
const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_NOSYS: i32 = 52;
const CLOCK_REALTIME: i32 = 0;
const CLOCK_MONOTONIC: i32 = 1;
/// The bridge never passes more arguments.
const MAX_BRIDGE_ARGS: u32 = 8;

/// Why a run is being stopped from outside the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopReason {
    Aborted = 1,
    TimedOut = 2,
}

/// The message of an abort without one, Pi's default.
pub(crate) const ABORTED_MESSAGE: &str = "Execution aborted";

/// The stop flag and deadline one run's thread, its owner, its deadline
/// watchdog, and the engine's epoch callback share.
pub(crate) struct RunControl {
    stop: AtomicU8,
    abort_message: Mutex<Option<String>>,
    deadline: Option<Instant>,
    timeout: Option<Duration>,
    engine: Engine,
    /// Set when the script thread has its outcome; wakes the watchdog.
    finished: Mutex<bool>,
    wake: Condvar,
}

impl RunControl {
    pub(crate) fn new(engine: Engine, timeout: Option<Duration>) -> Self {
        Self {
            stop: AtomicU8::new(0),
            abort_message: Mutex::new(None),
            deadline: timeout.and_then(|timeout| Instant::now().checked_add(timeout)),
            timeout,
            engine,
            finished: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Records `reason` unless another came first, then bumps the engine's
    /// epoch so a running guest reaches its epoch check.
    pub(crate) fn stop(&self, reason: StopReason) {
        self.stop_with(reason, None);
    }

    /// [`RunControl::stop`], with the abort's message when this stop is the
    /// first.
    pub(crate) fn stop_with(&self, reason: StopReason, message: Option<String>) {
        let mut abort_message = self
            .abort_message
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let first = self
            .stop
            .compare_exchange(0, reason as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if first {
            *abort_message = message;
        }
        drop(abort_message);
        self.engine.increment_epoch();
    }

    /// Stops the run at its deadline whether or not anything else is
    /// running: the guest may be inside one long built-in that neither polls
    /// QuickJS's interrupt handler nor returns to the driver, and only an
    /// epoch increment reaches it. Returns once the deadline passed or the
    /// script thread called [`RunControl::finish`].
    pub(crate) fn watch_deadline(&self) {
        let Some(deadline) = self.deadline else {
            return;
        };
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *finished {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                drop(finished);
                self.stop(StopReason::TimedOut);
                return;
            }
            finished = self
                .wake
                .wait_timeout(finished, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Releases the deadline watchdog: the script thread has its outcome.
    pub(crate) fn finish(&self) {
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst) != 0
    }

    /// Whether the guest must stop: stopped, or past the deadline (which
    /// then counts as the stop).
    pub(crate) fn should_interrupt(&self) -> bool {
        if self.stopped() {
            return true;
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.stop(StopReason::TimedOut);
            return true;
        }
        false
    }

    /// The failure a stop reports.
    pub(crate) fn failure(&self) -> Option<Failure> {
        match self.stop.load(Ordering::SeqCst) {
            1 => Some(Failure::Aborted {
                message: self
                    .abort_message
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                    .unwrap_or_else(|| ABORTED_MESSAGE.to_owned()),
            }),
            2 => Some(Failure::Timeout {
                message: format!(
                    "Execution timed out after {} ms",
                    self.timeout.map_or(0, |timeout| timeout.as_millis())
                ),
            }),
            _ => None,
        }
    }
}

/// Calls waiting for an answer and the result numbers used, shared by the
/// script thread (which adds calls) and [`crate::Run`] (which answers them).
#[derive(Default)]
pub(crate) struct CallBook {
    pub(crate) pending: HashSet<u32>,
    pub(crate) delivered: HashSet<u32>,
    pub(crate) finished: bool,
}

pub(crate) fn lock(book: &Mutex<CallBook>) -> std::sync::MutexGuard<'_, CallBook> {
    book.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `StoreLimits`, recording a refused growth so the run ends as
/// [`Failure::OutOfMemory`].
pub(crate) struct MemoryLimiter {
    limits: StoreLimits,
    pub(crate) exceeded: Option<String>,
}

impl MemoryLimiter {
    fn new(linear_memory_bytes: usize) -> Self {
        Self {
            limits: StoreLimitsBuilder::new()
                .memory_size(linear_memory_bytes)
                .memories(1)
                .tables(1)
                .instances(1)
                .trap_on_grow_failure(true)
                .build(),
            exceeded: None,
        }
    }
}

impl ResourceLimiter for MemoryLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let result = self.limits.memory_growing(current, desired, maximum);
        if !matches!(result, Ok(true)) {
            self.exceeded.get_or_insert_with(|| {
                format!("the script's memory cannot grow from {current} to {desired} bytes")
            });
        }
        result
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.exceeded
            .get_or_insert_with(|| format!("the script's memory could not grow: {error:#}"));
        self.limits.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.limits.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

/// The clocks a VM reads.
pub(crate) enum Clock {
    /// The host's clocks; the monotonic one counts from the VM's start.
    Real { anchor: Instant },
    /// Deterministic mode: a seed-derived realtime reading while the context
    /// is created, because QuickJS seeds `Math.random()` from the first clock
    /// read, then `now` (milliseconds since the epoch), never going backwards.
    Virtual {
        creating: bool,
        started_at: f64,
        now: f64,
        seed: u32,
    },
}

/// Milliseconds to whole nanoseconds, rounding as `Math.round`.
fn millis_to_nanos(millis: f64) -> u64 {
    let rounded = (millis + 0.5).floor();
    if rounded.is_finite() && rounded > 0.0 {
        (rounded as u64).saturating_mul(1_000_000)
    } else {
        0
    }
}

impl Clock {
    /// The reading in nanoseconds, or `None` for an unsupported clock.
    fn read(&self, clock_id: i32) -> Option<u64> {
        match self {
            Self::Real { anchor } => match clock_id {
                CLOCK_REALTIME => Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |since| {
                            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                        }),
                ),
                CLOCK_MONOTONIC => {
                    Some(u64::try_from(anchor.elapsed().as_nanos()).unwrap_or(u64::MAX))
                }
                _ => None,
            },
            Self::Virtual {
                creating,
                started_at,
                now,
                seed,
            } => Some(if *creating {
                // QuickJS seeds Math.random() from this reading in microseconds and
                // turns 0 into 1, so seed s reads s + 1.
                if clock_id == CLOCK_REALTIME {
                    (u64::from(*seed) + 1) * 1000
                } else {
                    0
                }
            } else if clock_id == CLOCK_REALTIME {
                millis_to_nanos(*now)
            } else {
                millis_to_nanos(*now - *started_at)
            }),
        }
    }
}

/// `random_get`'s source.
pub(crate) enum Random {
    Os,
    /// mulberry32 over the seed, one 32-bit draw per four bytes, as Pi's `vm.ts`.
    Seeded(u32),
}

impl Random {
    fn fill(&mut self, bytes: &mut [u8]) -> GuestResult<()> {
        match self {
            Self::Os => getrandom::getrandom(bytes).map_err(|error| {
                GuestError::Protocol(format!("the system random source failed: {error}"))
            }),
            Self::Seeded(state) => {
                for chunk in bytes.chunks_mut(4) {
                    *state = state.wrapping_add(0x6d2b_79f5);
                    let mut value = (*state ^ (*state >> 15)).wrapping_mul(1 | *state);
                    value = (value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)))
                        ^ value;
                    let value = value ^ (value >> 14);
                    for (index, byte) in chunk.iter_mut().enumerate() {
                        *byte = (value >> (index * 8)) as u8;
                    }
                }
                Ok(())
            }
        }
    }
}

/// Guest addresses the bridge needs once the context exists.
#[derive(Clone, Copy)]
pub(crate) struct BridgeValues {
    /// A 4-byte allocation for string lengths.
    pub(crate) scratch: u32,
    /// `undefined`, the bridge's return value.
    pub(crate) undefined: u32,
}

/// One store's state.
pub(crate) struct HostState {
    pub(crate) exports: Option<GuestExports>,
    pub(crate) bridge: Option<BridgeValues>,
    pub(crate) limiter: MemoryLimiter,
    pub(crate) control: Arc<RunControl>,
    pub(crate) clock: Clock,
    random: Random,
    events: UnboundedSender<Event>,
    book: Arc<Mutex<CallBook>>,
    deterministic: bool,
    /// Calls the script has issued, tools and globals.
    pub(crate) issued: u32,
    /// Calls whose answers the VM has applied; `issued - settled` calls wait.
    pub(crate) settled: u32,
    output_chars: usize,
    output_items: usize,
    /// Set while the driver asks the prelude whether the script stalled.
    pub(crate) checking_stall: bool,
    /// Deterministic mode: the result being processed, 0 during the first run.
    pub(crate) current_seq: u32,
    /// The script's end, once the bridge reported it.
    pub(crate) done: Option<Outcome>,
    /// A failure an import recorded before trapping.
    pub(crate) failure: Option<Failure>,
}

/// What a store starts with.
pub(crate) struct HostSetup {
    pub(crate) control: Arc<RunControl>,
    pub(crate) events: UnboundedSender<Event>,
    pub(crate) book: Arc<Mutex<CallBook>>,
    pub(crate) linear_memory_bytes: usize,
    /// Deterministic mode's start and seed.
    pub(crate) deterministic: Option<(f64, u32)>,
}

impl HostState {
    pub(crate) fn new(setup: HostSetup) -> Self {
        let (clock, random) = match setup.deterministic {
            Some((started_at, seed)) => (
                Clock::Virtual {
                    creating: true,
                    started_at,
                    now: started_at,
                    seed,
                },
                Random::Seeded(seed ^ 0x9e37_79b9),
            ),
            None => (
                Clock::Real {
                    anchor: Instant::now(),
                },
                Random::Os,
            ),
        };
        Self {
            exports: None,
            bridge: None,
            limiter: MemoryLimiter::new(setup.linear_memory_bytes),
            control: setup.control,
            clock,
            random,
            events: setup.events,
            book: setup.book,
            deterministic: setup.deterministic.is_some(),
            issued: 0,
            settled: 0,
            output_chars: 0,
            output_items: 0,
            checking_stall: false,
            current_seq: 0,
            done: None,
            failure: None,
        }
    }

    pub(crate) fn emit(&self, event: Event) {
        // The owner may be gone; the run is being stopped then.
        let _ = self.events.send(event);
    }

    fn record(&mut self, failure: Failure) {
        if self.done.is_none() && self.failure.is_none() {
            self.failure = Some(failure);
        }
    }

    /// Turns an import's error into the trap that unwinds the guest,
    /// recording what it means for the outcome.
    pub(crate) fn trap(&mut self, error: GuestError) -> wasmtime::Error {
        match error {
            GuestError::Trap(error) => error,
            GuestError::Protocol(message) => {
                self.record(Failure::Protocol {
                    message: message.clone(),
                });
                wasmtime::Error::msg(message)
            }
            GuestError::OutOfMemory(message) => {
                self.record(Failure::OutOfMemory {
                    message: message.clone(),
                });
                wasmtime::Error::msg(message)
            }
            GuestError::OutputLimit(message) => {
                self.record(Failure::OutputLimit {
                    message: message.clone(),
                });
                wasmtime::Error::msg(message)
            }
            GuestError::TooLarge => {
                let message = "the guest passed a string longer than accepted".to_owned();
                self.record(Failure::Protocol {
                    message: message.clone(),
                });
                wasmtime::Error::msg(message)
            }
            GuestError::Finished => wasmtime::Error::msg("the script finished"),
        }
    }

    fn exports(&self) -> GuestResult<GuestExports> {
        self.exports.clone().ok_or_else(|| {
            GuestError::Protocol("the guest called the host before it was instantiated".to_owned())
        })
    }
}

type HostCaller<'a> = Caller<'a, HostState>;

/// Runs an import's body, turning its error into a trap.
fn guard<T>(
    caller: &mut HostCaller<'_>,
    body: impl FnOnce(&mut HostCaller<'_>) -> GuestResult<T>,
) -> wasmtime::Result<T> {
    body(caller).map_err(|error| caller.data_mut().trap(error))
}

/// Defines the twelve imports.
pub(crate) fn add_imports(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    const WASI: &str = "wasi_snapshot_preview1";
    linker.func_wrap(
        WASI,
        "clock_time_get",
        |mut caller: HostCaller<'_>, id: i32, _precision: i64, result: u32| {
            guard(&mut caller, |caller| clock_time_get(caller, id, result))
        },
    )?;
    linker.func_wrap(
        WASI,
        "random_get",
        |mut caller: HostCaller<'_>, ptr: u32, len: u32| {
            guard(&mut caller, |caller| random_get(caller, ptr, len))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_write",
        |mut caller: HostCaller<'_>, _fd: i32, iovs: u32, iovs_len: u32, written: u32| {
            guard(&mut caller, |caller| {
                fd_write(caller, iovs, iovs_len, written)
            })
        },
    )?;
    // The guest has no descriptors.
    linker.func_wrap(WASI, "fd_close", |_fd: i32| ERRNO_BADF)?;
    linker.func_wrap(
        WASI,
        "fd_seek",
        |_fd: i32, _offset: i64, _whence: i32, _result: u32| ERRNO_BADF,
    )?;
    linker.func_wrap(WASI, "fd_fdstat_get", |_fd: i32, _stat: u32| ERRNO_BADF)?;

    linker.func_wrap(
        "env",
        "host_call",
        |mut caller: HostCaller<'_>, name: u32, name_len: u32, _this: u32, argc: u32, argv: u32| {
            guard(&mut caller, |caller| {
                host_call(caller, name, name_len, argc, argv)
            })
        },
    )?;
    linker.func_wrap("env", "host_interrupt", |caller: HostCaller<'_>| {
        i32::from(caller.data().control.should_interrupt())
    })?;
    linker.func_wrap(
        "env",
        "host_promise_rejection",
        |mut caller: HostCaller<'_>, promise: u32, reason: u32, _handled: i32| {
            guard(&mut caller, |caller| {
                host_promise_rejection(caller, promise, reason)
            })
        },
    )?;
    // Scripts have no modules: the loader is never enabled, and a call is refused
    // with NULL, which QuickJS reports as a failed import.
    linker.func_wrap("env", "host_module_normalize", |_base: u32, _name: u32| {
        0u32
    })?;
    linker.func_wrap("env", "host_module_load", |_name: u32, _length: u32| 0u32)?;
    // Scripts run in UTC.
    linker.func_wrap(
        "env",
        "host_get_timezone_offset",
        |_high: i32, _low: i32| 0i32,
    )?;
    Ok(())
}

fn clock_time_get(caller: &mut HostCaller<'_>, id: i32, result: u32) -> GuestResult<i32> {
    let Some(nanoseconds) = caller.data().clock.read(id) else {
        return Ok(ERRNO_NOSYS);
    };
    let exports = caller.data().exports()?;
    write_bytes(
        exports.memory.data_mut(&mut *caller),
        result,
        &nanoseconds.to_le_bytes(),
    )?;
    Ok(ERRNO_SUCCESS)
}

fn random_get(caller: &mut HostCaller<'_>, ptr: u32, len: u32) -> GuestResult<i32> {
    let exports = caller.data().exports()?;
    let (memory, state) = exports.memory.data_and_store_mut(&mut *caller);
    state.random.fill(guest_slice_mut(memory, ptr, len)?)?;
    Ok(ERRNO_SUCCESS)
}

/// QuickJS's diagnostics on descriptors 1 and 2 belong to the host
/// application, not the script: discarded, every byte reported as written so
/// libc does not retry.
fn fd_write(
    caller: &mut HostCaller<'_>,
    iovs: u32,
    iovs_len: u32,
    written: u32,
) -> GuestResult<i32> {
    let exports = caller.data().exports()?;
    let memory = exports.memory.data_mut(&mut *caller);
    let list_len = iovs_len
        .checked_mul(8)
        .ok_or_else(|| GuestError::Protocol(format!("fd_write was passed {iovs_len} buffers")))?;
    let list = guest_slice(memory, iovs, list_len)?;
    let mut total: u32 = 0;
    for entry in list.as_chunks::<8>().0 {
        let len = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        total = total
            .checked_add(len)
            .ok_or_else(|| GuestError::Protocol("fd_write's buffers exceed 4 GiB".to_owned()))?;
    }
    write_bytes(memory, written, &total.to_le_bytes())?;
    Ok(ERRNO_SUCCESS)
}

/// Never enabled (Pi installs no rejection handler); `quickjs-wasi` frees the
/// two values when no handler is set, and so does this.
fn host_promise_rejection(
    caller: &mut HostCaller<'_>,
    promise: u32,
    reason: u32,
) -> GuestResult<()> {
    let exports = caller.data().exports()?;
    for value in [promise, reason] {
        // A value box holds one 8-byte NaN-boxed `JSValue`.
        guest_slice(exports.memory.data(&*caller), value, 8)?;
        exports.free_value.call(&mut *caller, value)?;
    }
    Ok(())
}

/// One bridge argument: `undefined` (or missing) as `None`, a string as its
/// text, anything else refused, since converting it would run script code.
fn string_arg(
    caller: &mut HostCaller<'_>,
    exports: &GuestExports,
    scratch: u32,
    value: Option<u32>,
    max_bytes: Option<usize>,
) -> GuestResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if exports.is_undefined.call(&mut *caller, value)? != 0 {
        return Ok(None);
    }
    if exports.is_string.call(&mut *caller, value)? == 0 {
        return Err(GuestError::Protocol(
            "a bridge argument is not a string".to_owned(),
        ));
    }
    let bytes = exports
        .string_bytes(&mut *caller, scratch, value, max_bytes)?
        .ok_or_else(|| GuestError::Protocol("a bridge string could not be read".to_owned()))?;
    Ok(Some(crate::guest::decode_wtf8(&bytes)))
}

fn required(value: Option<String>, what: &str) -> GuestResult<String> {
    value.ok_or_else(|| GuestError::Protocol(format!("the bridge's {what} is missing")))
}

/// A malformed payload from the prelude, which shares the built-ins with the
/// script and can be made to send one.
fn bridge_broken(what: &str) -> GuestError {
    GuestError::Protocol(format!(
        "Sandbox bridge broken: {what}. The script may have modified built-ins such as a prototype's toJSON."
    ))
}

fn parse_bridge_json(json: &str, what: &str) -> GuestResult<Value> {
    serde_json::from_str(&replace_lone_surrogate_escapes(json))
        .map_err(|_| bridge_broken(&format!("{what} is not valid JSON")))
}

/// The four hex digits of a `\uXXXX` escape starting at `at`.
fn unicode_escape(bytes: &[u8], at: usize) -> Option<u16> {
    let digits = bytes.get(at..at + 6)?.strip_prefix(b"\\u")?;
    let text = std::str::from_utf8(digits).ok()?;
    u16::from_str_radix(text, 16).ok()
}

/// Rewrites each `\uD800`-`\uDFFF` escape in a JSON string that is not half
/// of a high-low pair to `\uFFFD`. QuickJS's `JSON.stringify` escapes a lone
/// surrogate, as JavaScript requires, and JavaScript's `JSON.parse` accepts
/// it, but a Rust string cannot hold one and `serde_json` refuses the escape.
/// Strings leaving the VM unescaped turn lone surrogates into U+FFFD too
/// ([`crate::guest::decode_wtf8`]).
pub(crate) fn replace_lone_surrogate_escapes(json: &str) -> Cow<'_, str> {
    let bytes = json.as_bytes();
    let mut replaced: Option<String> = None;
    let mut copied = 0;
    let mut in_string = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                in_string = !in_string;
                index += 1;
            }
            b'\\' if in_string => {
                let Some(unit) = unicode_escape(bytes, index) else {
                    // Any other escape is two bytes; a malformed one is left
                    // for the parser to refuse.
                    index += 2;
                    continue;
                };
                let low = unicode_escape(bytes, index + 6);
                let paired = (0xd800..=0xdbff).contains(&unit)
                    && low.is_some_and(|low| (0xdc00..=0xdfff).contains(&low));
                if paired {
                    index += 12;
                } else if (0xd800..=0xdfff).contains(&unit) {
                    let out = replaced.get_or_insert_with(|| String::with_capacity(json.len()));
                    out.push_str(&json[copied..index]);
                    out.push_str("\\uFFFD");
                    index += 6;
                    copied = index;
                } else {
                    index += 6;
                }
            }
            _ => index += 1,
        }
    }
    match replaced {
        Some(mut out) => {
            out.push_str(&json[copied..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(json),
    }
}

/// Pi's `parseStoreWrites`: `[[key, json], [key]]`.
pub(crate) fn parse_store_writes(json: &str) -> GuestResult<StoreWrites> {
    let Value::Array(entries) = parse_bridge_json(json, "store writes")? else {
        return Err(bridge_broken("store writes are not an array"));
    };
    let mut writes = StoreWrites::default();
    for entry in entries {
        match entry.as_array().map(Vec::as_slice) {
            Some([Value::String(key)]) => writes.delete.push(key.clone()),
            Some([Value::String(key), Value::String(json)]) => {
                let value = parse_bridge_json(
                    json,
                    &format!(
                        "store value for {}",
                        crate::js::stringify(&Value::String(key.clone()))
                    ),
                )?;
                writes.set.insert(key.clone(), value);
            }
            _ => return Err(bridge_broken("store writes contain a malformed entry")),
        }
    }
    Ok(writes)
}

/// Pi's `parseScriptError`: `{ name?, message, stack? }`.
pub(crate) fn parse_script_error(json: &str) -> GuestResult<ScriptError> {
    let Value::Object(error) = parse_bridge_json(json, "script error")? else {
        return Err(bridge_broken("script error is not an object"));
    };
    let optional = |key: &str| match error.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(bridge_broken("script error is malformed")),
    };
    let name = optional("name")?;
    let stack = optional("stack")?;
    let Some(Value::String(message)) = error.get("message") else {
        return Err(bridge_broken("script error is malformed"));
    };
    Ok(ScriptError {
        name,
        message: message.clone(),
        stack,
    })
}

/// The prelude's bridge: `bridge(kind, a, b, c)` with kind `call` or `global`
/// (id, name, argsJson), `output` (`text` or `console`, text) or (`image`,
/// data, mimeType), or `done` (ok, valueJsonOrErrorJson, writesJson).
fn host_call(
    caller: &mut HostCaller<'_>,
    name: u32,
    name_len: u32,
    argc: u32,
    argv: u32,
) -> GuestResult<u32> {
    let exports = caller.data().exports()?;
    let bridge = caller.data().bridge.ok_or_else(|| {
        GuestError::Protocol("the bridge was called before it existed".to_owned())
    })?;
    let memory = exports.memory.data(&*caller);
    if guest_slice(memory, name, name_len)? != b"bridge" {
        return Err(GuestError::Protocol(
            "the guest called an unknown host function".to_owned(),
        ));
    }
    if argc > MAX_BRIDGE_ARGS {
        return Err(GuestError::Protocol(format!(
            "the bridge was called with {argc} arguments"
        )));
    }
    let args: Vec<u32> = guest_slice(memory, argv, argc * 4)?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    let arg = |index: usize| args.get(index).copied();
    let kind = required(
        string_arg(caller, &exports, bridge.scratch, arg(0), Some(16))?,
        "kind",
    )?;
    match kind.as_str() {
        "call" | "global" => {
            let id_value =
                arg(1).ok_or_else(|| GuestError::Protocol("a call has no id".to_owned()))?;
            let id = exports.get_float64.call(&mut *caller, id_value)?;
            if !(id.fract() == 0.0 && id >= 1.0 && id <= f64::from(u32::MAX)) {
                return Err(GuestError::Protocol(format!("a call has the id {id}")));
            }
            let id = id as u32;
            // A script that issues calls without awaiting them never lets
            // the VM apply an answer, so its calls, answers, and promises
            // would pile up until the heap limit; Bake 0.3's codemode
            // runtime failed such a run the same way (`maxPendingCalls`).
            let waiting = caller.data().issued.saturating_sub(caller.data().settled);
            if waiting as usize >= MAX_PENDING_CALLS {
                return Err(GuestError::Protocol(format!(
                    "The script has {MAX_PENDING_CALLS} calls waiting for results, the most a run allows; await calls before making more"
                )));
            }
            let name = required(
                string_arg(caller, &exports, bridge.scratch, arg(2), None)?,
                "call name",
            )?;
            let args = string_arg(caller, &exports, bridge.scratch, arg(3), None)?
                .map(|json| parse_bridge_json(&json, "call arguments"))
                .transpose()?;
            {
                let state = caller.data();
                let mut book = lock(&state.book);
                if !book.pending.insert(id) {
                    return Err(bridge_broken(&format!("duplicate call id {id}")));
                }
            }
            let state = caller.data_mut();
            state.issued = state.issued.saturating_add(1);
            state.emit(Event::Call(if kind == "call" {
                HostCall::Tool { id, name, args }
            } else {
                HostCall::Global { id, name, args }
            }));
        }
        "output" => {
            let sub = required(
                string_arg(caller, &exports, bridge.scratch, arg(1), Some(16))?,
                "output kind",
            )?;
            // A UTF-16 unit takes at most 3 bytes, so more bytes than three
            // times the remaining characters are over the limit before they
            // are copied.
            let remaining = MAX_OUTPUT_CHARS.saturating_sub(caller.data().output_chars);
            let data = match string_arg(
                caller,
                &exports,
                bridge.scratch,
                arg(2),
                Some(remaining.saturating_mul(3)),
            ) {
                Err(GuestError::TooLarge) => return Err(output_limit()),
                other => required(other?, "output data")?,
            };
            let state = caller.data_mut();
            state.output_chars = state.output_chars.saturating_add(utf16_len(&data));
            state.output_items += 1;
            if state.output_chars > MAX_OUTPUT_CHARS || state.output_items > MAX_OUTPUT_ITEMS {
                return Err(output_limit());
            }
            let item = match sub.as_str() {
                "text" => OutputItem::Text {
                    text: data,
                    console: false,
                },
                "console" => OutputItem::Text {
                    text: data,
                    console: true,
                },
                "image" => {
                    let mime_type = required(
                        string_arg(caller, &exports, bridge.scratch, arg(3), Some(64))?,
                        "image type",
                    )?;
                    OutputItem::Image { data, mime_type }
                }
                _ => {
                    return Err(GuestError::Protocol(
                        "the bridge sent an unknown output kind".to_owned(),
                    ));
                }
            };
            caller.data().emit(Event::Output(item));
        }
        "done" => {
            // A stop decided first wins, as Pi's host ignores a `done` after it.
            if caller.data().control.stopped() {
                return Err(GuestError::Finished);
            }
            let ok_value =
                arg(1).ok_or_else(|| GuestError::Protocol("done has no status".to_owned()))?;
            let ok = exports.get_bool.call(&mut *caller, ok_value)? != 0;
            let payload = string_arg(caller, &exports, bridge.scratch, arg(2), None)?;
            let result = if ok {
                let writes = required(
                    string_arg(caller, &exports, bridge.scratch, arg(3), None)?,
                    "store writes",
                )?;
                let value = payload
                    .map(|json| parse_bridge_json(&json, "return value"))
                    .transpose()?;
                let store_writes = parse_store_writes(&writes)?;
                Ok(Completion {
                    value,
                    store_writes,
                })
            } else {
                let error = parse_script_error(&required(payload, "script error")?)?;
                Err(if caller.data().checking_stall {
                    Failure::NeverSettles(error)
                } else {
                    Failure::Exception(error)
                })
            };
            let state = caller.data_mut();
            let during = state.deterministic.then_some(During {
                seq: state.current_seq,
                issued: state.issued,
            });
            if state.done.is_none() && state.failure.is_none() {
                state.done = Some(Outcome { result, during });
            }
            return Err(GuestError::Finished);
        }
        _ => return Err(bridge_broken("unknown message from the VM")),
    }
    Ok(exports.dup_value.call(&mut *caller, bridge.undefined)?)
}

fn output_limit() -> GuestError {
    GuestError::OutputLimit(format!(
        "script output exceeded the limit of {MAX_OUTPUT_CHARS} characters or {MAX_OUTPUT_ITEMS} text(), image(), and console calls"
    ))
}

/// A JSON object of store values for the prelude: key to JSON text.
pub(crate) fn store_json(store: &Map<String, Value>) -> String {
    let serialized: Map<String, Value> = store
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.to_string())))
        .collect();
    Value::Object(serialized).to_string()
}

#[cfg(test)]
mod tests {
    use wasmtime::{Store, TypedFunc, WasmParams, WasmResults};

    use super::*;
    use crate::engine::CodemodeEngine;

    /// A store with an initialized VM, whose imports the tests call
    /// directly with forged pointers and lengths.
    struct Vm {
        store: Store<HostState>,
        linker: Linker<HostState>,
        exports: GuestExports,
        control: Arc<RunControl>,
    }

    fn vm() -> Vm {
        let engine = match CodemodeEngine::get() {
            Ok(engine) => engine,
            Err(error) => panic!("{error}"),
        };
        let control = Arc::new(RunControl::new(engine.wasmtime_engine().clone(), None));
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        let mut store = Store::new(
            engine.wasmtime_engine(),
            HostState::new(HostSetup {
                control: Arc::clone(&control),
                events,
                book: Arc::default(),
                linear_memory_bytes: 64 << 20,
                deterministic: None,
            }),
        );
        store.limiter(|state| &mut state.limiter);
        store.set_epoch_deadline(1 << 40);
        let mut linker = Linker::new(engine.wasmtime_engine());
        let setup = add_imports(&mut linker)
            .and_then(|()| engine.instance_pre().instantiate(&mut store))
            .and_then(|instance| GuestExports::new(&instance, &mut store));
        let exports = match setup {
            Ok(exports) => exports,
            Err(error) => panic!("{error:#}"),
        };
        store.data_mut().exports = Some(exports.clone());
        let init = exports
            .initialize
            .call(&mut store, ())
            .and_then(|()| exports.init.call(&mut store, ()))
            .and_then(|_| exports.malloc.call(&mut store, 8))
            .and_then(|scratch| Ok((scratch, exports.get_undefined.call(&mut store, ())?)));
        let (scratch, undefined) = match init {
            Ok(values) => values,
            Err(error) => panic!("{error:#}"),
        };
        store.data_mut().bridge = Some(BridgeValues { scratch, undefined });
        Vm {
            store,
            linker,
            exports,
            control,
        }
    }

    impl Vm {
        fn import<P: WasmParams, R: WasmResults>(
            &mut self,
            module: &str,
            name: &str,
        ) -> TypedFunc<P, R> {
            let func = self
                .linker
                .get(&mut self.store, module, name)
                .ok()
                .and_then(wasmtime::Extern::into_func);
            match func.map(|func| func.typed::<P, R>(&self.store)) {
                Some(Ok(func)) => func,
                _ => panic!("no import {module}.{name}"),
            }
        }

        fn host_call(&mut self, args: (u32, u32, u32, u32, u32)) -> wasmtime::Result<u32> {
            let func = self.import::<(u32, u32, u32, u32, u32), u32>("env", "host_call");
            func.call(&mut self.store, args)
        }

        fn cstring(&mut self, text: &str) -> u32 {
            match self.exports.write_cstring(&mut self.store, text.as_bytes()) {
                Ok(ptr) => ptr,
                Err(error) => panic!("{error}"),
            }
        }

        fn words(&mut self, words: &[u32]) -> u32 {
            let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
            let ptr = match self
                .exports
                .malloc
                .call(&mut self.store, bytes.len() as u32)
            {
                Ok(ptr) => ptr,
                Err(error) => panic!("{error:#}"),
            };
            assert!(
                write_bytes(self.exports.memory.data_mut(&mut self.store), ptr, &bytes).is_ok()
            );
            ptr
        }

        fn string_value(&mut self, text: &str) -> u32 {
            match self.exports.new_string_value(&mut self.store, text) {
                Ok(value) => value,
                Err(error) => panic!("{error}"),
            }
        }

        fn memory_size(&self) -> u32 {
            self.exports.memory.data_size(&self.store) as u32
        }

        /// The failure the refused call recorded.
        fn protocol_failure(&mut self) -> String {
            match self.store.data_mut().failure.take() {
                Some(Failure::Protocol { message }) => message,
                other => panic!("expected a protocol failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn host_call_refuses_forged_names_argument_lists_and_values() {
        let mut vm = vm();
        let end = vm.memory_size();
        for args in [
            (end - 2, 16, 0, 0, 0),
            (8, u32::MAX, 0, 0, 0),
            (u32::MAX, 1, 0, 0, 0),
        ] {
            assert!(vm.host_call(args).is_err(), "{args:?}");
            assert!(vm.protocol_failure().contains("outside its"), "{args:?}");
        }
        let other = vm.cstring("bridgx");
        assert!(vm.host_call((other, 6, 0, 0, 0)).is_err());
        assert!(vm.protocol_failure().contains("unknown host function"));

        let name = vm.cstring("bridge");
        for (argc, argv) in [
            (4, end),
            (4, u32::MAX - 3),
            (4, end - 8),
            (1 << 30, 16),
            (u32::MAX, 16),
        ] {
            assert!(
                vm.host_call((name, 6, 0, argc, argv)).is_err(),
                "{argc} {argv}"
            );
            vm.protocol_failure();
        }
        // In-bounds argument lists holding forged value pointers: the guest's
        // own reads of them trap, and nothing on the host panics.
        for forged in [end, u32::MAX - 3, end - 4] {
            let argv = vm.words(&[forged, forged, forged, forged]);
            assert!(vm.host_call((name, 6, 0, 4, argv)).is_err(), "{forged}");
        }
    }

    #[test]
    fn host_call_refuses_unknown_and_malformed_bridge_messages() {
        let mut vm = vm();
        let name = vm.cstring("bridge");
        let kind = vm.string_value("nonsense");
        let argv = vm.words(&[kind]);
        assert!(vm.host_call((name, 6, 0, 1, argv)).is_err());
        assert_eq!(
            vm.protocol_failure(),
            "Sandbox bridge broken: unknown message from the VM. The script may have modified built-ins such as a prototype's toJSON."
        );
        // A kind that is not a string is refused before any conversion runs.
        let number = match vm.exports.new_number.call(&mut vm.store, 1.0) {
            Ok(value) => value,
            Err(error) => panic!("{error:#}"),
        };
        let argv = vm.words(&[number]);
        assert!(vm.host_call((name, 6, 0, 1, argv)).is_err());
        assert!(vm.protocol_failure().contains("not a string"));
        // A call with a fractional id.
        let call = vm.string_value("call");
        let id = match vm.exports.new_number.call(&mut vm.store, 1.5) {
            Ok(value) => value,
            Err(error) => panic!("{error:#}"),
        };
        let tool = vm.string_value("echo");
        let argv = vm.words(&[call, id, tool]);
        assert!(vm.host_call((name, 6, 0, 3, argv)).is_err());
        assert!(vm.protocol_failure().contains("id 1.5"));
        // A well-formed call is accepted.
        let id = match vm.exports.new_number.call(&mut vm.store, 1.0) {
            Ok(value) => value,
            Err(error) => panic!("{error:#}"),
        };
        let argv = vm.words(&[call, id, tool]);
        assert!(vm.host_call((name, 6, 0, 3, argv)).is_ok());
        assert_eq!(vm.store.data().issued, 1);
        // The same id again is a broken bridge.
        assert!(vm.host_call((name, 6, 0, 3, argv)).is_err());
        assert!(vm.protocol_failure().contains("duplicate call id 1"));
    }

    #[test]
    fn wasi_imports_refuse_forged_pointers() {
        let mut vm = vm();
        let end = vm.memory_size();
        let clock = vm.import::<(i32, i64, u32), i32>("wasi_snapshot_preview1", "clock_time_get");
        for ptr in [end - 4, u32::MAX] {
            assert!(clock.call(&mut vm.store, (0, 0, ptr)).is_err());
            vm.protocol_failure();
        }
        let scratch = vm.words(&[0, 0]);
        assert_eq!(
            clock.call(&mut vm.store, (0, 0, scratch)).ok(),
            Some(ERRNO_SUCCESS)
        );
        assert_eq!(
            clock.call(&mut vm.store, (7, 0, scratch)).ok(),
            Some(ERRNO_NOSYS)
        );

        let random = vm.import::<(u32, u32), i32>("wasi_snapshot_preview1", "random_get");
        for (ptr, len) in [(end - 2, 4), (8, u32::MAX), (u32::MAX, 1)] {
            assert!(
                random.call(&mut vm.store, (ptr, len)).is_err(),
                "{ptr} {len}"
            );
            vm.protocol_failure();
        }
        assert_eq!(
            random.call(&mut vm.store, (scratch, 8)).ok(),
            Some(ERRNO_SUCCESS)
        );

        let write = vm.import::<(i32, u32, u32, u32), i32>("wasi_snapshot_preview1", "fd_write");
        let iovs = vm.words(&[0, 5, 0, 7]);
        let huge = vm.words(&[0, u32::MAX, 0, 2]);
        for (iovs, count, written) in [
            (end - 4, 1, scratch),
            (iovs, u32::MAX, scratch),
            (iovs, 1 << 29, scratch),
            (iovs, 2, end - 2),
            (huge, 2, scratch),
        ] {
            assert!(
                write
                    .call(&mut vm.store, (1, iovs, count, written))
                    .is_err(),
                "{iovs} {count} {written}"
            );
            vm.protocol_failure();
        }
        assert_eq!(
            write.call(&mut vm.store, (2, iovs, 2, scratch)).ok(),
            Some(ERRNO_SUCCESS)
        );
        let written = crate::guest::read_u32(vm.exports.memory.data(&vm.store), scratch).ok();
        assert_eq!(written, Some(12));

        let close = vm.import::<i32, i32>("wasi_snapshot_preview1", "fd_close");
        assert_eq!(close.call(&mut vm.store, 1).ok(), Some(ERRNO_BADF));
        let seek = vm.import::<(i32, i64, i32, u32), i32>("wasi_snapshot_preview1", "fd_seek");
        assert_eq!(
            seek.call(&mut vm.store, (1, 0, 0, u32::MAX)).ok(),
            Some(ERRNO_BADF)
        );
        let stat = vm.import::<(i32, u32), i32>("wasi_snapshot_preview1", "fd_fdstat_get");
        assert_eq!(
            stat.call(&mut vm.store, (1, u32::MAX)).ok(),
            Some(ERRNO_BADF)
        );
    }

    #[test]
    fn env_imports_refuse_forged_pointers() {
        let mut vm = vm();
        let end = vm.memory_size();
        let rejection = vm.import::<(u32, u32, i32), ()>("env", "host_promise_rejection");
        for (promise, reason) in [(end - 4, 8), (u32::MAX, u32::MAX)] {
            assert!(rejection.call(&mut vm.store, (promise, reason, 0)).is_err());
            vm.protocol_failure();
        }
        let normalize = vm.import::<(u32, u32), u32>("env", "host_module_normalize");
        assert_eq!(
            normalize.call(&mut vm.store, (u32::MAX, u32::MAX)).ok(),
            Some(0)
        );
        let load = vm.import::<(u32, u32), u32>("env", "host_module_load");
        assert_eq!(load.call(&mut vm.store, (u32::MAX, u32::MAX)).ok(), Some(0));
        let timezone = vm.import::<(i32, i32), i32>("env", "host_get_timezone_offset");
        assert_eq!(timezone.call(&mut vm.store, (-1, -1)).ok(), Some(0));
        let interrupt = vm.import::<(), i32>("env", "host_interrupt");
        assert_eq!(interrupt.call(&mut vm.store, ()).ok(), Some(0));
        vm.control.stop(StopReason::Aborted);
        assert_eq!(interrupt.call(&mut vm.store, ()).ok(), Some(1));
        assert!(vm.store.data().failure.is_none());
    }

    fn broken(result: GuestResult<impl std::fmt::Debug>) -> String {
        match result {
            Err(GuestError::Protocol(message)) => message,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// Pi's "reports a broken bridge as a sandbox error" cases, against the
    /// decoder its raw worker fixture exercised.
    #[test]
    fn reports_a_broken_bridge() {
        let prefix = |reason: &str| format!("Sandbox bridge broken: {reason}");
        assert!(
            broken(parse_store_writes("null"))
                .starts_with(&prefix("store writes are not an array"))
        );
        assert!(
            broken(parse_store_writes("[1]"))
                .starts_with(&prefix("store writes contain a malformed entry"))
        );
        assert!(
            broken(parse_store_writes(r#"[["k", "{"]]"#))
                .starts_with(&prefix("store value for \"k\" is not valid JSON"))
        );
        assert!(
            broken(parse_bridge_json("{", "return value"))
                .starts_with(&prefix("return value is not valid JSON"))
        );
        assert!(
            broken(parse_script_error("5")).starts_with(&prefix("script error is not an object"))
        );
        assert!(broken(parse_script_error("{}")).starts_with(&prefix("script error is malformed")));
        let writes = parse_store_writes(r#"[["a", "1"], ["b"]]"#).ok();
        assert_eq!(
            writes.map(|writes| (Value::Object(writes.set), writes.delete)),
            Some((serde_json::json!({ "a": 1 }), vec!["b".to_owned()]))
        );
    }

    #[test]
    fn replaces_only_escaped_lone_surrogates_inside_strings() {
        let cases = [
            (r#""\ud83d""#, r#""\uFFFD""#),
            (r#""\uDC00x""#, r#""\uFFFDx""#),
            (
                r#"["\ud800\ud800", "\udfff\ud83d"]"#,
                r#"["\uFFFD\uFFFD", "\uFFFD\uFFFD"]"#,
            ),
            // A high surrogate followed by a non-surrogate escape or text.
            (r#""\ud800\u0041\ud800a""#, r#""\uFFFD\u0041\uFFFDa""#),
            (r#""é\ud800""#, r#""é\uFFFD""#),
        ];
        for (json, expected) in cases {
            assert_eq!(replace_lone_surrogate_escapes(json), expected, "{json}");
        }
        for unchanged in [
            r#""\ud83d\ude00""#,
            r#""\\ud800""#,
            r#""\"\ud83d\ude00\\""#,
            r#"{"a": "\u00e9", "b": 1}"#,
            // Malformed escapes are left for the parser to refuse.
            r#""\ud8""#,
            r#""\"#,
        ] {
            assert!(
                matches!(replace_lone_surrogate_escapes(unchanged), Cow::Borrowed(text) if text == unchanged),
                "{unchanged}"
            );
        }
        assert_eq!(
            parse_bridge_json(r#"{"\ud800": ["\udfff", "😀"]}"#, "test").ok(),
            Some(serde_json::json!({ "\u{fffd}": ["\u{fffd}", "😀"] }))
        );
    }

    #[test]
    fn the_seeded_generator_is_pis_mulberry32() {
        // mulberry32(1 ^ 0x9e3779b9)'s first draw, little-endian, as Pi's
        // deterministic `random_get` writes it.
        let mut random = Random::Seeded(1 ^ 0x9e37_79b9);
        let mut bytes = [0u8; 6];
        assert!(random.fill(&mut bytes).is_ok());
        let mut again = Random::Seeded(1 ^ 0x9e37_79b9);
        let mut same = [0u8; 6];
        assert!(again.fill(&mut same).is_ok());
        assert_eq!(bytes, same);
        assert_ne!(bytes, [0; 6]);
    }
}
