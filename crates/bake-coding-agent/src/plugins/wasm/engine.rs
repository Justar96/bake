//! The Wasmtime side of the helper: the engine configuration, the host
//! bindings of `bake:plugin@0.1.0`, the per-store limits, and one guest
//! call. Only the helper process uses this module.
//!
//! Each store gets an empty WASI context, Bake's `log` import, an aggregate
//! [`ResourceLimiter`], CPU fuel with asynchronous yields, a copy budget
//! (`Store::set_hostcall_fuel`) charged before the host allocates anything
//! the guest passes or returns, a handle cap, and metered random
//! generators. A trap or a missed deadline discards the store, since a
//! trapped instance cannot be entered again; the next call instantiates a
//! fresh one from the compiled component.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::protocol::{CallRequest, LogLevel, LogLine, Outcome, TrapKind};
use crate::plugins::MAX_REASON_BYTES;

mod bindings {
    wasmtime::component::bindgen!({
        path: "../bake-plugin-sdk/wit",
        world: "guest",
        exports: { default: async },
    });
}

use bindings::exports::bake::plugin::plugin::{Decision, ToolCall};
use bindings::{Guest, GuestPre};

/// A compiled component, checked against the linker.
pub(crate) type Prepared = GuestPre<HostState>;

/// Aggregate linear memory per store, summed over every memory.
pub(crate) const MAX_LINEAR_MEMORY: usize = 64 << 20;
/// Aggregate table elements per store, summed over every table.
pub(crate) const MAX_TABLE_ELEMENTS: usize = 10_000;
/// Core instances, memories, and tables per store. The Rust guest needs 3,
/// 1, and 2.
const MAX_INSTANCES: usize = 8;
const MAX_MEMORIES: usize = 2;
const MAX_TABLES: usize = 4;
/// Live resource handles per store (WASI streams, pollables).
const MAX_HANDLES: usize = 1024;
/// Bytes one guest-to-host transfer may copy: one host-call argument list
/// or one return value. Above the 16 KiB reason cap, so an oversized
/// reason up to this size is lifted and reported as an invalid return.
pub(crate) const HOSTCALL_FUEL: usize = 128 * 1024;
/// Fuel between cooperative yields to the helper's executor, so its
/// deadline timer fires during a spinning guest.
const YIELD_INTERVAL: u64 = 10_000_000;
/// Captured guest stdout and stderr per store, each; later writes fail.
const OUTPUT_CAPTURE: usize = 64 * 1024;
/// Random bytes per call and per store, across both generators.
const RANDOM_PER_CALL: u64 = 64 * 1024;
const RANDOM_PER_STORE: u64 = 1 << 20;
/// The longest kept log line, and the most kept per call, in bytes.
pub(crate) const MAX_LOG_LINE: usize = 4 * 1024;
pub(crate) const MAX_LOGS_PER_CALL: usize = 64 * 1024;
/// The longest guest error or trap text a reply carries.
const MAX_FAILURE_TEXT: usize = 4 * 1024;

/// The helper's engine: Cranelift on one thread, fuel instrumentation, and
/// linear-memory reservations sized for a small address-space budget.
/// Wasmtime's default 4 GiB reservation per memory cannot fit one.
pub(crate) fn engine() -> wasmtime::Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    // Trap texts stay short: no Wasm backtrace is attached.
    config.wasm_backtrace_max_frames(None);
    config.memory_reservation(64 << 20);
    config.memory_guard_size(64 << 10);
    config.memory_reservation_for_growth(16 << 20);
    config.guard_before_linear_memory(false);
    // Copy-on-write images would back initial memory with a memfd, which
    // the helper's zero file-size limit forbids.
    config.memory_init_cow(false);
    // Unix signal handling on macOS, as `bake-codemode`'s engine uses:
    // Wasmtime requires every engine in a process to agree, and its Mach-port
    // thread aborts the process when a signal interrupts it.
    config.macos_use_mach_ports(false);
    Engine::new(&config)
}

/// The linker: WASI 0.2 over each store's empty context, and Bake's host
/// interface.
pub(crate) fn linker(engine: &Engine) -> wasmtime::Result<Linker<HostState>> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    Guest::add_to_linker::<_, HasSelf<HostState>>(&mut linker, |state| state)?;
    Ok(linker)
}

/// Compiles `component` and checks it against the linker; the error says
/// which step failed.
pub(crate) fn prepare(
    engine: &Engine,
    linker: &Linker<HostState>,
    component: &[u8],
) -> Result<Prepared, (super::protocol::LoadFailure, String)> {
    use super::protocol::LoadFailure;
    let component = Component::new(engine, component)
        .map_err(|error| (LoadFailure::Compile, format!("{error:#}")))?;
    let pre = linker
        .instantiate_pre(&component)
        .and_then(GuestPre::new)
        .map_err(|error| (LoadFailure::Link, format!("{error:#}")))?;
    Ok(pre)
}

