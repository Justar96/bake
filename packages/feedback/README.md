---
description: "The feedback package group: user feedback on sessions, for users and maintainers choosing, composing, or debugging feedback capture."
kind: "package-group"
---

# feedback/ — recorded human feedback

## Summary

The feedback group collects human opinions about the harness's work: users submit a free-text remark about a whole session with the `/feedback` command. Feedback never reaches the model — it is a signal about the output, never input to it. This page maps the group; the package README and the [feedback subsystem page](../../docs/subsystems/feedback.md) own the contract.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

<a id="packages"></a>
## Packages

| Package | Role |
|---|---|
| [`command-feedback`](command-feedback/README.md) | Session-level feedback: the `/feedback` command, the `sessionFeedback` Remote, and the fixed category taxonomy, all without a model turn |

Session remarks are a one-way signal: recording one is safe at any point in a conversation and never changes what the model sees. With a feedback-gated sharing policy, recording a session remark is what releases the session for sharing. Bake ships no sharing destination, so feedback stays in the local session log until you configure a telemetry collector with `DSH_TELEMETRY_OTLP_URL`.

<a id="related-documentation"></a>
## Related documentation

- [Feedback subsystem](../../docs/subsystems/feedback.md) — the session remark event and its category taxonomy.
- [Session telemetry subsystem](../../docs/subsystems/session-telemetry.md) — the sharing policy disclosed by the `/feedback` acknowledgement.
- [Anonymous user identity](../identity/README.md) — the per-harness-home id embedded in the feedback acknowledgement.

<a id="dev-note"></a>
## Dev Note

None.
