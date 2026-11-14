# Bake CI

Follow the root [agent instructions](../AGENTS.md). CI uses Cargo directly, with commands run from the repository root so its pinned toolchain and lockfile apply. Fuzz commands run from `fuzz/` with its separate nightly pin and lockfile.

- `workflows/ci.yml` checks formatting, Clippy, workspace tests, and builds. Bake's CI and releases run locally instead ([agent instructions](../AGENTS.md#branches-and-releases)), so it runs only by manual dispatch, when the owner asks. It checks Windows x64 unless the dispatch names other platforms.
- `workflows/rust-quality.yml` runs optional coverage, mutation testing, and fuzzing on manual dispatch. Keep tool pins, artifact paths, and invocation directories consistent with their Cargo configuration.
- `workflows/main-source.yml` requires pull requests into `main` to come from this repository's `develop` branch.

Preserve sandbox prerequisites and the Linux backend/fault-check requirements. A successful build or unit test is not evidence of installer, PTY, live-model, or deployment checks. Keep platform support claims separate from the hosts used to qualify code.

Keep workflow permissions read-only unless a requested operation requires more. Pass untrusted dispatch inputs and branch names through environment variables and quote them in shell commands. Do not introduce publishing, signing, deployment, or runner credentials as part of routine CI maintenance. Releases are built and published locally with `bake-release`, not by a workflow; do not add a release workflow.

After editing workflows, run `actionlint` if available and inspect every referenced local path and command. Report unavailable checks. Documentation changes in this directory need link and whitespace checks, not a CI dispatch.
