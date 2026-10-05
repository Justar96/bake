# A background job_output wait ends the turn

Candidate `v0.3.3+ae5e6ba` at `ae5e6ba144`, compared with `v0.3.3+6bca297` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-05T05:05:47.342Z.

job_output under wakeup delivery tells the model to end its turn rather than wait, and a still-running wait returns a short notice; the standard suite starts no background job, so this measures description and schema cost only. Accepted regression: claude-sonnet-5-5 total tokens +0.7% [0.3, 1], the longer job_output description and wait parameter sent on every request. gpt-6.1-sol failed 3 base and 2 candidate runs, unrelated to this change.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5-5 vs v0.3.3+6bca297: total tokens 0.7% [0.3, 1]

## Against `v0.3.3+6bca297`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.7% [-3.7, 1] | +2.9% [0, 7.4] | 67 → 66 | 73 → 72 | 93% → 93% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +0.7% [0.3, 1] | -0.1% [-5.1, 3.5] | 66 → 66 | 66 → 66 | 94% → 94% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -6.9% [-13.6, 0.3] | -3.4% [-10.8, 4.6] | 87 → 81 | 96 → 92 | 94% → 94% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | 0% [-1.5, 1.4] | +17.2% [-0.6, 45.4] | 91 → 91 | 94 → 96 | 96% → 95% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | +4.2% [-0.7, 11.9] | +4.1% [-0.5, 11.9] | 69 → 71 | 99 → 99 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 16/21 | +3.1% [-5.6, 11.1] | -5.6% [-23.8, 12.7] | 73 → 73 | 57 → 58 | 48% → 52% | 0 → 0 | 3 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.6% [0.3, 0.9] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.6% [0.5, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.9% [0.3, 1.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +0.7% [0.4, 1.2] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -8.8% [-24.1, 3.1] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +0.7% [0.6, 0.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.9% [0.8, 1.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +0.7% [-2, 3.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +0.6% [0.6, 0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.7% [0.6, 0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.6% [0.5, 0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +0.7% [0.7, 0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.8% [0.6, 1.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +0.7% [0.7, 0.7] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -18.9% [-29.2, -1.1] | 12 → 10 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -8.8% [-25.6, -0.4] | 12 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +0.4% [-25, 35.5] | 10 → 10 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -1.4% [-6.1, 4] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -20.4% [-28.5, -12] | 15 → 12 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -4.8% [-26.2, 11.8] | 12 → 11 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +10.8% [-1.2, 35.3] | 11 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +0.4% [-3.8, 4.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -0.5% [-2.7, 0.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.6% [0.4, 0.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +1.2% [0.1, 2.2] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -3% [-7.4, 5.9] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +0.7% [0.2, 1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +1.5% [0.6, 2.7] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | +25% [0.9, 71.8] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | +0.7% [0.4, 0.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +0.6% [0.5, 0.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +0.8% [0.4, 1] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | +3.4% [-10.1, 11.8] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -2.6% [-4.8, 0.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | +1.9% [0.7, 4] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -5.3% [-30.4, 17.7] | 15 → 14 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -14.5% [-14.5, -14.5] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +18.7% [18.7, 18.7] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | path_discovery | 2/3 | +20.7% [18, 23.1] | 10 → 11 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +11.8% [0.7, 18.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | +8.7% [-0.1, 19.4] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -8.8% [-14.1, 0.9] | 12 → 12 | 0 → 0 |

</details>

## `v0.3.3+ae5e6ba` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 12140 | 16481 | 3.14 | 93% | 0 |
| claude-sonnet-5-5 | 24/24 | 12144 | 16149 | 3.14 | 94% | 0 |
| deepseek-flash | 24/24 | 11998 | 14719 | 3.86 | 94% | 0 |
| deepseek-v4-pro | 24/24 | 12000 | 16544 | 4.33 | 95% | 0 |
| gemini-3.8-flash-medium | 24/24 | 11862 | 11838 | 3.38 | 0% | 0 |
| gpt-6.1-sol | 22/24 | 11838 | 12224 | 4.53 | 47% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 0 | v0.3.3+6bca297 | exit 1 |
| gpt-6.1-sol | ordinary_edit | 1 | v0.3.3+6bca297 | exit 1 |
| gpt-6.1-sol | ordinary_edit | 2 | v0.3.3+ae5e6ba | exit 1 |
| gpt-6.1-sol | path_discovery | 1 | v0.3.3+6bca297 | exit 1 |
| gpt-6.1-sol | stale_edit | 0 | v0.3.3+ae5e6ba | exit 1 |
