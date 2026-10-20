//! Shared fixtures for the Wasm plugin host tests: the shipped `bake`
//! as the helper, the committed guest components, components built from
//! core-module text, synthetic slow-to-compile components, and process
//! observation.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use bake_coding_agent::plugins::ToolCallView;
use bake_coding_agent::plugins::wasm::{
    CompileQueue, HELPER_ARG, HelperCommand, HostLimits, HostRequest, HostStatus, StartError,
    WasmPackage, WasmPluginHost,
};
use serde_json::Value;
use tokio::sync::watch;

/// The shipped binary, which serves as the helper.
pub const BAKE: &str = env!("CARGO_BIN_EXE_bake");

/// The `rust/` directory.
pub fn rust_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The production helper command.
pub fn command() -> HelperCommand {
    HelperCommand::new(BAKE, [HELPER_ARG])
}

fn read(path: &str) -> Vec<u8> {
    let path = rust_dir().join(path);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The committed `guard` component and its manifest.
pub fn guard() -> WasmPackage {
    WasmPackage::read_dir(&rust_dir().join("plugins/guard")).expect("the guard package is valid")
}

/// Every tool name the hostile fixture answers.
pub const HOSTILE_TOOLS: [&str; 12] = [
    "write",
    "edit",
    "spin",
    "grow",
    "flood-output",
    "log-alias",
    "log-huge",
    "reason",
    "ambient",
    "sleep",
    "error",
    "panic",
];

/// `component` under a manifest for `tools`.
pub fn package(id: &str, tools: &[&str], component: Vec<u8>) -> WasmPackage {
    let manifest = serde_json::json!({
        "api": "0.1.0",
        "id": id,
        "version": "0.0.0",
        "vetoTools": tools,
    });
    WasmPackage::new(manifest.to_string().as_bytes(), component).expect("valid package")
}

/// The committed hostile component.
pub fn hostile() -> WasmPackage {
    package(
        "hostile",
        &HOSTILE_TOOLS,
        read("plugins/fixtures/hostile/hostile.wasm"),
    )
}

/// Test limits: a budget within the 512 MiB ceiling for hostile runs.
pub fn limits() -> HostLimits {
    HostLimits {
        memory_budget: 384 << 20,
        ..HostLimits::default()
    }
}

/// A start request for `package` under `limits` with its own queue.
pub async fn try_start(
    package: &WasmPackage,
    limits: &HostLimits,
) -> Result<WasmPluginHost, StartError> {
    try_start_with(&command(), package, limits, None).await
}

pub async fn try_start_with(
    command: &HelperCommand,
    package: &WasmPackage,
    limits: &HostLimits,
    status: Option<watch::Sender<HostStatus>>,
) -> Result<WasmPluginHost, StartError> {
    let host = new_host(command, package, limits, CompileQueue::new(4), status)?;
    match host.initialize(None).await {
        Ok(()) => Ok(host),
        Err(error) => {
            // The failed start tore the helper down; the host agrees, and a
            // later shutdown returns the same, complete report.
            let exit = host.shutdown().await;
            assert!(exit.is_complete(), "{exit:?} after {error:?}");
            if let Some(reported) = error.exit() {
                assert_eq!(reported, &exit, "{error:?}");
            }
            if let Some(pid) = host.pid() {
                assert!(process_gone(pid), "helper {pid} survived a failed start");
            }
            Err(error)
        }
    }
}

/// A host with no process yet.
pub fn new_host(
    command: &HelperCommand,
    package: &WasmPackage,
    limits: &HostLimits,
    queue: CompileQueue,
    status: Option<watch::Sender<HostStatus>>,
) -> Result<WasmPluginHost, StartError> {
    WasmPluginHost::new(HostRequest {
        command: command.clone(),
        package: package.clone(),
        limits: limits.clone(),
        queue,
        generation: 1,
        status,
    })
}

/// Polls, bounded, until `pid` has been reaped. Keyed to the process
/// state, not to time.
pub async fn until_gone(pid: u32) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !process_gone(pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the helper is reaped within 60 s");
}

pub async fn start(package: &WasmPackage, limits: &HostLimits) -> WasmPluginHost {
    try_start(package, limits).await.expect("the host starts")
}

/// A call view over `args`.
pub fn call<'a>(name: &'a str, args: &'a Value) -> ToolCallView<'a> {
    ToolCallView {
        tool_call_id: "call-1",
        tool_name: name,
        args,
    }
}

/// Waits, bounded, until `status` reaches a state `ready` accepts.
pub async fn wait_status(
    status: &mut watch::Receiver<HostStatus>,
    ready: impl Fn(HostStatus) -> bool,
) {
    tokio::time::timeout(
        Duration::from_secs(60),
        status.wait_for(|state| ready(*state)),
    )
    .await
    .expect("the status changes within 60 s")
    .expect("the host keeps its status sender");
}

/// Whether process `pid` no longer exists: on Unix, it exited and was
/// reaped. Windows has no unreaped state, so there it means only that the
/// process exited; it says nothing about whether its owner still holds a
/// handle to it (see [`ProcessHandle`]).
pub fn process_gone(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        // A zombie still has a /proc entry; a reaped process has none.
        !Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: a signal-0 probe sends nothing.
        let failed = unsafe { libc::kill(pid as libc::pid_t, 0) } == -1;
        failed && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: plain call; only ERROR_INVALID_PARAMETER proves the pid
        // no longer names a process. Access denial is not an observed exit.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_INVALID_PARAMETER as i32);
        }
        // The handle's signalled state, not `STILL_ACTIVE`, which a process
        // may also return as its exit code.
        // SAFETY: a live handle, closed below; a zero timeout only polls.
        let exited = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
        // SAFETY: the handle opened above.
        unsafe { CloseHandle(handle) };
        exited
    }
}

