---
description: "Generate, acknowledge, and verify Session persistence-type changes locally before opening a pull request."
---

# Cookbook: reviewing persistence-type changes

## Summary

Use this tutorial after changing a declared Session persistence type in a contributor checkout with dependencies installed. Supply a compatibility explanation, then let one command classify the change and generate its records. The [record reference](../persistence-changes/README.md) explains the files and automatic rules. All comparison inputs live in the checkout; no base branch or network access is required.

## Table of Contents

- [Optional: inspect the change](#generate)
- [1. Record the change](#acknowledge)
- [2. Check, commit, and push](#verify)
- [Update an unaccepted record](#competing-records)
- [Dev Note](#dev-note)

-----

<a id="generate"></a>
## Optional: inspect the change

For a preview before recording, run from the repository root:

```sh
pnpm --silent run verify-persistence-changes --json
```

Use `--silent` when consuming JSON: pnpm otherwise appends lifecycle failure text to stdout. Failed commands still exit with code 1.

Read the reported root, path, change kind, and version requirement. A referenced type can affect several event digests; inspect every affected root. Until the history covers the new schemas, verification fails. A stale generated inventory also fails verification; the recording command refreshes it. If `changes` is empty after reordering fields or union alternatives, run `pnpm run gen-persistence-catalog` and repeat the check. An unchanged digest needs no new acknowledgement even when copied declarations or source locations produce a catalog diff.

<a id="acknowledge"></a>
## 1. Record the change

Write a local JSON file containing `summary`, `compatibility`, and `verification` strings. The following input describes an exercised required-to-optional hook audit field change. Replace the explanation and test evidence with facts about your change; the CLI does not establish these claims.

Save the input as `.artifacts/persistence-change.prose.json`, creating the ignored directory if needed:

```json
{
  "summary": "Makes the persisted hook audit decision optional.",
  "compatibility": "Existing records remain valid. Hook execution consumes HookOutput instead of replaying this audit field. Producers still write decisions, and absence does not imply pass.",
  "verification": "pnpm exec vitest run packages/hooks/hook-protocol/tests/events.spec.ts: 10 tests passed."
}
```

Use a date and descriptive slug in place of this example id:

```sh
pnpm --silent run persistence-changes --record 2026-09-11-poc-optional --prose .artifacts/persistence-change.prose.json --json
```

The command validates the history and prose, infers the minimum version decision, and checks any required header increase before writing. It generates the record, complete after schemas, the catalog, and the machine inventory. Review the explanations and returned `changes`, `roots`, and `files` before committing. Omitting `--prose` creates unfinished drafts that verification rejects until their explanations are completed.

Inference follows the [fixed compatibility rules](../persistence-changes/README.md#compatibility-rules); it never changes source or relaxes them. If a bump is required, first follow [adding a Session format version](adding-a-session-format-version.md). The record must include its own increasing `SessionHeader.version` transition; an unrelated historical bump cannot authorize it. Routine changes never create another baseline.

<a id="verify"></a>
## 2. Check, commit, and push

Select the changed owner's behavior checks through the [testing policy](../testing.md), then run the documentation checks:

```sh
pnpm run doc-sync
```

`doc-sync` checks persistence inventory and catalog freshness, and the complete history. A recording command's `ok: true` does not replace these checks or the owner's behavior and migration tests. JSON failures retain `ok: false`, a diagnostic `code`, and exit code 1. Structured changes include stable kinds and per-root before/after digests, so automation need not parse descriptions.

Record generation owns its catalog and record. Review and stage the intended diff, then commit and push normally. The staged lint and whitespace hooks and the pre-push Host/Client typecheck still apply.

<a id="competing-records"></a>
## Update an unaccepted record

If source changes after recording, review the compatibility explanation and refresh the same unaccepted terminal record:

```sh
pnpm --silent run persistence-changes --update 2026-09-11-poc-optional --prose .artifacts/persistence-change.prose.json --json
```

The command refreshes the machine declaration, schemas, and catalog. Without `--prose`, it preserves the existing explanation. Update refuses the initial baseline and records that another record depends on. The tree cannot identify which records were accepted in review: preserve accepted history and create a successor instead.

When integration creates competing terminal records, update the unaccepted record against the remaining history, then reassess the resulting diff. An unrelated root's acknowledgement does not need refreshing. The [mechanism decision](../../.agents/notes/implemented/process/2026-09-11-persistence-type-history.md) explains why complete snapshots and per-root predecessors are retained.

An explicit `--decision` remains a checked assertion. For an existing property's value-type change, the following deliberately wrong assertion fails before writing:

```sh
pnpm --silent run persistence-changes --update 2026-09-11-poc-optional --decision same-version --json
```

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
