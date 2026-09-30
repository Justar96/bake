# Agent Note: Content-anchored edits

Status: implemented

English | [中文](2026-09-30-content-anchored-edits.zh.md)

## Problem

The read-before-edit guard refused any `edit` of a file the session had not read, or had not read since the file last changed. In 75 recorded Bake sessions (9,444 tool calls), 103 of 213 tool errors were those refusals: 70 unread and 33 stale. Each one cost a model request, and usually a second request for the re-read, while the whole context was resent both times. Models also routed around the guard. They made 384 file edits through `bash` with Python scripts, compared with 1,700 through `edit`. Those scripts carried a median of two replacements each, and 298 of them asserted the exact-once match that `edit` already enforces. The scripts gave up atomicity, diff cards, and observation tracking. Sixteen of the 33 stale refusals followed such a shell edit in the same session.

## Decision

`dsh-fs-observation-policy` returns an `anchored` edit intent by default (`editGuard: anchored`). The intent carries the observed version when the session has one. The provider applies it with the shared `checkEditGuard` rule inside its per-target lock:

- A replacement without `replaceAll` applies to the current content whether or not the file was observed, because its exact, unique match is the precondition.
- A `replaceAll` replacement has no uniqueness anchor. It still needs an observed version (`FS_NOT_OBSERVED`) that is current (`FS_STALE_VERSION`).
- The outcome reports `basis: observed | changed | unobserved`. When the basis is not `observed`, `edit` appends the numbered edited lines to its result, capped at 40, so the model's view is current without a re-read.

`write` keeps its version guard, and `editGuard: version` restores the previous edit refusal. `edit` also accepts `edits: [{ old_string, new_string, replace_all? }]`. Every entry matches against the original content, the matched spans may not overlap, and all of them publish in one atomic write. `ctx.fs.editText` accepts one `FsEditRequest` or an array of them. `str_replace_editor` treats an anchored `str_replace` like `edit`: it matches against the content it just read, and a compare-and-swap on that stat protects the window. A line-addressed `insert` still needs an observation.

## Why a unique match is a sufficient precondition

The provider reads, matches, and writes inside one per-target lock. A concurrent change elsewhere in the file is part of the content the replacement applies to, so it is kept. A concurrent change that overlaps the matched span makes the text stop matching, and the edit fails with `FS_EDIT_NOT_FOUND`. It cannot silently overwrite the change. A model cannot reproduce a unique multi-character span of a file it has never seen except by chance, so an unread edit is in practice based on content the model obtained another way, for example through `grep` or `sed -n`. A full overwrite and a replace-all carry no such anchor, and they keep the guard.

## Alternatives considered

**Keep the strict guard and improve the refusal text.** This was the previous choice. Repeated identical refusals were suppressed without dispatch. A paired evaluation over 90 live runs showed no token saving, because the refusal and the recovery read still enter model history. Longer guidance in descriptions added about 600 input tokens to every request on one model.

**Allow stale edits but refuse unread ones.** This removes only the 33 stale refusals. The 70 unread refusals and the pull toward shell scripts remain.

**Retry the edit automatically after an internal re-read.** This is equivalent to anchoring but hides that the file changed. The anchored basis reports the change, and the tool shows the edited lines.

**A separate multi-edit tool.** Its schema would be resent on every request, and models would have to choose between two edit tools. An optional `edits` array on `edit` costs less and keeps the single-edit form models already know.

## Consequences

Unread and stale edit refusals disappear except for `replace_all`, and several same-file changes fit in one call. The edited-lines echo adds tokens only when the model had not seen the content it changed. An edit can now land in a file the model never opened, so its result is the model's first view of that region. A deployment that wants the old contract sets `editGuard: version`, and the same build can compare both guards. The [event-gate Agent Note](../architecture/2026-06-26-file-context-as-event-gate.md) still owns the event vocabulary and the provider-enforced guard; this note changes only the default edit decision.

## Verification

`packages/fs/tool-fs/tests/integration.spec.ts` covers the anchored default on the real local backend. It checks unread and changed files, a match removed by another writer, strict `replace_all` and `write`, atomic multi-edit, overlap and missing-entry diagnostics, and concurrent edits of different spans. The strict contract remains covered under `editGuard: version`. `packages/fs/fs-observation-policy/tests/policy.spec.ts` covers both guard decisions.
