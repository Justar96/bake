//! Drives one QuickJS VM through one script on the run's thread; ported from
//! Pi v1.3.0's `packages/codemode/src/runtime/vm.ts` (MIT, see `NOTICE`),
//! with `quickjs-wasi`'s `QuickJS.create` (MIT) for the VM's setup.
//!
//! The contract is `vm.ts`'s:
//! - The VM runs only when the host delivers something: the script's first
//!   run, then each answer.
//! - Promise jobs run in batches of [`JOBS_PER_POLL`], with the stop flag
//!   checked between batches, because a script such as `while (true) await
//!   null` executes only a few opcodes per job, so QuickJS's own interrupt
//!   polling would let it run for a long time between polls.
//! - After each drain the prelude's `stalled()` fails a script that waits on
//!   nothing that can ever resume it ([`Failure::NeverSettles`]).
//! - The VM never decides on its own that it is finished: the prelude reports
//!   the end through the bridge, which unwinds the guest.
//!
//! Deterministic mode fixes what remains nondeterministic: the clock and
//! randomness (see `host.rs`), UTC, no `WeakRef` or `FinalizationRegistry`,
//! and answers applied strictly in `seq` order, early ones buffered. The VM
//! reports [`Event::Started`] with its fingerprint before the script runs and
//! [`Event::Idle`] after each drain.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use wasmtime::{InstancePre, Store, UpdateDeadline};

use crate::guest::{GuestError, GuestExports, GuestResult};
use crate::host::{BridgeValues, CallBook, Clock, HostSetup, HostState, RunControl, StopReason};
use crate::prelude;
use crate::types::{
    CallResult, Deterministic, Event, Failure, Limits, MAX_STACK_BYTES, Outcome, ScriptError,
};

/// Promise jobs run between two stop checks while draining the job queue.
const JOBS_PER_POLL: u32 = 50;
/// Bumped whenever this module or the prelude changes what a deterministic
/// script observes beyond what the fingerprint's other parts capture.
const DETERMINISTIC_VERSION: u32 = 1;
/// `quickjs-wasi`'s `Intrinsics.ALL` and `Intrinsics.WEAK_REF`.
const INTRINSICS_ALL: u32 = 0xffff_ffff;
const INTRINSIC_WEAK_REF: u32 = 1 << 9;
/// The engine's file names; the prelude drops its own frames from stacks.
const PRELUDE_FILE: &str = "codemode-prelude.js";
const SCRIPT_FILE: &str = "codemode.js";

/// An answer for the script thread.
pub(crate) struct Delivery {
    pub(crate) id: u32,
    pub(crate) result: CallResult,
    /// Deterministic mode: the result number and the clock while it is processed.
    pub(crate) order: Option<(u32, f64)>,
}

/// What the owner sends the script thread.
pub(crate) enum Command {
    Answer(Delivery),
    /// Stop now; the stop reason is already recorded.
    Stop,
}

/// The script and the prelude's arguments, as JSON text.
pub(crate) struct Script {
    pub(crate) code: String,
    pub(crate) tools_json: String,
    pub(crate) globals_json: String,
    pub(crate) store_json: String,
}

/// Everything the script thread needs.
pub(crate) struct ScriptInput {
    pub(crate) pre: InstancePre<HostState>,
    pub(crate) script: Script,
    pub(crate) limits: Limits,
    pub(crate) deterministic: Option<Deterministic>,
    pub(crate) control: Arc<RunControl>,
    pub(crate) events: UnboundedSender<Event>,
    pub(crate) book: Arc<Mutex<CallBook>>,
    pub(crate) commands: Receiver<Command>,
}

/// The VM's long-lived values, owned until the store is dropped.
struct Values {
    api: u32,
    settle: u32,
    run: u32,
    stalled: u32,
    undefined: u32,
    true_value: u32,
    false_value: u32,
    scratch: u32,
}

struct Driver {
    store: Store<HostState>,
    exports: GuestExports,
    values: Values,
    control: Arc<RunControl>,
}

/// Runs the script to its outcome. The store, and with it the VM, is dropped
/// on return.
pub(crate) fn run_script(input: ScriptInput) -> Outcome {
    let ScriptInput {
        pre,
        script,
        limits,
        deterministic,
        control,
        events,
        book,
        commands,
    } = input;
    let mut store = Store::new(
        pre.module().engine(),
        HostState::new(HostSetup {
            control: Arc::clone(&control),
            events,
            book,
            linear_memory_bytes: limits.linear_memory_bytes,
            deterministic: deterministic.map(|mode| (mode.started_at_ms, mode.seed)),
        }),
    );
    store.limiter(|state| &mut state.limiter);
    // Every epoch increment, from any run's stop, makes each running store
    // check its own flag and deadline.
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|context| {
        if context.data().control.should_interrupt() {
            Err(wasmtime::Error::msg("the script was interrupted"))
        } else {
            Ok(UpdateDeadline::Continue(1))
        }
    });
    if control.should_interrupt() {
        return conclude_store(&mut store, &control, GuestError::Finished);
    }
    let mut driver = match Driver::start(store, &pre, limits, deterministic, &control) {
        Ok(driver) => driver,
        Err((mut store, error)) => return conclude_store(&mut store, &control, error),
    };
    let result = driver.run(&script, limits, deterministic.is_some(), &commands);
    match result {
        Ok(outcome) => outcome,
        Err(error) => conclude_store(&mut driver.store, &control, error),
    }
}

