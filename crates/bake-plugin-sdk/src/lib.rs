//! Guest-side bindings for Bake plugins.
//!
//! A Bake plugin is a Rust crate compiled to `wasm32-wasip2` as a WASI 0.2
//! component. It targets the `guest` world of [`wit/plugin.wit`], package
//! `bake:plugin@0.1.0`: it imports Bake's `host` interface and exports the
//! `plugin` interface. Bake runs it in Wasmtime inside its own helper
//! process, with an empty WASI context: no files, environment, arguments,
//! sockets, or terminal. Its only effects are its answers and the bounded
//! [`log`] lines.
//!
//! ```ignore
//! struct Guard;
//!
//! impl bake_plugin_sdk::Plugin for Guard {
//!     fn on_tool_call(call: bake_plugin_sdk::ToolCall) -> Result<bake_plugin_sdk::Decision, String> {
//!         Ok(bake_plugin_sdk::Decision::None)
//!     }
//! }
//!
//! #[cfg(target_family = "wasm")]
//! bake_plugin_sdk::export_plugin!(Guard);
//! ```
//!
//! Build with `cargo build --target wasm32-wasip2` and a `cdylib` crate
//! type; the toolchain's component linker writes the component. The
//! plugin's `plugin.json` manifest names the API version this crate
//! implements ([`API_VERSION`]) and the tools whose calls it vetoes.
//!
//! The crate only supports the guest: Bake's host generates its own
//! bindings from the same WIT file. On other targets it compiles, but the
//! imports cannot be called, and a `cdylib` must not export: the export
//! names are not valid native linker symbols, so gate `export_plugin!` on
//! `target_family = "wasm"`.
//!
//! [`wit/plugin.wit`]: https://github.com/Justar96/bake/blob/develop/crates/bake-plugin-sdk/wit/plugin.wit

#![allow(missing_docs, clippy::all, unsafe_op_in_unsafe_fn)]

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    pub_export_macro: true,
    export_macro_name: "export_plugin",
    default_bindings_module: "bake_plugin_sdk",
});

pub use bake::plugin::host::{LogLevel, log};
pub use exports::bake::plugin::plugin::{BlockInfo, Decision, Guest as Plugin, ToolCall};

/// The `bake:plugin` package version these bindings implement.
pub const API_VERSION: &str = "0.1.0";
