//! The engine: the vendored `quickjs.wasm`, compiled once per process by
//! Cranelift with its own Wasmtime configuration, its import and export
//! signatures checked, and linked against the host's twelve imports.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use wasmtime::{Config, Engine, ExternType, InstancePre, Linker, Module};

use crate::host::{HostState, add_imports};
use crate::run::{Run, start};
use crate::types::{EngineError, ExecuteRequest, StartError};

/// `quickjs-wasi` 3.6.2's `quickjs.wasm`: QuickJS-NG compiled to a WASI
/// preview 1 reactor. See `engine/PROVENANCE`.
pub const QUICKJS_WASM: &[u8] = include_bytes!("../engine/quickjs.wasm");
/// The SHA-256 of [`QUICKJS_WASM`], as `engine/PROVENANCE` records it.
pub const QUICKJS_WASM_SHA256: &str =
    "d4c9375f2b1ca4dc95f72c8aa2982a7a9951ac8011490d79c6582df732b4bbd9";
/// The `quickjs-wasi` release the binary comes from.
pub const QUICKJS_WASI_VERSION: &str = "3.6.2";
/// Wasmtime's limit on the Wasm stack, below the script thread's 40 MiB.
/// QuickJS's stack guard measures only the C stack in linear memory, and a
/// built-in such as `JSON.parse` or `JSON.stringify` of deeply nested data
/// uses far more native stack per byte of it: 8 MiB still traps on nested
/// arrays at the guard's depth, 12 MiB does not; 32 MiB leaves a margin.
const MAX_WASM_STACK: usize = 32 << 20;

/// The module's imports, exactly: module, name, and signature.
const EXPECTED_IMPORTS: [(&str, &str, &str); 12] = [
    ("env", "host_get_timezone_offset", "i32 i32 -> i32"),
    ("env", "host_interrupt", " -> i32"),
    ("env", "host_promise_rejection", "i32 i32 i32 -> "),
    ("env", "host_module_normalize", "i32 i32 -> i32"),
    ("env", "host_module_load", "i32 i32 -> i32"),
    ("env", "host_call", "i32 i32 i32 i32 i32 -> i32"),
    (
        "wasi_snapshot_preview1",
        "clock_time_get",
        "i32 i64 i32 -> i32",
    ),
    ("wasi_snapshot_preview1", "fd_close", "i32 -> i32"),
    ("wasi_snapshot_preview1", "fd_fdstat_get", "i32 i32 -> i32"),
    (
        "wasi_snapshot_preview1",
        "fd_seek",
        "i32 i64 i32 i32 -> i32",
    ),
    (
        "wasi_snapshot_preview1",
        "fd_write",
        "i32 i32 i32 i32 -> i32",
    ),
    ("wasi_snapshot_preview1", "random_get", "i32 i32 -> i32"),
];

/// The exports the driver calls (`guest::GuestExports`) and their signatures.
const EXPECTED_EXPORTS: [(&str, &str); 30] = [
    ("_initialize", " -> "),
    ("qjs_init", " -> i32"),
    ("qjs_init2", "i32 -> i32"),
    ("qjs_get_quickjs_version", " -> i32"),
    ("qjs_set_memory_limit", "i32 -> "),
    ("qjs_set_max_stack_size", "i32 -> "),
    ("qjs_set_interrupt_handler", "i32 -> "),
    ("qjs_eval", "i32 i32 i32 i32 -> i32"),
    ("qjs_new_string", "i32 i32 -> i32"),
    ("qjs_new_number", "f64 -> i32"),
    ("qjs_get_undefined", " -> i32"),
    ("qjs_get_true", " -> i32"),
    ("qjs_get_false", " -> i32"),
    ("qjs_get_float64", "i32 -> f64"),
    ("qjs_get_string_len", "i32 i32 -> i32"),
    ("qjs_free_cstring", "i32 -> "),
    ("qjs_is_exception", "i32 -> i32"),
    ("qjs_is_undefined", "i32 -> i32"),
    ("qjs_is_string", "i32 -> i32"),
    ("qjs_get_bool", "i32 -> i32"),
    ("qjs_dup_value", "i32 -> i32"),
    ("qjs_free_value", "i32 -> "),
    ("qjs_get_prop_string", "i32 i32 -> i32"),
    ("qjs_call", "i32 i32 i32 i32 -> i32"),
    ("qjs_new_host_function", "i32 i32 i32 -> i32"),
    ("qjs_is_job_pending", " -> i32"),
    ("qjs_execute_pending_job", " -> i32"),
    ("qjs_get_exception", " -> i32"),
    ("wasm_malloc", "i32 -> i32"),
    ("wasm_free", "i32 -> "),
];

/// The compiled engine, shared by every run in the process.
pub struct CodemodeEngine {
    engine: Engine,
    pre: InstancePre<HostState>,
    compile_time: Duration,
}

impl std::fmt::Debug for CodemodeEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodemodeEngine")
            .field("compile_time", &self.compile_time)
            .finish_non_exhaustive()
    }
}

static ENGINE: OnceLock<Result<CodemodeEngine, EngineError>> = OnceLock::new();