/// The outcome once the VM stopped with `error`: the end the script reported,
/// a failure an import recorded, the memory limit, a stop, then `error`.
fn conclude_store(
    store: &mut Store<HostState>,
    control: &RunControl,
    error: GuestError,
) -> Outcome {
    let state = store.data_mut();
    if let Some(done) = state.done.take() {
        return done;
    }
    if let Some(failure) = state.failure.take() {
        return Outcome::failed(failure);
    }
    if let Some(message) = state.limiter.exceeded.take() {
        return Outcome::failed(Failure::OutOfMemory { message });
    }
    if let Some(failure) = control.failure() {
        return Outcome::failed(failure);
    }
    Outcome::failed(match error {
        GuestError::Protocol(message) => Failure::Protocol { message },
        GuestError::OutOfMemory(message) => Failure::OutOfMemory { message },
        GuestError::OutputLimit(message) => Failure::OutputLimit { message },
        GuestError::Trap(error) => Failure::Protocol {
            message: format!("The script's VM failed: {error:#}"),
        },
        GuestError::TooLarge | GuestError::Finished => Failure::Protocol {
            message: "The script's VM stopped without a result".to_owned(),
        },
    })
}

impl Driver {
    /// Instantiates the module and creates the runtime, context, and bridge
    /// values, as `QuickJS.create` does.
    fn start(
        mut store: Store<HostState>,
        pre: &InstancePre<HostState>,
        limits: Limits,
        deterministic: Option<Deterministic>,
        control: &Arc<RunControl>,
    ) -> Result<Self, (Store<HostState>, GuestError)> {
        let setup = (|| -> GuestResult<(GuestExports, Values)> {
            let instance = pre.instantiate(&mut store)?;
            let exports = GuestExports::new(&instance, &mut store)?;
            store.data_mut().exports = Some(exports.clone());
            exports.initialize.call(&mut store, ())?;
            let status = if deterministic.is_some() {
                exports
                    .init2
                    .call(&mut store, INTRINSICS_ALL & !INTRINSIC_WEAK_REF)?
            } else {
                exports.init.call(&mut store, ())?
            };
            if status != 0 {
                return Err(GuestError::Protocol(
                    "Failed to initialize QuickJS runtime".to_owned(),
                ));
            }
            if let Clock::Virtual { creating, .. } = &mut store.data_mut().clock {
                *creating = false;
            }
            if let Some(heap) = limits.heap_bytes {
                exports.set_memory_limit.call(&mut store, heap)?;
            }
            // Without a guard, deep recursion overflows the Wasm stack and traps
            // instead of throwing a catchable RangeError.
            exports
                .set_max_stack_size
                .call(&mut store, MAX_STACK_BYTES)?;
            exports.set_interrupt_handler.call(&mut store, 1)?;
            let scratch = exports.malloc.call(&mut store, 8)?;
            if scratch == 0 {
                return Err(GuestError::OutOfMemory(
                    "the guest could not allocate 8 bytes".to_owned(),
                ));
            }
            let undefined = exports.get_undefined.call(&mut store, ())?;
            let true_value = exports.get_true.call(&mut store, ())?;
            let false_value = exports.get_false.call(&mut store, ())?;
            store.data_mut().bridge = Some(BridgeValues { scratch, undefined });
            Ok((
                exports,
                Values {
                    api: 0,
                    settle: 0,
                    run: 0,
                    stalled: 0,
                    undefined,
                    true_value,
                    false_value,
                    scratch,
                },
            ))
        })();
        match setup {
            Ok((exports, values)) => Ok(Self {
                store,
                exports,
                values,
                control: Arc::clone(control),
            }),
            Err(error) => Err((store, error)),
        }
    }

