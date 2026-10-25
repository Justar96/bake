//! The native engine of Pi's codemode for Bake.
//!
//! A script is model-written JavaScript, the body of an async function, whose
//! only capability is calling the tools and globals its host declares. This
//! crate runs it in the QuickJS-NG WebAssembly binary Pi v1.3.0 runs
//! (`quickjs-wasi` 3.6.2's `quickjs.wasm`, vendored under `engine/`), executed
//! by Wasmtime inside the calling process. Bake implements the module's
//! twelve imports and drives its C API itself; the script environment
//! (`tools`, `ALL_TOOLS`, `text`, `image`, `exit`, `console`, `store`, `load`)
//! is Pi's prelude, verbatim. Like Pi's `packages/codemode`, the crate has no
//! agent types: the host answers every call.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use bake_codemode::{CodemodeEngine, Event, ExecuteRequest, HostCall, Source, ToolDeclaration};
//!
//! let engine = CodemodeEngine::get()?;
//! let mut request = ExecuteRequest::new("");
//! request.source = Source::parse("return await tools.echo({ a: 1 })")?;
//! request.tools = vec![ToolDeclaration::named("echo")];
//! let mut run = engine.start(request)?;
//! loop {
//!     match run.next().await {
//!         Event::Call(HostCall::Tool { id, args, .. }) => run.answer(id, Ok(args))?,
//!         Event::Done(outcome) => break println!("{:?}", outcome.result),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`CodemodeEngine`] is compiled once per process. Each [`Run`] owns one OS
//! thread with a 40 MiB stack; dropping it aborts the script and joins the
//! thread. A script stops at its deadline or [`Run::abort`] through
//! Wasmtime's epoch interruption (so long built-ins stop too), QuickJS's
//! interrupt poll, and checks between promise job batches. QuickJS's heap is
//! limited to 256 MiB by default and the guest's linear memory to 320 MiB.
//! Deterministic mode makes a run replayable: see [`Deterministic`].
//!
//! [`Source`] parses the `// @options:` line, [`render_declarations`] renders
//! TypeScript declarations from JSON Schema, and [`Bm25Ranker`] ranks tools
//! for the host's `searchTools()` answers.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod declarations;
mod engine;
mod guest;
mod host;
mod identifier;
mod js;
mod prelude;
mod run;
mod source;
mod tool_search;
mod types;
mod vm;

pub use declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, ScriptFunction, render_declarations, render_tool_output_type,
    render_tool_sample, render_tool_signature, schema_to_type,
};
pub use engine::{CodemodeEngine, QUICKJS_WASI_VERSION, QUICKJS_WASM, QUICKJS_WASM_SHA256};
pub use identifier::to_codemode_identifier;
pub use prelude::{
    MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS,
};
pub use run::{Run, RunHandle, SCRIPT_THREAD_STACK_BYTES};
pub use source::{CODEMODE_OPTIONS_PREFIX, MAX_TIMEOUT_MS, Source, SourceError, SourceOptions};
pub use tool_search::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, ToolNamespace, ToolSearchDocument, ToolSearchMatch,
    create_tool_search_document, tokenize,
};
pub use types::{
    AnswerError, CallError, CallResult, Completion, DEFAULT_HEAP_BYTES,
    DEFAULT_LINEAR_MEMORY_BYTES, Deterministic, During, EngineError, Event, ExecuteRequest,
    ExitHook, Failure, GlobalDeclaration, HostCall, Limits, MAX_PENDING_ARGUMENT_BYTES,
    MAX_PENDING_CALLS, MAX_STACK_BYTES, Outcome, OutputItem, ScriptError, StartError, StoreValues,
    StoreWrites, ToolDeclaration, discovery_globals,
};