impl CodemodeEngine {
    /// The process's engine, compiling it on first use (about half a second
    /// in a release build). The first call blocks the calling thread while
    /// Cranelift compiles, as does any call made meanwhile: on an async
    /// runtime, make it from a blocking task such as
    /// `tokio::task::spawn_blocking`.
    ///
    /// # Errors
    ///
    /// [`EngineError`] when Wasmtime cannot compile the module or its ABI
    /// differs from the one the driver expects; every later call returns the
    /// same error.
    pub fn get() -> Result<&'static Self, EngineError> {
        ENGINE
            .get_or_init(Self::compile)
            .as_ref()
            .map_err(Clone::clone)
    }

    fn compile() -> Result<Self, EngineError> {
        let started = Instant::now();
        let mut config = Config::new();
        config.epoch_interruption(true);
        config.max_wasm_stack(MAX_WASM_STACK);
        // Wasmtime's `async` feature is on in the workspace (for the plugin
        // host); its fiber stacks must hold the Wasm stack limit, though this
        // engine only runs synchronously.
        config.async_stack_size(MAX_WASM_STACK + (1 << 20));
        // Trap texts stay short: no Wasm backtrace is attached.
        config.wasm_backtrace_max_frames(None);
        let engine = Engine::new(&config).map_err(|error| EngineError(format!("{error:#}")))?;
        let module = Module::new(&engine, QUICKJS_WASM)
            .map_err(|error| EngineError(format!("{error:#}")))?;
        check_abi(&module)?;
        let mut linker = Linker::new(&engine);
        add_imports(&mut linker).map_err(|error| EngineError(format!("{error:#}")))?;
        let pre = linker
            .instantiate_pre(&module)
            .map_err(|error| EngineError(format!("{error:#}")))?;
        Ok(Self {
            engine,
            pre,
            compile_time: started.elapsed(),
        })
    }

    /// How long compiling the module took.
    pub fn compile_time(&self) -> Duration {
        self.compile_time
    }

    /// Starts `request`'s script on its own thread; the returned [`Run`] is
    /// the only handle to it.
    ///
    /// # Errors
    ///
    /// [`StartError`] for an invalid request or a thread that could not
    /// start; the request's exit hook has been called.
    pub fn start(&self, request: ExecuteRequest) -> Result<Run, StartError> {
        start(self, request)
    }

    pub(crate) fn wasmtime_engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) fn instance_pre(&self) -> &InstancePre<HostState> {
        &self.pre
    }
}

fn signature(ty: &ExternType) -> Option<String> {
    let ExternType::Func(func) = ty else {
        return None;
    };
    let params: Vec<String> = func.params().map(|param| param.to_string()).collect();
    let results: Vec<String> = func.results().map(|result| result.to_string()).collect();
    Some(format!("{} -> {}", params.join(" "), results.join(" ")))
}

/// Refuses a module whose imports are not exactly the twelve the host
/// implements, or whose exports the driver calls have other signatures.
fn check_abi(module: &Module) -> Result<(), EngineError> {
    let mut imports: Vec<(String, String, Option<String>)> = module
        .imports()
        .map(|import| {
            (
                import.module().to_owned(),
                import.name().to_owned(),
                signature(&import.ty()),
            )
        })
        .collect();
    let mut expected: Vec<(String, String, Option<String>)> = EXPECTED_IMPORTS
        .iter()
        .map(|(module, name, ty)| {
            (
                (*module).to_owned(),
                (*name).to_owned(),
                Some((*ty).to_owned()),
            )
        })
        .collect();
    imports.sort();
    expected.sort();
    if imports != expected {
        return Err(EngineError(format!(
            "the module's imports differ from the host's: {imports:?}"
        )));
    }
    for (name, expected) in EXPECTED_EXPORTS {
        let found = module.get_export(name).as_ref().and_then(signature);
        if found.as_deref() != Some(expected) {
            return Err(EngineError(format!(
                "the module's export {name} is {found:?}, not a function {expected}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// SHA-256 (FIPS 180-4), enough to check the embedded binary.
    pub(crate) fn sha256(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut state: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut message = data.to_vec();
        let bits = (data.len() as u64).wrapping_mul(8);
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&bits.to_be_bytes());
        for block in message.as_chunks::<64>().0 {
            let mut w = [0u32; 64];
            for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
                w[index] = u32::from_be_bytes(*word);
            }
            for index in 16..64 {
                let s0 = w[index - 15].rotate_right(7)
                    ^ w[index - 15].rotate_right(18)
                    ^ (w[index - 15] >> 3);
                let s1 = w[index - 2].rotate_right(17)
                    ^ w[index - 2].rotate_right(19)
                    ^ (w[index - 2] >> 10);
                w[index] = w[index - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[index - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
            for index in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let choice = (e & f) ^ (!e & g);
                let t1 = h
                    .wrapping_add(s1)
                    .wrapping_add(choice)
                    .wrapping_add(K[index])
                    .wrapping_add(w[index]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let majority = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(majority);
                h = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
                *slot = slot.wrapping_add(value);
            }
        }
        state.iter().map(|word| format!("{word:08x}")).collect()
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_embedded_engine_is_the_pinned_binary() {
        assert_eq!(QUICKJS_WASM.len(), 637_405);
        assert_eq!(sha256(QUICKJS_WASM), QUICKJS_WASM_SHA256);
        assert!(include_str!("../engine/PROVENANCE").contains(QUICKJS_WASM_SHA256));
    }

    #[test]
    fn the_module_has_exactly_the_expected_abi() {
        let engine = CodemodeEngine::get();
        assert!(engine.is_ok(), "{engine:?}");
    }
}
