# Bake development

This workspace builds `bake`, Bake's terminal coding agent. Run Cargo from the repository root so the toolchain pinned in `rust-toolchain.toml` applies. The product ships as one executable with embedded user documentation and needs no Node or Bun at runtime. Bake releases Linux x86_64 and Windows x64; macOS support follows later ([direction](direction.md)).

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

Bake reads `BAKE_HOME`, defaulting to `~/.bake`. `bake docs models` describes CLIProxyAPI sign-in; `bake docs settings` describes settings. Interactive and print modes share the same agent session, tools, model adapters, and Pi-format session storage. `bake preview` uses a fixture runtime with the same view components. The executable's [documentation](../crates/bake-cli/docs/) owns current commands and terminal behavior.

## Crates

Each row says what a crate owns. The crate's own module documentation describes its API; keep the row to one or two sentences.

| Crate | Responsibility |
|---|---|
| [`bake-agent`](../crates/bake-agent/) | The agent loop and stateful agent, ported from Pi's `agent` package |
| [`bake-ai`](../crates/bake-ai/) | Provider-neutral model streaming, ported from Pi's `ai` package |
| [`bake-cli`](../crates/bake-cli/) | The `bake` executable: launch options, embedded docs, print and RPC modes, and update commands |
| [`bake-codemode`](../crates/bake-codemode/) | Pi's codemode engine: the pinned QuickJS-NG WebAssembly binary run in process by Wasmtime, its prelude, declarations, and tool-search ranking; no agent types |
| [`bake-coding-agent`](../crates/bake-coding-agent/) | Pi's coding-agent services for Bake: sessions, compaction, models and the CLIProxyAPI route, prompts and skills, the tools (files, bash, web, `visualize`, `ask_user`, codemode), the permission layer over `bake-sandbox`, MCP servers, plugins, hooks, opt-in subagents, reminders, telecom, and spawned agents |
| [`bake-mcp`](../crates/bake-mcp/) | The Model Context Protocol client, ported from Pi's `mcp` package, over stdio and Streamable HTTP |
| [`bake-mermaid`](../crates/bake-mermaid/) | A bounded, total Mermaid-to-text renderer adapted from OpenAI Codex's `codex-mermaid` (Apache-2.0) |
| [`bake-plugin-sdk`](../crates/bake-plugin-sdk/) | Guest-side bindings for the `bake:plugin@0.1.0` WIT contract, for components built for `wasm32-wasip2` |
| [`bake-process`](../crates/bake-process/) | The owner of a spawned child and its whole process tree: a Unix process group or a Windows Job, and kill or detach settled to observed quiescence |
| [`bake-release`](../crates/bake-release/) | The release tool, never shipped: prepare, preflight, pack, license bundle, glibc floor, install rehearsal, manifest signing, publish, and the live check |
| [`bake-sandbox`](../crates/bake-sandbox/) | Process confinement: sandbox modes, per-call policies, escalation outcomes, and the backends behind the provider seam |
| [`bake-svg`](../crates/bake-svg/) | Bounded SVG on resvg for `visualize`: size and element limits, no linked resources, text and shape reports, and rendering |
| [`bake-tui`](../crates/bake-tui/) | Terminal ownership and effects: input, clipboard, title and notifications, images, the event loop, and live and fixture runtimes |
| [`bake-tui-view`](../crates/bake-tui-view/) | Pure editor and presentation state: key bindings, the transcript and its tool cards, dialogs, the status line, `visualize` drawings, and layout |
| [`bake-update`](../crates/bake-update/) | Signed release verification, bounded downloads, installation locks, atomic updates, and rollback |

Dependencies are pinned exactly in `Cargo.toml`, with comments stating their purpose, license, and whether they ship. Keep test-only crates in dev-dependencies. Crate NOTICE files keep Pi's MIT attribution and source revisions, and `bake-mermaid`'s the Apache-2.0 license and NOTICE of the Codex source it adapts.

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

Cargo runs unit, integration, and documentation tests. Each crate's tests cover its own contract: the agent loop and cancellation, streaming adapters against fake model servers, session recovery and durability, the sandbox and approval boundaries, process ownership, terminal restoration and expected-output rendering under truecolor, ANSI, `NO_COLOR`, and the classic frame, the tools' limits and results, MCP, telecom and spawned agents against scripted stand-ins, plugin isolation, and signed updates. Property tests exercise every parser and text transform of untrusted input. Fixtures keep their Pi source revision, and fake servers keep behavioral checks keyless. `bake-release`'s tests hold every copy of the release key equal and run the shipped `install.sh` against a loopback host.

For additional logic checks, install cargo-mutants, cargo-llvm-cov, and cargo-fuzz. Run `cargo mutants --package <crate>` on changed logic and `cargo llvm-cov --workspace --html` for coverage. Fuzz targets live under `fuzz/` on their separately pinned nightly: run `cargo fuzz list` or `cargo fuzz run <target> -- -max_total_time=120` from that directory. See [testing](testing.md) and [contributing](../CONTRIBUTING.md).