/// The test's own handle to a helper, to observe its exit independently of
/// the host that owns it.
///
/// Open it only while the owner still holds its own handle to the process:
/// Windows does not reuse a pid while any handle to its process is open, so
/// the pid then names the owner's helper, and this handle keeps naming it.
/// The process having exited (this handle signalled) is not the owner
/// having observed the exit or closed its handle: Windows has no unreaped
/// state, so ownership is shown by the host's own behaviour instead.
#[cfg(windows)]
pub struct ProcessHandle(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl ProcessHandle {
    /// Opens `pid`; the caller vouches that its owner still holds it.
    pub fn open(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain call; a non-null result is a fresh handle.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(
            !raw.is_null(),
            "open helper {pid}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the fresh handle above, owned from here on.
        Self(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) })
    }

    /// The exit code, once the process has exited.
    pub fn exit_code(&self) -> Option<u32> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        // SAFETY: a live handle owned by `self`; a zero timeout only polls.
        if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: a live handle and valid output space.
        let read = unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) };
        assert!(read != 0, "{}", std::io::Error::last_os_error());
        Some(code)
    }

    /// Waits, bounded, for the process to exit; its exit code. Keyed to
    /// the process state, not to time.
    pub async fn exited(&self) -> u32 {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Some(code) = self.exit_code() {
                    return code;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the helper exits within 60 s")
    }
}

/// The Linux scheduler state letter of `pid` (`R`, `S`, `Z`, ...).
#[cfg(target_os = "linux")]
pub fn linux_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?.1.chars().next()
}

/// A private directory under the system temporary directory, removed when
/// dropped. Created exclusively, named by the process id and a counter.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "bake-wasm-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create a private temp directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The plugin WIT, as the guest SDK ships it.
pub const WIT: &str = include_str!("../../../bake-plugin-sdk/wit/plugin.wit");

/// Componentizes a core module (text) for `world` in `wit` text.
pub fn componentize(core: &[u8], wit: &str, world: &str) -> Vec<u8> {
    let mut module = core.to_vec();
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve.push_str("plugin.wit", wit).expect("the WIT parses");
    let world = resolve
        .select_world(&[package], Some(world))
        .expect("the world exists");
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )
    .expect("metadata embeds");
    wit_component::ComponentEncoder::default()
        .module(&module)
        .expect("the module is accepted")
        .validate(true)
        .encode()
        .expect("the component encodes")
}

/// A core module implementing the plugin export: a bump allocator and an
/// `on-tool-call` that runs `body` (which may use the call's six flat
/// parameters, `args_json`'s length being local 5; the exported memory is
/// `$mem`) and returns `ok(none)`
/// from the zeroed result area at address 0. `extra` adds module fields.
pub fn plugin_module(body: &str, extra: &str, export: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
  {extra}
  (memory $mem (export "memory") 1)
  (table $t 1 funcref)
  (global $heap (mut i32) (i32.const 4096))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $p i32)
    global.get $heap
    i32.const 7
    i32.add
    i32.const -8
    i32.and
    local.tee $p
    local.get 3
    i32.add
    global.set $heap
    (block $ok
      (loop $more
        global.get $heap
        memory.size $mem
        i32.const 16
        i32.shl
        i32.le_u
        br_if $ok
        i32.const 1
        memory.grow $mem
        i32.const -1
        i32.eq
        if
          unreachable
        end
        br $more))
    local.get $p)
  (func (export "{export}") (param i32 i32 i32 i32 i32 i32) (result i32)
    {body}
    i32.const 0))"#
    );
    wat::parse_str(text).expect("the core module text parses")
}

