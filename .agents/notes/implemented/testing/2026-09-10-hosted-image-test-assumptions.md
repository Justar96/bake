# Agent Note: Hosted-image assumptions in subprocess tests

Status: implemented

## Problem

A mocked PTY exit can race a real Linux scope bootstrap when a test inherits the host's containment choice. A spawn failure racing teardown can also settle differently depending on which operation completes first. Tests must state the host capabilities they exercise and distinguish a valid settlement from an assumed race winner.

## Decision

The cases in [`subprocess-local/tests/local.spec.ts`](../../../../packages/subprocess/subprocess-local/tests/local.spec.ts) control platform selection according to what they verify.

Terminal tests with mocked PTY exits that do not exercise platform selection use `internals = { platform: 'darwin' }` to select fallback containment. The `releases a terminal after top-level exit reaches quiescence` case leaves the platform unset and makes `probeLinuxNative` return false, exercising the host's default selection without starting a real Linux scope. A probe mock alongside the platform pin is redundant because the pin bypasses that probe.

`disposal contains a spawn-failure rejection that races teardown` asserts the settlement contract: a bootstrap that published its pre-exec failure rejects with that failure, and a teardown that stopped the bootstrap first settles as the requested `SIGTERM`. Only the Linux scope records the stopped arm; the win32 job owner rejects a cancelled start, and the fallback launcher rejects the missing directory. Disposal must contain the rejection in either ordering.

## Alternatives considered

**Raising only the per-test timeout.** Rejected: additional time does not control host capabilities or make one race winner mandatory. The fixture must control the irrelevant host dependency, or its assertions must accept each valid settlement.

**Combining a platform pin with probe mocks.** Rejected as redundant: the pin selects fallback before the native probe is called. A test of the host's default platform selection instead leaves the platform unset and controls the probe result.

## Consequences

Pinned fixtures avoid unintended containment choices. Dedicated Linux-scope and win32-job cases cover their own platform behavior instead of relying on incidental selection in terminal lifecycle tests. A race test verifies settlement and cleanup without treating scheduler order as a guarantee.
