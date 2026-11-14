---
name: test-writing
description: "Write tests the way this team does - what to test at the unit, integration, and end-to-end levels, naming a test after the behavior it proves, arranging fixtures, avoiding sleeps and order dependence, and making a failure message say what went wrong. Use when adding tests for new code, writing a regression test for a bug, fixing a flaky test, or when asked how to test something. Prefers a few tests that prove behavior through public interfaces over many that mirror the implementation."
---

# Writing tests

- Name tests after behavior: `test_refund_is_rejected_after_90_days`.
- One behavior per test; arrange, act, assert.
- No sleeps: wait on a condition with a timeout.
- A bug fix starts with a test that fails without it.
