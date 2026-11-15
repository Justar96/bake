---
name: wasm-plugins
description: "Build, test, and publish a WebAssembly plugin for our host application - the component model interface, the SDK crate, capabilities a plugin must request, size and startup budgets, and the hostile-input tests every plugin must pass before it is listed. Use when creating a new plugin, updating a plugin to a new interface version, debugging a plugin that traps or is refused by the host, or when asked about wit files, cargo component, wasm32-wasip2, fuel limits, or plugin capabilities. A plugin requests only the capabilities it uses, stays under 2 MB, starts in under 50 ms on the reference machine, and ships with its source revision and license recorded next to the built component."
---

# Wasm plugins

1. Start from `plugins/template`; declare capabilities in `plugin.toml`.
2. Build with `cargo component build --release`; check size and startup.
3. Run the hostile-input suite; a trap on bad input fails the plugin.
