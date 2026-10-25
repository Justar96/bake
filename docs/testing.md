# Testing Bake

Install the toolchain pinned at the repository root and run Cargo there. Use `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and `cargo build --workspace --locked` before pushing. For a focused test, run `cargo test --locked -p <crate> --lib <filter>`. Workspace tests include documentation tests.

Tests own paths, listeners, subprocesses, clocks, and global mutations. They mock external systems, never the runtime under test. Use invariant properties for parsers and transforms of untrusted input. Commit property regression seeds; do not commit crash artifacts.

Install cargo-mutants for mutation checks, cargo-llvm-cov for line and region coverage, and cargo-fuzz for fuzzing. From the repository root, run `cargo mutants --package <crate>` and address missed mutations; explain equivalent mutants. Run `cargo llvm-cov --workspace --html` to inspect gaps. From `fuzz/`, run `cargo fuzz list` and `cargo fuzz run <target> -- -max_total_time=120` after changing a parser that target reaches. Fix crashes with deterministic regressions in the owning crate. Coverage of confined helper processes has instrumentation limits.

Terminal changes need expected-output tests and a real-terminal exercise covering the changed behavior, including restoration. Cargo unit tests do not establish PTY coverage. The Windows pseudoconsole helper remains available as the `bake-process` `conpty` example.

Model-visible changes need paired evaluation against a clean build of the base `develop` commit. Record the models, prompts, trial counts, results, and accepted regressions in documentation for the change. Fixture smokes alone do not measure model quality. Follow [contributing](../CONTRIBUTING.md) for platform checks, and report actual passes, failures, skips, and host architectures.