    fn run(
        &mut self,
        script: &Script,
        limits: Limits,
        deterministic: bool,
        commands: &Receiver<Command>,
    ) -> GuestResult<Outcome> {
        self.load_prelude(script)?;
        if deterministic {
            let fingerprint = self.fingerprint(limits)?;
            self.store.data().emit(Event::Started { fingerprint });
        }

        // The prefix shares the first line with the script so reported line
        // numbers match the script as written.
        let wrapped = format!("(async (tools, console) => {{{}\n}})", script.code);
        let function = self
            .exports
            .eval_raw(&mut self.store, &wrapped, SCRIPT_FILE)?;
        if self.exports.is_exception.call(&mut self.store, function)? != 0 {
            self.free(function)?;
            if self.control.should_interrupt() {
                return Err(GuestError::Finished);
            }
            let error = self.describe_exception()?;
            return Ok(Outcome::failed(Failure::Exception(error)));
        }
        let result = self.exports.call_raw(
            &mut self.store,
            self.values.run,
            self.values.api,
            &[function],
        );
        self.free(function)?;
        self.consume(result?)?;
        self.drain()?;
        if deterministic {
            let issued = self.store.data().issued;
            self.store.data().emit(Event::Idle { seq: 0, issued });
        }

        let mut early: BTreeMap<u32, Delivery> = BTreeMap::new();
        let mut next_seq: u32 = 1;
        loop {
            let command = match self.control.deadline() {
                Some(deadline) => match commands
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => {
                        self.control.stop(StopReason::TimedOut);
                        return Err(GuestError::Finished);
                    }
                    Err(RecvTimeoutError::Disconnected) => Command::Stop,
                },
                None => commands.recv().unwrap_or(Command::Stop),
            };
            let delivery = match command {
                Command::Answer(delivery) => delivery,
                Command::Stop => {
                    // A dropped owner without a recorded reason aborts.
                    self.control.stop(StopReason::Aborted);
                    return Err(GuestError::Finished);
                }
            };
            if self.control.should_interrupt() {
                return Err(GuestError::Finished);
            }
            let Some((seq, _)) = delivery.order else {
                self.apply(&delivery)?;
                continue;
            };
            early.insert(seq, delivery);
            while let Some(delivery) = early.remove(&next_seq) {
                let now = delivery.order.map_or(f64::NAN, |(_, now)| now);
                let state = self.store.data_mut();
                if let Clock::Virtual { now: clock, .. } = &mut state.clock {
                    // Never backwards: a resumed host's clock may be behind the one that started the run.
                    if now.is_finite() {
                        *clock = clock.max(now);
                    }
                }
                state.current_seq = next_seq;
                self.apply(&delivery)?;
                let issued = self.store.data().issued;
                self.store.data().emit(Event::Idle {
                    seq: next_seq,
                    issued,
                });
                next_seq = next_seq.saturating_add(1);
            }
        }
    }

    /// Evaluates the prelude and calls it with the bridge and the run's
    /// declarations and store.
    fn load_prelude(&mut self, script: &Script) -> GuestResult<()> {
        let name = self.exports.write_cstring(&mut self.store, b"bridge")?;
        let bridge = self
            .exports
            .new_host_function
            .call(&mut self.store, (name, 6, 0));
        self.exports.free.call(&mut self.store, name)?;
        let bridge = bridge?;
        let prelude = self
            .exports
            .eval_raw(&mut self.store, prelude::source(), PRELUDE_FILE)?;
        self.check(prelude, "the script environment failed to load")?;
        let mut args = vec![bridge];
        for json in [&script.tools_json, &script.globals_json, &script.store_json] {
            args.push(self.exports.new_string_value(&mut self.store, json)?);
        }
        let api = self
            .exports
            .call_raw(&mut self.store, prelude, self.values.undefined, &args)?;
        for value in args {
            self.free(value)?;
        }
        self.free(prelude)?;
        self.check(api, "the script environment failed to start")?;
        self.values.api = api;
        self.values.settle = self.exports.get_prop(&mut self.store, api, "settle")?;
        self.values.run = self.exports.get_prop(&mut self.store, api, "run")?;
        self.values.stalled = self.exports.get_prop(&mut self.store, api, "stalled")?;
        Ok(())
    }

    /// Pi's fingerprint: what the VM is, so a replay can refuse a different one.
    fn fingerprint(&mut self, limits: Limits) -> GuestResult<String> {
        let version = self.exports.get_quickjs_version.call(&mut self.store, ())?;
        let quickjs = self.exports.read_c_string(&self.store, version, 64)?;
        let heap = limits
            .heap_bytes
            .map_or_else(|| "none".to_owned(), |heap| heap.to_string());
        Ok(format!(
            "quickjs-wasi {}, quickjs {quickjs}, prelude {}, deterministic {DETERMINISTIC_VERSION}, stack {MAX_STACK_BYTES}, heap {heap}, budget none",
            crate::engine::QUICKJS_WASI_VERSION,
            prelude::hash()
        ))
    }

    /// Settles call `id` in the prelude, then runs the script until it waits again.
    fn apply(&mut self, delivery: &Delivery) -> GuestResult<()> {
        self.store.data_mut().settle(delivery.id);
        let (ok, payload, properties) = match &delivery.result {
            Ok(value) => (true, value.as_ref().map(Value::to_string), None),
            Err(error) => (
                false,
                Some(error.message.clone()),
                error
                    .properties
                    .as_ref()
                    .map(|properties| Value::Object(properties.clone()).to_string()),
            ),
        };
        let mut owned = Vec::new();
        let id = self
            .exports
            .new_number
            .call(&mut self.store, f64::from(delivery.id))?;
        owned.push(id);
        let mut args = vec![
            id,
            if ok {
                self.values.true_value
            } else {
                self.values.false_value
            },
        ];
        for text in [payload, properties] {
            match text {
                Some(text) => {
                    let value = self.exports.new_string_value(&mut self.store, &text);
                    let value = match value {
                        Ok(value) => value,
                        Err(error) => {
                            for value in owned {
                                self.free(value)?;
                            }
                            return Err(error);
                        }
                    };
                    owned.push(value);
                    args.push(value);
                }
                None => args.push(self.values.undefined),
            }
        }
        let result =
            self.exports
                .call_raw(&mut self.store, self.values.settle, self.values.api, &args);
        for value in owned {
            self.free(value)?;
        }
        self.consume(result?)?;
        self.drain()
    }

    /// Runs queued jobs, then fails a script that waits on nothing that can
    /// ever resume it.
    fn drain(&mut self) -> GuestResult<()> {
        let mut jobs: u32 = 0;
        while self.exports.is_job_pending.call(&mut self.store, ())? != 0 {
            // A job only fails for uncatchable errors, such as an interrupt.
            if self.exports.execute_pending_job.call(&mut self.store, ())? < 0 {
                if self.control.should_interrupt() {
                    return Err(GuestError::Finished);
                }
                return Err(GuestError::Protocol("Promise job failed".to_owned()));
            }
            jobs = jobs.wrapping_add(1);
            if jobs.is_multiple_of(JOBS_PER_POLL) && self.control.should_interrupt() {
                return Err(GuestError::Finished);
            }
        }
        self.store.data_mut().checking_stall = true;
        let result =
            self.exports
                .call_raw(&mut self.store, self.values.stalled, self.values.api, &[]);
        self.store.data_mut().checking_stall = false;
        self.consume(result?)
    }

    /// Frees a call's result, failing the run when it is an exception.
    fn consume(&mut self, value: u32) -> GuestResult<()> {
        self.check(value, "the script environment failed")?;
        self.free(value)
    }

    /// Fails with the pending exception's `name: message` when `value` is an
    /// exception, as Pi's `crashMessage`, or with the stop when stopped.
    fn check(&mut self, value: u32, what: &str) -> GuestResult<()> {
        if self.exports.is_exception.call(&mut self.store, value)? == 0 {
            return Ok(());
        }
        self.free(value)?;
        if self.control.should_interrupt() {
            return Err(GuestError::Finished);
        }
        let error = self.describe_exception()?;
        Err(GuestError::Protocol(format!(
            "{what}: {}: {}",
            error.name.as_deref().unwrap_or("Error"),
            error.message
        )))
    }

    /// The pending exception as Pi's `describeException` reports it.
    fn describe_exception(&mut self) -> GuestResult<ScriptError> {
        let exception = self.exports.get_exception.call(&mut self.store, ())?;
        let described = self.describe(exception);
        self.free(exception)?;
        described
    }

    fn describe(&mut self, exception: u32) -> GuestResult<ScriptError> {
        let name = match self.property_text(exception, "name")? {
            Some(name) => name,
            None => "Error".to_owned(),
        };
        let message = match self.property_text(exception, "message")? {
            Some(message) => message,
            None => self
                .exports
                .string(&mut self.store, self.values.scratch, exception)?,
        };
        let stack = self.property_text(exception, "stack")?;
        let head = if message.is_empty() {
            name.clone()
        } else {
            format!("{name}: {message}")
        };
        let stack = match stack.as_deref().map(str::trim_end) {
            Some(frames) if !frames.is_empty() => format!("{head}\n{frames}"),
            _ => head,
        };
        Ok(ScriptError {
            name: Some(name),
            message,
            stack: Some(stack),
        })
    }

    /// Property `name` of `object` as a string, `None` when undefined.
    fn property_text(&mut self, object: u32, name: &str) -> GuestResult<Option<String>> {
        let value = self.exports.get_prop(&mut self.store, object, name)?;
        let text = if self.exports.is_undefined.call(&mut self.store, value)? != 0 {
            Ok(None)
        } else {
            self.exports
                .string(&mut self.store, self.values.scratch, value)
                .map(Some)
        };
        self.free(value)?;
        text
    }

    fn free(&mut self, value: u32) -> GuestResult<()> {
        Ok(self.exports.free_value.call(&mut self.store, value)?)
    }
}
