# Code mode only against off on fan_out

Candidate `codemode-only+b3bcb94a9c` at `b3bcb94a9c`, compared with `codemode-off+b3bcb94a9c` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: fan_out. Trials: 3. Started: 2026-10-19T20:45:14.157Z.

Same build and models as 2026-10-20-codemode-current-models, on fan_out alone. only adds codemode to defaultTools with codemode.mode only, which leaves the other tools out of every request and lists them in the codemode description. That description is larger than the tools it replaces (toolSchemaChars 4692-4860 to 6997-7047), so only saves no request bytes. Every sample succeeded in both arms. Most models scripted only to run one nested bash command, so the pair measures the indirection more than fan-out: nestedCalls is mostly bash, and reads were fanned out only by the Gemini models (five and seven nested reads). deepseek-v4-pro in all three samples, and deepseek-flash in one, called bash directly though only codemode was declared. bake-rs runs that call as Pi does: only hides declarations but keeps the tools active. Accepted regressions: claude-sonnet-5 total tokens +37.2%, with requests 9 to 11; gpt-6-sol tokens +147.9% and tool errors 0 to 4, where every error is a SyntaxError from a raw newline inside a double-quoted JavaScript string that the model then rewrote as a template literal, and its scripts printed every ledger into context; kimi-k2.7-code tool errors 1 to 2: two failed scripts, each of which it rewrote. Gains: claude-opus-5 requests 15 to 10 (-33%), and gemini-3-flash requests -10% and tokens -10%, both with intervals that span zero.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5 vs codemode-off+b3bcb94a9c: total tokens 37.2% [3.8, 103.7]
- gpt-6-sol vs codemode-off+b3bcb94a9c: total tokens 147.9% [30, 430.8]
- gpt-6-sol vs codemode-off+b3bcb94a9c: tool errors 0 -> 4
- kimi-k2.7-code vs codemode-off+b3bcb94a9c: tool errors 1 -> 2

## Against `codemode-off+b3bcb94a9c`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 3/3 | -11.1% [-46.9, 153.9] | +45.4% [8.2, 423.9] | 15 → 10 (-33.3% [-40, -25]) | 12 → 7 | 76% → 61% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 3/3 | +37.2% [3.8, 103.7] | +9.8% [0.2, 16.2] | 9 → 11 (+22.2% [0, 66.7]) | 6 → 8 | 57% → 66% | 0 → 0 | 0 → 0 |
| deepseek-flash | 3/3 | +7% [-8.7, 38] | +12% [2.4, 32.2] | 16 → 14 (-12.5% [-28.6, 25]) | 13 → 11 | 75% → 74% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | -0.1% [-24.5, 7.3] | +5.1% [0.6, 8.1] | 18 → 17 (-5.6% [-16.7, 0]) | 16 → 14 | 78% → 77% | 0 → 0 | 0 → 0 |
| gemini-3-flash | 3/3 | -10.3% [-45.1, 32.9] | -11.8% [-29.9, 8.2] | 29 → 26 (-10.3% [-33.3, 11.1]) | 26 → 23 | 37% → 38% | 3 → 3 | 0 → 0 |
| gemini-3.5-flash-lite | 3/3 | +4.2% [-34, 45.5] | -7.5% [-35.9, 30.1] | 30 → 28 (-6.7% [-35.7, 25]) | 27 → 25 | 37% → 44% | 3 → 0 | 0 → 0 |
| gpt-6-sol | 3/3 | +147.9% [30, 430.8] | +51.5% [-18, 170.3] | 13 → 16 (+23.1% [0, 40]) | 12 → 13 | 50% → 70% | 0 → 4 | 0 → 0 |
| kimi-k2.7-code | 3/3 | +56% [-20.7, 233.1] | +96.5% [-2.8, 347.5] | 20 → 19 (-5% [-28.6, 28.6]) | 18 → 16 | 79% → 74% | 1 → 2 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | fan_out | 3/3 | -11.1% [-46.9, 153.9] | 15 → 10 | 0 → 0 |
| claude-sonnet-5 | fan_out | 3/3 | +37.2% [3.8, 103.7] | 9 → 11 | 0 → 0 |
| deepseek-flash | fan_out | 3/3 | +7% [-8.7, 38] | 16 → 14 | 0 → 0 |
| deepseek-v4-pro | fan_out | 3/3 | -0.1% [-24.5, 7.3] | 18 → 17 | 0 → 0 |
| gemini-3-flash | fan_out | 3/3 | -10.3% [-45.1, 32.9] | 29 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | fan_out | 3/3 | +4.2% [-34, 45.5] | 30 → 28 | 0 → 0 |
| gpt-6-sol | fan_out | 3/3 | +147.9% [30, 430.8] | 13 → 16 | 0 → 0 |
| kimi-k2.7-code | fan_out | 3/3 | +56% [-20.7, 233.1] | 20 → 19 | 0 → 0 |

</details>

## `codemode-only+b3bcb94a9c` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 3/3 | n/a | 69328 | 3.33 | 61% | 0 |
| claude-sonnet-5 | 3/3 | n/a | 78370 | 3.67 | 66% | 0 |
| deepseek-flash | 3/3 | n/a | 78741 | 4.67 | 74% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 66149 | 5.67 | 77% | 0 |
| gemini-3-flash | 3/3 | n/a | 48710 | 8.67 | 38% | 3 |
| gemini-3.5-flash-lite | 3/3 | n/a | 57144 | 9.33 | 44% | 0 |
| gpt-6-sol | 3/3 | n/a | 70102 | 5.33 | 70% | 4 |
| kimi-k2.7-code | 3/3 | n/a | 75105 | 6.33 | 74% | 2 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 3 | 4 | 0 | 0 | 0/0 | 0 |
| claude-sonnet-5 | 3 | 5 | 0 | 0 | 0/0 | 0 |
| deepseek-flash | 3 | 8 | 0 | 0 | 0/0 | 0 |
| deepseek-v4-pro | 3 | 11 | 0 | 0 | 0/2 | 0 |
| gemini-3-flash | 3 | 20 | 0 | 0 | 0/0 | 0 |
| gemini-3.5-flash-lite | 3 | 22 | 0 | 0 | 0/0 | 0 |
| gpt-6-sol | 3 | 10 | 0 | 0 | 0/0 | 0 |
| kimi-k2.7-code | 3 | 13 | 0 | 0 | 0/0 | 0 |
