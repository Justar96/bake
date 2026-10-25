# Bake

Bake is an independent, MIT-licensed terminal coding agent. Its product is the `bake` executable built from the Cargo workspace at the repository root. User-facing text calls it Bake. The repository contains Rust, its embedded assets and test fixtures, documentation, and Cargo-based CI. Linux x86_64 and arm64 are released; macOS and Windows support follows in 0.4.x.

Use the current source, [development guide](docs/development.md), and [contribution guide](CONTRIBUTING.md) for implementation and commands. The [0.4 roadmap](docs/roadmap/rust-0.4/README.md) and [support register](docs/roadmap/rust-0.4/scope-00/support.md) retain design decisions and qualification history. Historical commands, deleted tooling, and linked Git snapshots do not define the current development workflow.

## Workspace

- `crates/`: agent, model adapters, tools, session services, sandbox, terminal, and executable.
- `plugins/`: Wasm plugin SDK examples and hostile fixtures.
- `fuzz/`: parser fuzz targets with a separate nightly toolchain and lockfile.
- `probes/`: standalone research crates; they are outside the main workspace checks.
- `crates/bake-cli/docs/`: user documentation embedded in `bake`.
- `docs/`: architecture, development guidance, and roadmap documentation.
- `.github/workflows/`: Cargo CI, optional Rust quality checks, and the `main` branch source check.

Keep development Cargo-based; do not reintroduce a JavaScript package workspace, Bun wrappers, or the retired tooling and archive directories. JavaScript, HTML, CSS, Wasm, and data files embedded by Rust or consumed by its tests are product inputs: preserve their include paths, provenance, and licenses. Keep them inside their owning Rust crate or the plugin fixtures. [Plugin build instructions](plugins/README.md) describe how to rebuild the committed components.

Run Cargo from the repository root so `rust-toolchain.toml` applies. Keep builds locked. `Cargo.lock` and `fuzz/Cargo.lock` own separate dependency graphs; standalone probes also own their locks. Pin external dependencies exactly, document their purpose and licensing, and keep test-only dependencies in `[dev-dependencies]`.

Inspect the working tree before editing and preserve unrelated changes. Never overwrite sessions, credentials, or user settings while checking behavior. Bake defaults to `~/.bake`; `BAKE_HOME` takes precedence over `DSH_HOME`. Preserve the existing environment fallbacks and user-owned command links.

## Code ownership

Follow the [architecture](docs/architecture.md) and [crate table](docs/development.md#crates). `bake-cli` owns executable entrypoints and embedded docs; `bake-coding-agent` owns sessions, tools, settings, and integrations; `bake-agent` owns the model/tool loop; `bake-ai` owns model protocols. Keep terminal effects in `bake-tui` and pure editor/view state in `bake-tui-view`. Process-tree ownership belongs in `bake-process`, confinement in `bake-sandbox`, and signed updates in `bake-update`.

Tool execution must cross the permission and approval boundary before side effects. Cancellation and teardown await owned work, stop owned descendants, restore terminal state, and leave no late callbacks. Rust must not panic on untrusted input. Keep changes in the crate that owns the contract rather than duplicating policy in callers.

## Commands and checks

Run from the repository root:

```sh
cargo run --locked -p bake-cli --
cargo run --locked -p bake-cli -- preview
cargo build --workspace --locked
cargo build --release --locked -p bake-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --locked -p bake-agent --lib
```

Use the [Bake testing skill](.agents/skills/bake-testing/SKILL.md) for Rust changes. Start with affected crates and their consumers. Tests own their temporary files, ports, subprocesses, and global mutations. Mock external nondeterminism, never the runtime under test. Parsers and transforms of untrusted input need invariant property tests. Use mutation, coverage, and fuzz checks where the changed logic warrants them; their commands and limits are in [testing](docs/testing.md).

For documentation-only changes, check the diff, local links, and commands against the source; do not claim runtime test evidence from that review. Embedded user docs and the root changelog are executable inputs, so changes to those also need a CLI build. Formatting must preserve unrelated working-tree edits. Run `git diff --check` before handing off changes.

Before pushing to `develop`, run format, Clippy, workspace tests, and build on Linux. Terminal changes also require an expected-output test and a real-terminal exercise. Agent, native, and terminal changes need those Cargo checks on the Intel x86_64 Mac via SSH alias `macpro` or `BAKE_MAC_HOST`, in `~/bake-ci` or `BAKE_MAC_DIR`. Report an unreachable host instead of claiming evidence. CI runs only on the release pull request into `main`, where it checks every platform before the merge, and on the release tag. During development, a manual run from a scratch branch (`gh workflow run ci.yml --ref <branch>`) checks only Windows and Apple Silicon, or the platforms its `platforms` input names; use it when a platform change needs that evidence early, and do not start one that repeats a check already made. Never commit a host address or bypass hooks. Report only checks actually run, including failures and skips.

## Branches and releases

`origin` is Bake's repository. Work lands directly on an up-to-date `develop`, rebased on `origin/develop`; no pull requests go into `develop`, and it is never force-pushed. `main` accepts only merge-commit release pull requests from `develop`. Never commit directly to `main` or `release/0.3`. Do not restore the retired runtime or merge `release/0.3` into `develop` or `main`. Do not merge from or push to an upstream reference automatically.

Release tags require an explicit request from the owner. CI checks the Rust workspace. A pushed `v<version>` tag runs the release workflow (`.github/workflows/release.yml`), which builds, signs, and publishes the native release to every install through `bake-release` and the download service in `distribution/`; [distribution/README.md](distribution/README.md) describes it. The Rust updater remains part of the product. Keep existing deployed download routes intact unless changing them is explicitly requested.

## Agent contracts

Port primarily from Pi's latest official release. Re-check it when starting a scope, record the revision and adapted files, preserve its MIT notice, and port its tests. Bake keeps the CLIProxyAPI route, sandbox/approvals/permission presets, and the Bake home and its settings and credentials. Retained contracts name their original source revision in fixture provenance. The [Pi-first plan](docs/roadmap/rust-0.4/pi-first-plan.md) governs where older scope text conflicts. Bake reads and writes Pi sessions; it does not migrate, read, or lock the retired agent's Session logs. Its system prompt is Bake's own.

With a public API change, update the crate row and test paragraph in `docs/development.md`, and the roadmap Dev Note.

## Evaluation and documentation

Changes to prompts, tools, results and errors, context, compaction, caching, the agent loop, or model adapters need paired evaluation against a clean built worktree of the base `develop` commit. Compare the built executables and document the method, models, prompts, trial counts, results, and accepted regressions with the change. The repository has no evaluator runner; do not prescribe deleted commands or claim a standard suite ran. Report missing live-model access and regressions to the user; keyless fixtures alone do not measure model quality.

Documentation describes current Bake behavior in English. Update embedded user docs when changing commands or behavior, and the owning crate documentation when changing APIs. Keep comments local and explain obligations that code cannot show. Files end with one newline. Never commit credentials or `.env` files.

Keep MIT in `LICENSE` and Cargo metadata, including Bake contributors' copyright and existing upstream notices. DeepSeek and Pi acknowledgements belong at the bottom of the root README; preserve detailed attribution in `THIRD_PARTY_NOTICES.md` and crate NOTICE/license files. Historical references should link to an existing Git revision rather than a deleted local path.