/// The usual export name.
pub const ON_TOOL_CALL: &str = "bake:plugin/plugin@0.1.0#on-tool-call";

/// A plugin component from core-module text pieces, for the 0.1.0 world.
pub fn wat_plugin(body: &str, extra: &str) -> Vec<u8> {
    componentize(&plugin_module(body, extra, ON_TOOL_CALL), WIT, "guest")
}

/// A valid plugin component with `functions` unused functions of `ops`
/// arithmetic operations each, which Wasmtime compiles before it can run
/// anything: three to six bytes per operation.
pub fn slow_component(functions: u32, ops: u32) -> Vec<u8> {
    use wasm_encoder::{
        CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection,
        GlobalSection, GlobalType, MemorySection, MemoryType, Module, TypeSection, ValType,
    };
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32; 4], [ValType::I32]);
    types.ty().function([ValType::I32; 6], [ValType::I32]);
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut funcs = FunctionSection::new();
    funcs.function(0);
    funcs.function(1);
    for _ in 0..functions {
        funcs.function(2);
    }
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 16,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(4096),
    );
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("cabi_realloc", ExportKind::Func, 0);
    exports.export(ON_TOOL_CALL, ExportKind::Func, 1);
    let mut code = CodeSection::new();
    // A bump allocator within the 1 MiB initial memory.
    let mut realloc = Function::new([(1, ValType::I32)]);
    realloc
        .instructions()
        .global_get(0)
        .i32_const(7)
        .i32_add()
        .i32_const(-8)
        .i32_and()
        .local_tee(4)
        .local_get(3)
        .i32_add()
        .global_set(0)
        .local_get(4)
        .end();
    code.function(&realloc);
    let mut on_call = Function::new([]);
    on_call.instructions().i32_const(0).end();
    code.function(&on_call);
    for function in 0..functions {
        let mut filler = Function::new([]);
        let mut sink = filler.instructions();
        sink.local_get(0);
        for op in 0..ops {
            sink.i32_const((op ^ function) as i32);
            match op % 3 {
                0 => sink.i32_add(),
                1 => sink.i32_mul(),
                _ => sink.i32_xor(),
            };
        }
        sink.end();
        code.function(&filler);
    }
    let mut module = Module::new();
    module
        .section(&types)
        .section(&funcs)
        .section(&memories)
        .section(&globals)
        .section(&exports)
        .section(&code);
    componentize(&module.finish(), WIT, "guest")
}

/// A helper command whose pipes outlive the helper: `/bin/sh` starts a
/// background reader of a private FIFO, which inherits the helper's
/// standard output and error, then `exec`s the real helper. After the
/// helper exits and is reaped, its pipes stay open until the FIFO is
/// written, so a teardown waits in its drain step: a deterministic barrier
/// with the real helper. [`Holder`]'s drop releases it, so a failing test
/// leaves no process behind.
#[cfg(unix)]
pub fn held_command(dir: &TempDir) -> (HelperCommand, Holder) {
    let fifo = dir.path().join("hold");
    let path = std::ffi::CString::new(fifo.to_str().expect("utf-8 path")).expect("no NUL");
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0, "mkfifo");
    let script = format!("(read line < '{}') & exec \"$0\" \"$@\"", fifo.display());
    (
        HelperCommand::new(
            "/bin/sh",
            [
                "-c".to_owned(),
                script,
                BAKE.to_owned(),
                HELPER_ARG.to_owned(),
            ],
        ),
        Holder(Some(fifo)),
    )
}

/// Releases a [`held_command`]'s pipes.
#[cfg(unix)]
pub struct Holder(Option<PathBuf>);

#[cfg(unix)]
impl Holder {
    /// Lets the background reader exit, closing the helper's pipes.
    pub fn release(&mut self) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(fifo) = self.0.take() {
            // Non-blocking: if the reader is gone already, opening fails
            // with ENXIO and there is nothing to release.
            if let Ok(mut writer) = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                let _ = writer.write_all(b"go\n");
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Holder {
    fn drop(&mut self) {
        self.release();
    }
}