/// One store's limits: aggregate memory and table totals, and the counts.
/// A refused growth traps, recording which limit it hit.
struct Limiter {
    memory: usize,
    elements: usize,
    exceeded: Option<TrapKind>,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        match self.memory.checked_add(desired.saturating_sub(current)) {
            Some(total) if total <= MAX_LINEAR_MEMORY => {
                self.memory = total;
                Ok(true)
            }
            _ => {
                self.exceeded = Some(TrapKind::Memory);
                Err(wasmtime::Error::msg(format!(
                    "growing a memory from {current} to {desired} bytes would bring the plugin's linear memory over {} MiB ({} bytes in use)",
                    MAX_LINEAR_MEMORY >> 20,
                    self.memory
                )))
            }
        }
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        match self.elements.checked_add(desired.saturating_sub(current)) {
            Some(total) if total <= MAX_TABLE_ELEMENTS => {
                self.elements = total;
                Ok(true)
            }
            _ => {
                self.exceeded = Some(TrapKind::Table);
                Err(wasmtime::Error::msg(format!(
                    "growing a table from {current} to {desired} elements would bring the plugin's tables over {MAX_TABLE_ELEMENTS} ({} in use)",
                    self.elements
                )))
            }
        }
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn memories(&self) -> usize {
        MAX_MEMORIES
    }

    fn tables(&self) -> usize {
        MAX_TABLES
    }
}

/// Random bytes drawn through both WASI generators.
#[derive(Default)]
struct RandomMeter {
    call: AtomicU64,
    store: AtomicU64,
    exceeded: AtomicBool,
}

/// A WASI random generator over the OS source that charges every byte to
/// the store's [`RandomMeter`]. Past a quota it still fills the buffer (the
/// interface cannot fail) but marks the call failed.
struct MeteredRandom(Arc<RandomMeter>);

