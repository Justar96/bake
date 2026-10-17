# Windows configuration trust probe

This standalone research crate investigates owner, DACL, label, and path checks before Bake reads Windows MCP configuration or enables installed Wasm plugins. It is outside the shipping Cargo workspace and release archives. It does not enable either product path.

The port was paused on 2026-10-17 during the native qualification refactor. **The Windows build is incomplete:** `src/win/mod.rs` declares `mechanisms`, but `src/win/mechanisms.rs` has not been written. The workflow has not been installed or run. Workspace checks do not compile this separate crate.

## Continue the research

Finish the mechanism tests, verify cleanup ownership and failing exit status, then qualify the proposal on native Windows before changing product trust checks. Test permitted and refused owners and ACLs, ancestor replacement, reparse points, sharing violations, stable read bytes, and atomic updates. Keep existing Windows configuration reads and plugin enablement refused until that evidence is reviewed.

## Dev Note

The [design snapshot](research/design.md) explains the proposed handle-relative read sequence and primary Microsoft sources. The earlier [prototype](research/prototype/Cargo.toml) preserves the worker's complete initial implementation; it passed 12 pure Linux tests, seven evaluator mutation controls, and Windows cross-target Clippy before this pause. It has not run on Windows, and its error reporting and cleanup need the unfinished refactor. The [workflow sketch](research/ci-qualification.yml) is a proposal, not an active CI job. Do not infer native evidence from cross-compilation or from successful checks of the main workspace.
