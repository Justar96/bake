# Background subagent settlement recorded before its notice (delegation, subagents on)

Candidate `integration+d0c1891829` at `d0c1891829`, compared with `develop+d55ab04388` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: delegation. Trials: 3. Started: 2026-10-22T08:07:54.764Z.

Paired clean native release builds: develop@d55ab04388 (base) and the 0.4 Linux integration at d0c1891829 (candidate), which writes a background subagent run's settlement as a `bake.subagent.settled` Pi custom entry, durable before its notice is posted, beside scope 03, 04, 05, 11, 13, and 16 and D18 work that reaches no model request. Both arms set `subagents: true` and persistent sessions; the delegation scenario ran on all eight standard and extended model routes, three trials per arm, through the opn24 gateway (EVAL_GATEWAY): 48 samples, all successful. A delegation sample's usage is incomplete, because a child's requests pass the proxy but not the parent's event stream, so the table compares failures only (0 to 0 on every model); over all samples the base made 143 requests with 3 tool errors and the candidate 147 with 1. All 48 system prompts are identical once the arm's own executable path (named twice) and the workspace path are normalized, and tool-schema sizes match on every model; the candidate's 42 more first-request bytes are its longer checkout path. Most delegations were foreground calls, whose path is unchanged: 21 of 24 base and 23 of 24 candidate samples. Four were background runs, three base and one candidate; the candidate one, kimi-k2.7-code trial 2, wrote its settlement entry before its notice, and the model then received the same finished-notice text the base posts. Its one tool error was a `job_output` call with the subagent's id, which the existing error answers. The model-visible difference the change makes, a recorded settlement reported as finished rather than interrupted when the parent stops between recording and delivering it, needs a host stop, so the Rust restart and barrier tests cover it, not this run. No prompt, tool-definition, or fixture file under rust/ differs from develop, so the standard suite was not rerun, as for the 2026-10-18 subagents record. Raw samples remain in ignored .preflight/.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../../../eval-candidate/evals/README.md#regressions).

## Against `develop+d55ab04388`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| deepseek-flash | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| gemini-3-flash | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| gemini-3.5-flash-lite | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| gpt-6-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| kimi-k2.7-code | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| deepseek-flash | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| gemini-3-flash | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| gemini-3.5-flash-lite | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| gpt-6-sol | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |
| kimi-k2.7-code | delegation | 0/3 | n/a | 0 → 0 | 0 → 0 |

</details>

## `integration+d0c1891829` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 3/3 | n/a | 15734 | 5 | 92% | 0 |
| claude-sonnet-5 | 3/3 | n/a | 15880 | 5 | 92% | 0 |
| deepseek-flash | 3/3 | n/a | 14036 | 6.33 | 75% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 17756 | 6.67 | 81% | 0 |
| gemini-3-flash | 3/3 | n/a | 27319 | 8 | 12% | 0 |
| gemini-3.5-flash-lite | 3/3 | n/a | 22604 | 7 | 8% | 0 |
| gpt-6-sol | 3/3 | n/a | 8117 | 5 | 63% | 0 |
| kimi-k2.7-code | 3/3 | n/a | 13257 | 6 | 87% | 1 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 3 | 6 | 0 | 0 | 0/3 | 0 |
| claude-sonnet-5 | 3 | 6 | 0 | 0 | 0/3 | 0 |
| deepseek-flash | 3 | 10 | 0 | 0 | 0/3 | 0 |
| deepseek-v4-pro | 3 | 11 | 0 | 1 | 0/3 | 0 |
| gemini-3-flash | 3 | 15 | 0 | 0 | 0/3 | 0 |
| gemini-3.5-flash-lite | 3 | 12 | 0 | 0 | 0/3 | 0 |
| gpt-6-sol | 3 | 6 | 0 | 0 | 0/3 | 0 |
| kimi-k2.7-code | 3 | 9 | 0 | 0 | 0/3 | 0 |
