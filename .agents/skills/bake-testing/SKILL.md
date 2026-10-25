---
name: bake-testing
description: Select and run Bake's Cargo checks when editing, testing, or reviewing Bake Rust code, including property tests, mutation testing, coverage, and parser fuzzing.
---

# Bake testing

Use this skill while changing Bake Rust code. Run Cargo from the repository root so the pinned toolchain applies. [Bake checks](../../../docs/development.md#checks) describes the commands.

## The edit loop

1. Check formatting with `cargo fmt --all -- --check`; format only files you changed if the checkout contains unrelated edits.
2. Run `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --locked -p <crate>` for affected crates and their consumers. For one test, add `--lib <filter>`.
3. Before pushing, run `cargo test --workspace --locked` and `cargo build --workspace --locked`. Follow [AGENTS.md](../../../AGENTS.md#commands-and-checks) for terminal and platform checks.

Keep runtime assets and fixtures in their owning Rust crates. When moving an input, check every include path and run its consumer's tests.

## Proving the logic

- **Properties for untrusted input.** Parsers and text transforms get invariant `proptest` tests beside their unit tests. Fix the case count with `with_cases`; commit regression seeds.
- **Mutation testing.** After changing logic, run `cargo mutants --package <crate>`. Address missed mutations or explain why they are equivalent. File moves and comment-only changes do not require mutation testing.
- **Coverage.** Run `cargo llvm-cov --workspace --html` to inspect untested lines and regions. Confined helpers have instrumentation limits.
- **Fuzzing.** From `fuzz/`, use `cargo fuzz run <target> -- -max_total_time=120` after changing a parser a target reaches. Fix crashes with deterministic tests in the owning crate; do not commit crash artifacts.

## Dependencies

Bake pins every Rust dependency to an exact version with a comment saying why, its license, and whether it ships. Edit `Cargo.toml` by hand instead of running `cargo add`, and keep test-only crates in `[dev-dependencies]`.
