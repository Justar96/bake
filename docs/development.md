# Bake development

This workspace builds `bake`, Bake's terminal coding agent. Run Cargo from the repository root so the toolchain pinned in `rust-toolchain.toml` applies. The product ships as one executable with embedded user documentation and needs no Node or Bun at runtime. Bake 0.4.0 releases Linux x86_64 and arm64; macOS and Windows support follows in 0.4.x.

## Run

From the repository root, build and run with Cargo:

```sh
cargo run --locked -p bake-cli --
cargo run --locked -p bake-cli -- --inline -c
cargo run --locked -p bake-cli -- preview
cargo run --locked -p bake-cli -- -p "explain this repository"
cargo run --locked -p bake-cli -- --help
cargo run --locked -p bake-cli -- docs
cargo run --locked -p bake-cli -- --self-check
```

Bake reads `BAKE_HOME`, then `DSH_HOME`, defaulting to `~/.bake`. `bake docs models` describes CLIProxyAPI sign-in; `bake docs settings` describes settings. Interactive and print modes share the same agent session, tools, model adapters, and Pi-format session storage. `bake preview` uses a fixture runtime with the same view components. The executable's [documentation](../crates/bake-cli/docs/) owns current commands and terminal behavior.

## Crates

| Crate | Responsibility |
|---|---|
| [`bake-agent`](../crates/bake-agent/) | The agent loop and stateful agent ported from Pi's `agent` package |
| [`bake-ai`](../crates/bake-ai/) | Provider-neutral model streaming ported from Pi's `ai` package |
| [`bake-cli`](../crates/bake-cli/) | The `bake` executable, launch options, embedded docs, print/RPC modes, and update commands |
| [`bake-codemode`](../crates/bake-codemode/) | The native engine of Pi's codemode for Bake: the pinned QuickJS-NG WebAssembly binary run by Wasmtime in process, with Bake's own WASI shim and C API driver, Pi's script environment (prelude), source format, declarations, deterministic mode, and tool-search ranking; no agent types |
| [`bake-coding-agent`](../crates/bake-coding-agent/) | Pi's coding-agent services for Bake: sessions, the file tools (read, write, edit, ls, find, grep) with their output and file-mutation ordering, the bash tool, models, prompts, skills, print mode, the CLIProxyAPI route, the D24 permission layer over bake-sandbox, session-owned MCP servers over stdio and Streamable HTTP, and the web_search and web_fetch tools |
| [`bake-mcp`](../crates/bake-mcp/) | The Model Context Protocol client boundary ported from Pi's `mcp` package: initialization, tool listing and calls, cancellation, and awaited shutdown over an injected, bounded transport, owned stdio server processes, and Streamable HTTP |
| [`bake-plugin-sdk`](../crates/bake-plugin-sdk/) | Guest-side bindings for Bake plugins: the bake:plugin@0.1.0 WIT contract for Rust components built for wasm32-wasip2 |
| [`bake-process`](../crates/bake-process/) | The owner of a spawned child and its whole process tree, shared by Bake's coding tools and MCP stdio servers: admission before creation, pinned leader identity, a Unix process group or a Windows Job the child is born in, and kill or detach settled to observed quiescence |
| [`bake-release`](../crates/bake-release/) | The release tool, never shipped: tag preflight, the archive packer and its self-checks, the third-party license bundle, the glibc floor, the install rehearsal with the shipped installer, manifest signing, and the live check, each verified with `bake-update` |
| [`bake-sandbox`](../crates/bake-sandbox/) | Bake's process-confinement contract: sandbox modes, per-call policies, escalation vocabulary, and the provider seam its backends implement |
| [`bake-tui`](../crates/bake-tui/) | Terminal ownership, Kitty keyboard input, clipboard, the terminal title with Ghostty progress and notifications, Herdr lifecycle and sidebar-token reporting, event loop, and live/fixture runtime connections, including the live opening banner, its access tags, and the startup update notice |
| [`bake-tui-view`](../crates/bake-tui-view/) | Pure editor and presentation state, key bindings, transcript layout, and rendering |
| [`bake-update`](../crates/bake-update/) | Signed release verification, bounded downloads, installation locks, atomic updates, and rollback |

Dependencies are pinned exactly in `Cargo.toml`, with comments stating their purpose, license, and whether they ship. Keep test-only crates in dev-dependencies. Crate NOTICE files retain Pi's MIT attribution and source revisions.

## Checks

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
cargo build --release --locked -p bake-cli
cargo test --locked -p bake-agent --lib
```

Cargo runs unit, integration, and documentation tests. The tests cover agent steps and cancellation, streaming adapters, session recovery and durability, sandbox and approval boundaries, native process ownership, editor/view invariants, the opening banner and the startup update check's cache and teardown, terminal restoration, terminal title, progress, and notifications, Herdr lifecycle and token reporting, and plugin isolation. Properties exercise untrusted parsers and text transforms; crate fixtures retain port provenance, and fake model servers keep behavioral checks keyless. The updater owns its release public key and version-order fixtures inside `crates/bake-update/`; `bake-release`'s tests hold every copy of that key equal, run the shipped `install.sh` against a loopback host, and check its version order against the same fixtures.

For additional logic checks, install cargo-mutants, cargo-llvm-cov, and cargo-fuzz. Run `cargo mutants --package <crate>` on changed logic and `cargo llvm-cov --workspace --html` for coverage. Fuzz targets live under `fuzz/` on their separately pinned nightly: run `cargo fuzz list` or `cargo fuzz run <target> -- -max_total_time=120` from that directory. Coverage of confined helpers has instrumentation limits. See [testing](testing.md), [contributing](../CONTRIBUTING.md), and the [support register](roadmap/rust-0.4/scope-00/support.md) for platform requirements.
