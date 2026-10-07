# Rust migration qualification ledger

## Summary

Read the evidence for a Rust migration attempt, including failed checks and missing coverage. Each JSON record identifies the candidate, observed commands, fixtures, artifacts, and limits. The offline validator checks record integrity. Scope acceptance still follows the [verification contract](../verification.md#evidence-required-for-every-scope).

## Table of Contents

- [Check records](#check-records)
- [Add an attempt](#add-an-attempt)
- [Recorded evidence](#recorded-evidence)
- [Limits](#limits)
- [Dev Note](#dev-note)

## Check records

Run from the repository root after installing its Bun dependencies:

```sh
bun run verify-rust-migration-ledger
bun scripts/rust-migration-ledger.ts --help
```

The default ledger is this directory. Use `--check --ledger <directory>` to check another ledger. The command reads files without running git, accessing the network, or rewriting records. Exit 0 means every record is valid, including partial and failed attempts. Exit 1 means an invalid record or an empty ledger; exit 2 means invalid arguments or an unusable ledger directory. Diagnostics name the record and field to correct. The same check runs in preflight's generated-files group.

## Add an attempt

Write one `scope-NN/<id>.json` file per attempt. Scopes range from `00` to `17`; the filename must match its unique lowercase record ID. The [TypeScript interfaces and validator](../../../../scripts/rust-migration-ledger.ts) own version 1 of `bake/rust-migration/qualification-attempt`. Unknown fields, unexplained empty evidence lists, malformed hashes, dangling command references, contradictory pass claims, and supersedes cycles fail validation. Records must be regular UTF-8 files of at most 1 MiB. Scope directories and records cannot be symlinks. Any extra entry, including ignored dot-files such as `.DS_Store`, fails the check; remove local clutter before retrying.

Keep committed attempts unchanged. Correct a record by adding a new one whose `supersedes` array names the earlier attempt in the same scope. The same link can connect a later qualification attempt to an earlier failed attempt on another candidate. Superseding never erases the earlier observation; readers must keep failures visible. This append-only rule is a repository convention; the validator does not inspect git history. IDs carry no chronological ordering.

Use `partial` while required evidence is missing, `failed` for a failed attempt, and `evidence-review` when submitting evidence for review. `complete` is unsupported. Use `{"missing":"reason"}` for an applicable fact that was not observed, or `{"notApplicable":"reason"}` where the field permits it. Never turn an unknown exit, tool version, artifact hash, or test count into an invented value.

Record the implementation's exact commit in `provenance.candidate`. Record the checkout actually tested in each command's `testedCommit`; CI may test a synthetic merge commit. These commits can differ. Collecting evidence in a later PR avoids making the candidate depend on its own record's commit. A temporary source mutation is not a committed checkout: identify its source and changed bytes explicitly.

Fixture entries distinguish shared fixture bytes from fixture-definition source. Sort literal repository-relative paths in code-unit order, hash their exact bytes with SHA-256, then compute the manifest digest over each `path NUL sha256 LF` entry. The validator recomputes that aggregate from the record; it does not compare files against a checkout. An artifact hash belongs to the command that observed it. Local binary hashes cannot stand in for CI artifacts.

Each result names its owner and the commands that observed it. Explain the units of command counts: gates, test cases, and fixture scenarios are different quantities. A pass must cite commands that observed their expected exit and do not count failures when expecting exit 0. A rejected negative control can cite a command intentionally expecting exit 1. Keep eval and performance records in their owning formats and link them here.

## Recorded evidence

The initial records cover PR #57's fake native eval arm and existing synthetic comparison harness:

| Attempt | Observed outcome |
|---|---|
| [First macOS attempt](scope-01/2026-10-07-native-fixture-macos-failure.json) | The exact environment assertion failed because CoreFoundation added `__CF_USER_TEXT_ENCODING` after exec. The record retains the failed CI job. |
| [Corrected attempt](scope-01/2026-10-07-native-fixture-evidence.json) | A clean checkout of merged commit `71805dfe1f1e7a9aef7088bd4043740e0d5b4d34` passed 60 Cargo tests, four compiled-arm cases, and three synthetic fixtures. Final CI passed the recorded native and app gates. Scope 01 remains partial. |

The final PR head, CI synthetic merge, and merged candidate have the same tree, `5c448cfbbdce48cb9912d24d0c0ac542c9dd3c98`; their distinct commit identities remain in the record. CI command counts describe preflight gates. The local `native-check`, `native-eval`, and `conformance` counts describe Cargo tests, compiled-arm cases, and shared fixtures respectively.

For the negative control, an isolated copy of the merged candidate removed only `testsUnchanged` from the evaluator's validated predicate. The correct-edit case passed, then the smoke assertion rejected the tampered-check verdict and exited 1. Those are the two reached cases in its counts. The original source stayed unchanged and the copy was removed. The local command wrapper also observed unchanged source and no surviving command-owned descendants after each clean candidate run.

Local references under `.preflight/ledger/` identify ignored, host-local evidence by hash; they are not published artifacts. The three clean command references point to check-result JSON that binds the tested commit, source/cleanup observations, and the adjacent log hash. CI references link the actual jobs. Successful CI jobs expose gate summaries but do not retain the smoke JSON or binary digests. The record therefore marks CI artifact hashes missing. The source commits and commands allow new attempts to be reproduced; reproduction creates a new record rather than changing these observations.

## Limits

Valid JSON evidence does not prove execution, artifact availability, fixture authenticity, or reviewer acceptance. This validator checks only structure and internal consistency. It does not assign scope requirements, authorize support changes, or update roadmap status.

The initial evidence does not qualify real-runtime fixtures, a live native model arm, the full release target matrix, Windows ConPTY, or user install/session rollback. Scope 00 decisions and full baseline acceptance remain open. `actionlint` was unavailable locally and in the recorded Linux/macOS app CI. These omissions remain visible in the records even though the available checks passed.

## Dev Note

The ledger is development evidence. Existing eval records and frozen historical/session artifacts keep their own formats and preservation rules.
