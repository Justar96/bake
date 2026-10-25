# Contributing to Bake

Bake's agent is a [Cargo workspace](docs/development.md) rooted in this repository. Cargo builds and tests the executable. Install the toolchain pinned in `rust-toolchain.toml` and run Cargo from the repository root so the pin applies. A C compiler and platform linker are required. Linux command tools use ripgrep and bubblewrap; the default sandbox needs a working backend.

```sh
cargo run --locked -p bake-cli --
cargo run --locked -p bake-cli -- preview
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

Work on an up-to-date `develop`, rebased on `origin/develop`. Work lands directly on `develop` after checks pass; never force-push it. `main` accepts only merge-commit release pull requests from `develop`. Never commit directly to `main` or `release/0.3`, and do not merge the retired branch into either branch. Do not bypass hooks.

Before pushing, run the checks above on Linux. Agent, native, and terminal changes also need those checks on the Intel x86_64 Mac via SSH alias `macpro` or `BAKE_MAC_HOST`, in `~/bake-ci` or `BAKE_MAC_DIR`. Report an unreachable host. Windows and Apple Silicon checks use an on-demand CI run from a scratch branch (`gh workflow run ci.yml --ref <branch>`); platform changes need that evidence before pushing. Report actual passes, failures, and skips, including host architectures. Terminal behavior needs an expected-output test and a real-terminal exercise; Cargo tests alone do not establish PTY coverage.

See [testing](docs/testing.md) for property, mutation, coverage, and fuzz checks. Model-visible changes need paired evaluation against a clean build of the base `develop` commit, with model identifiers, prompts, trial counts, results, and accepted regressions recorded in the change's documentation. Do not infer model quality from keyless fixture tests.

Any change to observable semantics names the verification boundary it affects.

- Concurrency, interleaving, scheduling, retry, cancellation, recovery, ownership handoff, or liveness updates the system model, or the change states why that model is unaffected.
- Executable Rust behavior updates the Rust verification layer. A theorem-owned kernel updates its proof. Workflow or DSL semantics update conformance or differential tests.
- Do not clone one state machine across Rust, TLA+, Lean, and a DSL for symmetry. Passing independent suites does not establish equivalence.

Port primarily from Pi's latest official release, recording the revision and adapted files and keeping its MIT attribution and tests. The [support register](docs/roadmap/rust-0.4/scope-00/support.md) owns retained Bake contracts and platform limits. Update the owning README with API changes. Preserve licenses and source attribution. Never commit credentials or `.env` files.

Release tags require an explicit request from the owner. The Cargo CI workflow validates the workspace on supported build hosts; it does not publish releases.
