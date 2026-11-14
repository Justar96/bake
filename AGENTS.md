# Bake

Bake is an independent, MIT-licensed terminal coding agent. Its product is the `bake` executable built from the Cargo workspace at the repository root. User-facing text calls it Bake. The repository contains Rust, its embedded assets and test fixtures, documentation, and Cargo-based CI. Linux x86_64 and Windows x64 are released from 0.4.7 (Windows without a qualified sandbox); 0.4.6 was the last Linux arm64 release, and macOS support follows later.

Use the current source, [development guide](docs/development.md), and [contribution guide](CONTRIBUTING.md) for implementation and commands. [Direction](docs/direction.md) holds the current priorities and the rules Bake keeps. Historical commands, deleted tooling, and linked Git snapshots do not define the current development workflow. Bake 0.3, the earlier TypeScript agent, is removed: do not add compatibility with its files, commands, `DSH_*` variables, or download route.

## Workspace

- `crates/`: agent, model adapters, tools, session services, sandbox, terminal, and executable.
- `plugins/`: Wasm plugin SDK examples and hostile fixtures.
- `fuzz/`: parser fuzz targets with a separate nightly toolchain and lockfile.
- `crates/bake-cli/docs/`: user documentation embedded in `bake`.
- `docs/`: architecture, development and testing guidance, and the development direction.
- `.github/workflows/`: Cargo CI, optional Rust quality checks, and the `main` branch source check.

Keep development Cargo-based; do not reintroduce a JavaScript package workspace, Bun wrappers, or the retired tooling and archive directories. JavaScript, HTML, CSS, Wasm, and data files embedded by Rust or consumed by its tests are product inputs: preserve their include paths, provenance, and licenses. Keep them inside their owning Rust crate or the plugin fixtures. [Plugin build instructions](plugins/README.md) describe how to rebuild the committed components.

Run Cargo from the repository root so `rust-toolchain.toml` applies. Keep builds locked. `Cargo.lock` and `fuzz/Cargo.lock` own separate dependency graphs. Pin external dependencies exactly, document their purpose and licensing, and keep test-only dependencies in `[dev-dependencies]`.

Inspect the working tree before editing and preserve unrelated changes. Never overwrite sessions, credentials, or user settings while checking behavior. Bake defaults to `~/.bake`, or `BAKE_HOME` when set. Preserve user-owned command links.

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

Before pushing to `develop`, run format, Clippy, workspace tests, and build on Linux. Terminal changes also require an expected-output test and a real-terminal exercise. CI runs locally, not on GitHub Actions: before a release, run those Cargo checks and the locked release build yourself on Linux x86_64, and the locked release build, pack, and an install with `install.ps1` on Windows x64. There are no macOS checks until Bake releases that platform. Do not start the GitHub workflows in `.github/workflows/` unless the owner asks. Never commit a host address or bypass hooks. Report only checks actually run, including failures and skips.

## Branches and releases

`origin` is Bake's repository. Work lands directly on an up-to-date `develop`, rebased on `origin/develop`; no pull requests go into `develop`, and it is never force-pushed. `main` accepts only merge-commit release pull requests from `develop`. Never commit directly to `main`. Do not restore the retired TypeScript runtime or merge its history into `develop` or `main`. Do not merge from or push to an upstream reference automatically.

Releases and their tags require an explicit request from the owner. A release is built, signed, and published locally, following [Release steps](distribution/README.md#release-steps): `bake-release prepare <version>` sets the version, lockfiles, and changelog on `develop` for the release pull request; after the merge, build, pack, check the glibc floor of, and rehearse the release `bake` on Linux x64, and pack it on Windows x64; then `bake-release publish <version>` tags `main`, creates the GitHub release with the archives, signs the manifest with the owner's local key, deploys the download service, and runs `verify-live`. Run `publish --dry-run` first. The service redirects archive downloads to the GitHub release, so that release must be public before the deploy. A deploy is not a release until `verify-live` passes. The Rust updater remains part of the product. Keep existing deployed download routes intact unless changing them is explicitly requested.

## Agent contracts

Port primarily from Pi's latest official release. Re-check it when starting a port, record the revision and adapted files, preserve its MIT notice, and port its tests. Bake keeps the CLIProxyAPI route, sandbox/approvals/permission presets, and the Bake home and its settings and credentials. Ported fixtures name their Pi source revision. Bake reads and writes Pi sessions. Its system prompt is Bake's own.

With a public API change, update the crate row and test paragraph in `docs/development.md`, and `docs/direction.md` when it changes a rule or priority.

## Evaluation and documentation

Changes to prompts, tools, results and errors, context, compaction, caching, the agent loop, or model adapters need paired evaluation against a clean built worktree of the base `develop` commit. Compare the built executables and document the method, models, prompts, trial counts, results, and accepted regressions with the change. The repository has no evaluator runner; do not prescribe deleted commands or claim a standard suite ran. Report missing live-model access and regressions to the user; keyless fixtures alone do not measure model quality.

Documentation describes current Bake behavior in English. Update embedded user docs when changing commands or behavior, and the owning crate documentation when changing APIs. Keep comments local and explain obligations that code cannot show. Files end with one newline. Never commit credentials or `.env` files.

Keep MIT in `LICENSE` and Cargo metadata, including Bake contributors' copyright and existing upstream notices. DeepSeek and Pi acknowledgements belong at the bottom of the root README; preserve detailed attribution in `THIRD_PARTY_NOTICES.md` and crate NOTICE/license files. Historical references should link to an existing Git revision rather than a deleted local path.
