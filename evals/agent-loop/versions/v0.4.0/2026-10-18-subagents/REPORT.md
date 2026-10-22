# Opt-in continuable subagents (D38): delegation with the setting off and on

Candidate `subagents-on+6852a31140` at `6852a31140`, compared with `subagents-off+6852a31140` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: delegation. Trials: 3. Started: 2026-10-18T10:19:58.852Z.

Both arms are bake-rs at 6852a31140; the on arm sets `subagents: true` through EVAL_SETTINGS_ON. Without the setting the agent has no subagent tool, so every off sample fails the delegation predicate, which requires a subagent call; that failure count and the token and request differences are the point of the comparison, not regressions. Each on sample delegated once and wrote the expected summary. A child's requests pass the proxy but not the parent's event stream, so on samples have usageComplete false and their token figures cover the parent only. One gemini-3.8-flash-high off sample looped for 39 requests rereading the notes without a subagent tool, as it did against develop. The default model surface is unchanged from develop: the first-request fixtures are identical, so the standard suite needs no new pair. steps/1-on-by-default measures the tools on by default against develop on the standard suite: total tokens rose 12 to 19% on every model, with no change in requests or failures, from about 1.7 KB of tool definitions in every request; that cost made them opt-in.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `subagents-off+6852a31140`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| deepseek-v4-pro | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| gemini-3.8-flash-high | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| gemini-4-argon-high | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-v4-pro | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gemini-3.8-flash-high | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gemini-4-argon-high | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | delegation | 0/3 | n/a | 0 → 0 | 3 → 0 |

</details>

## `subagents-on+6852a31140` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 11616 | 5 | 89% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 11524 | 5 | 90% | 0 |
| deepseek-flash | 3/3 | n/a | 9999 | 6.67 | 73% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 11725 | 6 | 81% | 0 |
| gemini-3.8-flash-high | 3/3 | n/a | 10735 | 5.67 | 0% | 0 |
| gemini-4-argon-high | 3/3 | n/a | 6809 | 5 | 0% | 0 |
| gpt-6.1-sol | 3/3 | n/a | 6138 | 5 | 50% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 6 | 0 | 0 | 0/3 | 0 |
| claude-sonnet-5-5 | 3 | 6 | 0 | 0 | 0/3 | 0 |
| deepseek-flash | 3 | 11 | 0 | 1 | 0/3 | 0 |
| deepseek-v4-pro | 3 | 9 | 0 | 1 | 0/3 | 0 |
| gemini-3.8-flash-high | 3 | 8 | 0 | 0 | 0/3 | 0 |
| gemini-4-argon-high | 3 | 6 | 0 | 0 | 0/3 | 0 |
| gpt-6.1-sol | 3 | 6 | 0 | 0 | 0/3 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | delegation | 0 | subagents-off+6852a31140 | validation failed |
| claude-opus-5-5 | delegation | 1 | subagents-off+6852a31140 | validation failed |
| claude-opus-5-5 | delegation | 2 | subagents-off+6852a31140 | validation failed |
| claude-sonnet-5-5 | delegation | 0 | subagents-off+6852a31140 | validation failed |
| claude-sonnet-5-5 | delegation | 1 | subagents-off+6852a31140 | validation failed |
| claude-sonnet-5-5 | delegation | 2 | subagents-off+6852a31140 | validation failed |
| deepseek-flash | delegation | 0 | subagents-off+6852a31140 | validation failed |
| deepseek-flash | delegation | 1 | subagents-off+6852a31140 | validation failed |
| deepseek-flash | delegation | 2 | subagents-off+6852a31140 | validation failed |
| deepseek-v4-pro | delegation | 0 | subagents-off+6852a31140 | validation failed |
| deepseek-v4-pro | delegation | 1 | subagents-off+6852a31140 | validation failed |
| deepseek-v4-pro | delegation | 2 | subagents-off+6852a31140 | validation failed |
| gemini-3.8-flash-high | delegation | 0 | subagents-off+6852a31140 | wall_clock_limit |
| gemini-3.8-flash-high | delegation | 1 | subagents-off+6852a31140 | validation failed |
| gemini-3.8-flash-high | delegation | 2 | subagents-off+6852a31140 | wall_clock_limit |
| gemini-4-argon-high | delegation | 0 | subagents-off+6852a31140 | validation failed |
| gemini-4-argon-high | delegation | 1 | subagents-off+6852a31140 | validation failed |
| gemini-4-argon-high | delegation | 2 | subagents-off+6852a31140 | validation failed |
| gpt-6.1-sol | delegation | 0 | subagents-off+6852a31140 | validation failed |
| gpt-6.1-sol | delegation | 1 | subagents-off+6852a31140 | validation failed |
| gpt-6.1-sol | delegation | 2 | subagents-off+6852a31140 | validation failed |
