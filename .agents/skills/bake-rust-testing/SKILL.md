---
name: bake-rust-testing
description: Use when editing, testing, or reviewing Rust under rust/ in the Bake repository, to run the fastest check that covers the edit, and to prove a change with property tests, mutation testing, coverage, or fuzzing instead of examples alone. Takes precedence over generic Rust guidance, such as Symposium's rust-best-practice skill, where they differ.
---

# Bake Rust Testing

Use this skill while changing the Rust agent under `rust/`. [Rust checks](../../../rust/README.md#checks) describes every command's scope and limits; this skill orders them.

## The edit loop

1. Run `bun run iterate:rust` after each edit. It formats (writing changes), runs Clippy with warnings denied, and runs the tests of the members the uncommitted change reaches and their reverse dependents, stopping at the first failure. Add `-p <crate>` to name packages, `--base <ref>` to cover a branch, or `--all` for everything.
2. For one test, run `bun run test:rust -p <crate> --lib <name>`; filters go to Cargo.
3. Before pushing, run `bun run preflight`. Outside CI its `rust-test` gate is scoped the same way; `bun run preflight --full` runs every Rust test. The [pre-push checks skill](../dsh-pre-push-checks/SKILL.md) selects the rest.

When an edit makes a member read a file outside its directory, add that file to `RUST_EXTERNAL_INPUTS` in `scripts/rust-scope.ts`; its scan test fails until you do, and scoped runs would otherwise miss the consumer's tests.

## Proving the logic

- **Properties for untrusted input.** A pure function that parses, wraps, truncates, or orders input the model, a server, or a file controls gets a `proptest` property beside its unit tests: an invariant that fails if the logic breaks, such as every split of a stream decoding to the same events. Do not restate the implementation. Fix the case count with `with_cases`, and commit any `proptest-regressions/` seed a failure records.
- **Mutation testing for the branch.** Run `bun run mutants:rust` before calling a change tested. Each `MISSED` line is a change no test notices: add an assertion that fails on it, or say in the commit why the mutant is equivalent. `--file <path>` narrows the run.
- **Coverage for gaps.** `bun run coverage:rust --html` shows untested regions. Code that runs inside a confined helper reads as uncovered; that is the tool's limit, not proof of a gap.
- **Fuzzing for panics.** Rust never panics on untrusted input. After changing a parser a `rust/fuzz` target reaches, run `bun run fuzz:rust <target> --seconds 120`. Fix a crash in its owning crate with a deterministic regression test; never commit a crashing input as a seed.

## Dependencies

Bake pins every Rust dependency to an exact version with a comment saying why, its license, and whether it ships. Edit `rust/Cargo.toml` by hand instead of running `cargo add`, and keep test-only crates in `[dev-dependencies]`.