impl rand_core::TryRng for MeteredRandom {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut bytes = [0u8; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut bytes = [0u8; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        let len = dst.len() as u64;
        let call = self
            .0
            .call
            .fetch_add(len, Ordering::Relaxed)
            .saturating_add(len);
        let store = self
            .0
            .store
            .fetch_add(len, Ordering::Relaxed)
            .saturating_add(len);
        if call > RANDOM_PER_CALL || store > RANDOM_PER_STORE || getrandom::fill(dst).is_err() {
            dst.fill(0);
            self.0.exceeded.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// The log lines one call staged, within its quota.
#[derive(Default)]
struct LogStage {
    lines: Vec<LogLine>,
    bytes: usize,
    dropped: u32,
}

/// One store's host state.
pub(crate) struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    limiter: Limiter,
    random: Arc<RandomMeter>,
    logs: LogStage,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl bindings::bake::plugin::host::Host for HostState {
    fn log(&mut self, level: bindings::bake::plugin::host::LogLevel, message: String) {
        use bindings::bake::plugin::host::LogLevel as Wit;
        let text = crate::plugins::truncate(&message, MAX_LOG_LINE);
        if self.logs.bytes + text.len() > MAX_LOGS_PER_CALL {
            self.logs.dropped = self.logs.dropped.saturating_add(1);
            return;
        }
        self.logs.bytes += text.len();
        self.logs.lines.push(LogLine {
            level: match level {
                Wit::Debug => LogLevel::Debug,
                Wit::Info => LogLevel::Info,
                Wit::Warn => LogLevel::Warn,
                Wit::Error => LogLevel::Error,
            },
            text: text.to_owned(),
        });
    }
}

/// A store and the instance in it.
struct Live {
    store: Store<HostState>,
    guest: Guest,
}

/// The compiled component and its current instance.
pub(crate) struct Plugin {
    pre: Prepared,
    live: Option<Live>,
    instance: u32,
    fuel: u64,
    deadline: Duration,
}

impl Plugin {
    pub(crate) fn new(pre: Prepared, fuel: u64, deadline: Duration) -> Self {
        Self {
            pre,
            live: None,
            instance: 0,
            fuel,
            deadline,
        }
    }

    fn store(&self) -> wasmtime::Result<Store<HostState>> {
        let random = Arc::new(RandomMeter::default());
        let wasi = WasiCtxBuilder::new()
            .stdout(MemoryOutputPipe::new(OUTPUT_CAPTURE))
            .stderr(MemoryOutputPipe::new(OUTPUT_CAPTURE))
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .max_random_size(RANDOM_PER_CALL)
            .secure_random(MeteredRandom(Arc::clone(&random)))
            .insecure_random(MeteredRandom(Arc::clone(&random)))
            .build();
        let mut table = ResourceTable::new();
        table.set_max_capacity(MAX_HANDLES);
        let mut store = Store::new(
            self.pre.engine(),
            HostState {
                wasi,
                table,
                limiter: Limiter {
                    memory: 0,
                    elements: 0,
                    exceeded: None,
                },
                random,
                logs: LogStage::default(),
            },
        );
        // The counts are read when the limiter is installed.
        store.limiter(|state| &mut state.limiter);
        store.set_hostcall_fuel(HOSTCALL_FUEL);
        store.set_max_component_handles(MAX_HANDLES);
        store.set_fuel(self.fuel)?;
        store.fuel_async_yield_interval(Some(YIELD_INTERVAL))?;
        Ok(store)
    }

    /// Runs one call within the helper's deadline and returns the instance
    /// that answered and the outcome, with the call's staged log lines.
    pub(crate) async fn call(
        &mut self,
        request: &CallRequest,
    ) -> (u32, Outcome, Vec<LogLine>, u32) {
        let deadline = self.deadline;
        let result = tokio::time::timeout(deadline, self.call_inner(request)).await;
        let instance = self.instance;
        let (outcome, logs, dropped) = match result {
            Ok(done) => done,
            Err(_elapsed) => {
                // Dropping the call disposed of its fiber and every host
                // future it awaited; the instance cannot be entered again.
                let staged = self.live.take().map(|live| live.store.into_data().logs);
                (
                    Outcome::Trap(
                        TrapKind::Deadline,
                        format!("the call missed its {} ms deadline", deadline.as_millis()),
                    ),
                    Vec::new(),
                    staged.map_or(0, |logs| {
                        logs.dropped.saturating_add(logs.lines.len() as u32)
                    }),
                )
            }
        };
        (instance, outcome, logs, dropped)
    }

    async fn call_inner(&mut self, request: &CallRequest) -> (Outcome, Vec<LogLine>, u32) {
        if self.live.is_none() {
            let mut store = match self.store() {
                Ok(store) => store,
                Err(error) => return (trap(TrapKind::Other, &error), Vec::new(), 0),
            };
            self.instance = self.instance.saturating_add(1);
            match self.pre.instantiate_async(&mut store).await {
                Ok(guest) => self.live = Some(Live { store, guest }),
                Err(error) => {
                    let kind = classify(store.data(), &error);
                    return (trap(kind, &error), Vec::new(), 0);
                }
            }
        }
        let Some(live) = self.live.as_mut() else {
            return (
                Outcome::Trap(TrapKind::Other, "no instance".to_owned()),
                Vec::new(),
                0,
            );
        };
        // Fresh per-call budgets: fuel, random, and the log stage.
        if let Err(error) = live.store.set_fuel(self.fuel) {
            return (trap(TrapKind::Other, &error), Vec::new(), 0);
        }
        live.store.data().random.call.store(0, Ordering::Relaxed);
        live.store.data_mut().logs = LogStage::default();

        let call = ToolCall {
            id: request.tool_call_id.clone(),
            name: request.tool_name.clone(),
            args_json: request.args_json.clone(),
        };
        let result = live
            .guest
            .bake_plugin_plugin()
            .call_on_tool_call(&mut live.store, &call)
            .await;
        let logs = std::mem::take(&mut live.store.data_mut().logs);
        let random_exceeded = live.store.data().random.exceeded.load(Ordering::Relaxed);
        let outcome = match result {
            Err(error) => trap(classify(live.store.data(), &error), &error),
            Ok(_) if random_exceeded => Outcome::Trap(
                TrapKind::Random,
                format!(
                    "the plugin drew more random bytes than its quota ({RANDOM_PER_CALL} a call, {RANDOM_PER_STORE} an instance)"
                ),
            ),
            Ok(Ok(Decision::None)) => Outcome::Continue,
            Ok(Ok(Decision::Block(block))) if block.reason.len() > MAX_REASON_BYTES => {
                Outcome::Trap(
                    TrapKind::InvalidReturn,
                    format!(
                        "the block reason is {} bytes, over the {MAX_REASON_BYTES}-byte limit",
                        block.reason.len()
                    ),
                )
            }
            Ok(Ok(Decision::Block(block))) => Outcome::Block {
                reason: block.reason,
                terminate: block.terminate,
            },
            Ok(Err(message)) => {
                Outcome::GuestError(crate::plugins::truncate(&message, MAX_FAILURE_TEXT).to_owned())
            }
        };
        if matches!(outcome, Outcome::Trap(..)) {
            self.live = None;
        }
        (outcome, logs.lines, logs.dropped)
    }
}

fn trap(kind: TrapKind, error: &wasmtime::Error) -> Outcome {
    let text = format!("{error:#}");
    Outcome::Trap(
        kind,
        crate::plugins::truncate(&text, MAX_FAILURE_TEXT).to_owned(),
    )
}

/// Which limit, if any, a trap came from.
fn classify(state: &HostState, error: &wasmtime::Error) -> TrapKind {
    if let Some(kind) = state.limiter.exceeded {
        return kind;
    }
    if error.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
        return TrapKind::OutOfFuel;
    }
    let text = format!("{error:#}");
    // Wasmtime's copy-budget error type is private; its text is pinned by
    // the hostile-return tests.
    if text.contains("fuel allocated for hostcalls has been exhausted") {
        TrapKind::Copy
    } else if text.contains("count too high") {
        TrapKind::Count
    } else {
        TrapKind::Other
    }
}
