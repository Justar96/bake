//! Third-party plugins as WebAssembly components, run in a helper process
//! Bake owns: the host boundary of the native plugin design
//! (`docs/roadmap/rust-0.4/native-plugins.md`).
//!
//! A plugin is a `plugin.json` manifest ([`WasmManifest`]) and a WASI 0.2
//! component built for `wasm32-wasip2` against `bake:plugin@0.1.0`
//! (`rust/crates/bake-plugin-sdk/wit/plugin.wit`), the only API version
//! this Bake supports ([`SUPPORTED_API_VERSIONS`]). That version has one
//! contribution, the tool-call veto, and one host import, `log`.
//!
//! [`WasmPluginHost`] runs one plugin generation in its own helper process:
//! `bake-rs` re-executed with [`HELPER_ARG`] ([`helper_main`]). The helper
//! receives the component's bytes, checks and compiles it with Wasmtime,
//! and answers calls over a bounded, versioned protocol; nothing compiled
//! crosses the pipe, and the parent never compiles or runs guest code.
//! The boundaries:
//!
//! - **Guest authority** is what Wasmtime links: WASI 0.2 over an empty
//!   context (no files, environment, arguments, stdin, sockets, or name
//!   lookup; clocks; metered random) and `log`. Imports outside that list,
//!   and other `bake:plugin` versions, are refused before compiling.
//! - **Guest resources** are bounded per store: linear memory and table
//!   elements summed over all memories and tables, instance, memory, table,
//!   and handle counts, CPU fuel per call with cooperative yields, a copy
//!   budget checked before the host allocates what the guest passes or
//!   returns, log and random quotas, and a deadline over the whole call.
//! - **The helper process** is a lifetime and memory boundary, not a
//!   filesystem or network sandbox: it runs as the user, with a scrubbed
//!   environment and no terminal. Before reading input it establishes a
//!   verified memory limit and refuses to run without one: on Linux and
//!   macOS a hard `RLIMIT_AS` it sets on itself (an address-space bound of
//!   its startup size plus the budget; on Linux the startup size is capped
//!   at [`MAX_LINUX_BASELINE`]), on Windows the committed-memory limit of
//!   the Job it was created in. Neither bounds total memory. Parent death
//!   closes its input, and it exits even mid-compile; on Windows the Job
//!   also kills it when the parent's handle closes.
//! - **The parent** owns the helper from its creation until it has been
//!   reaped and its pipes have closed, across dropped and bounded
//!   operations ([`WasmPluginHost::shutdown`] resumes an unfinished
//!   teardown); it bounds every wait and checks every reply, and it bounds
//!   a call's id, argument depth, and argument length before sending.
//!
//! A session runs the Wasm plugins its Bake home enables
//! ([`crate::plugins::installed`]) through a per-plugin adapter
//! (`session.rs`) that owns each helper generation it starts; the
//! `bake-rs plugin check` diagnostic runs one host directly.

mod engine;
mod helper;
mod host;
pub(crate) mod manifest;
mod os_limit;
mod process;
mod protocol;
pub(crate) mod session;
mod validate;

pub use helper::helper_main;
pub use host::{
    CallError, CheckedVeto, CompileQueue, ExitReport, HelperCommand, HostLimits, HostRequest,
    HostStatus, StartError, WasmPluginHost,
};
pub use manifest::{PackageError, WasmManifest, WasmPackage};
pub use protocol::{LimitMechanism, LoadFailure, LogLevel, LogLine, MemoryLimit, TrapKind};
pub use session::{HelperInfo, WasmSetup};

/// The hidden first argument that turns a Bake executable into the plugin
/// helper.
pub const HELPER_ARG: &str = "__bake-wasm-plugin-host";

/// The `bake:plugin` version new plugins target.
pub const API_VERSION: &str = "0.1.0";

/// Every `bake:plugin` version this Bake links, exactly.
pub const SUPPORTED_API_VERSIONS: &[&str] = &[API_VERSION];

/// The largest `plugin.json`, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The largest component any host accepts, in bytes; [`HostLimits`] sets
/// a host's own, smaller limit.
pub const MAX_COMPONENT_BYTES: usize = 16 << 20;

/// The largest tool-call arguments sent to a plugin, as JSON, in bytes.
pub const MAX_ARGS_BYTES: usize = 1 << 20;

/// The deepest nesting of arrays and objects in a call's arguments.
/// Checked without recursion before the arguments are serialized.
pub const MAX_ARGS_DEPTH: usize = 64;

/// The longest tool call id sent to a plugin, in bytes.
pub const MAX_TOOL_CALL_ID_BYTES: usize = 1024;

/// On Linux, the largest address space the helper may have when it sets
/// its limit, in bytes. The helper refuses to start above it and the
/// parent refuses a `Hello` reporting more, so the helper's absolute
/// `RLIMIT_AS` is at most this plus [`HostLimits::memory_budget`].
/// Measured baselines are about 62 MiB (release) on x86_64.
pub const MAX_LINUX_BASELINE: u64 = 256 << 20;

/// The log text one host keeps across all its calls, in bytes.
pub const MAX_SESSION_LOG_BYTES: usize = 1 << 20;

/// The build a helper must be: the protocol checks it in the `Hello`.
/// Builds of one version are not told apart, except that on Linux the
/// helper is `/proc/self/exe`, the very image the parent runs.
const BUILD: &str = concat!("bake-rs ", env!("CARGO_PKG_VERSION"), " wasmtime 49.0.3");
