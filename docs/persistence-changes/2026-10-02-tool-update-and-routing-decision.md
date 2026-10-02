---
description: "Records a persistence type transition and its compatibility acknowledgement."
kind: persistence-change
---

# 2026-10-02-tool-update-and-routing-decision

## Summary

Acknowledges two event roots that Session format 3 already writes but no record declared yet: `request/tool-update`, which logs the native tool additions and removals an in-history route sends with a request, and `subagent/routing-decision`, which logs how a delegated child's model was chosen.

## Table of Contents

- [Declaration](#declaration)
- [Compatibility](#compatibility)
- [Verification](#verification)
- [Dev Note](#dev-note)

<a id="declaration"></a>
## Declaration

```yaml persistence-change
schemaVersion: 1
id: 2026-10-02-tool-update-and-routing-decision
baseline: false
changes:
  - root: "event:request/tool-update"
    previous: null
    after: "95d0a6c9a0ac744e3428b96a596a0b536df0c8852cab0b64122bfb91608fb908"
    decision: same-version
  - root: "event:subagent/routing-decision"
    previous: null
    after: "5df6c1d20d3b13520831b4f81f5d9f90e66d46baf9b1d04bb2da9ed4a72bb795"
    decision: same-version
```

<a id="compatibility"></a>
## Compatibility

Both are additive event roots within Session format 3, so the header version stays the same. A reader that predates either event skips it as an unknown event type: `subagent/routing-decision` never reaches model history, and `request/tool-update` only describes request deltas that the request header and tool history already reconstruct. No existing root changes shape or digest.

<a id="verification"></a>
## Verification

`bun run preflight` passes on the branch, including `verify-persistence-catalog` and the runtime suite's session restore and projection tests. `bun scripts/persistence-changes.ts --check` reported exactly these two roots as unacknowledged additions, each classified `same-version allowed`.

<a id="dev-note"></a>
## Dev Note

None.
