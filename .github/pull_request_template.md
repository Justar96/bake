## Changes

Describe the user-visible change and any runtime packages affected.

## Verification

Paste the summary `bun run preflight` printed (or `bun run verify` for the whole runtime suite). For each FAIL, WARN, or SKIP, say why it is expected or unrelated to this change. List any other commands you ran and their results, including the recorded PTY scenario for a terminal behavior change, and report missing evidence explicitly.

```text
```

- [ ] `bun run preflight` ran on this branch and its summary is above
- [ ] A terminal behavior change has an expected-output test and a PTY scenario
- [ ] A user-visible change has a `CHANGELOG.md` `[Unreleased]` entry
- [ ] The owning README or JSDoc is updated, English and Chinese pages together

## Upstream

If this imports an upstream fix, link its release or commit and explain any adaptation. Confirm that session generations and license notices remain intact.
