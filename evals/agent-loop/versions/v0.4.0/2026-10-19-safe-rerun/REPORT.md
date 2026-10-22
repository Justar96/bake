# Scope 08 replay classes and the selected-run Safe rerun

Candidate `c+d8c2e3c691` at `d8c2e3c691`, compared with `develop+8fdc5e13b1` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-18T19:44:19.390Z.

Paired clean native builds: base 8fdc5e13b1 (the approval-history commit, the rerun's parent after the rebase onto c5f02c476a) and candidate d8c2e3c691 (replay classes and the Safe rerun), each in its own worktree. Standard suite, 7 models, 3 trials, 336/336 successful. Recorded by owner decision although the rerun changes what the model sees only when --resume-run resumes a crashed run with an interrupted Safe call, which this suite never does: the first request's bytes and the tool definitions are identical on every model and scenario. Flagged: claude-opus-5-5 total tokens +3.5% [0.3, 8.1], almost all in shell_then_edit, where two of three candidate trials took one more request and tool call (4 instead of 3); claude-sonnet-5-5 +0.7% [0.2, 1.4], one more tool call in two trials of shell_then_edit and one of unprompted_edit. Both are the model's own path choices over byte-identical requests, not an effect of the change, and are accepted. Failures and tool errors are unchanged or lower.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs develop+8fdc5e13b1: total tokens 3.5% [0.3, 8.1]
- claude-sonnet-5-5 vs develop+8fdc5e13b1: total tokens 0.7% [0.2, 1.4]

## Against `develop+8fdc5e13b1`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +3.5% [0.3, 8.1] | +2.8% [-1.9, 8.1] | 66 → 68 (+3% [0, 7.7]) | 84 → 88 | 86% → 86% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +0.7% [0.2, 1.4] | +3.1% [-2.2, 8.8] | 68 → 68 (0% [0, 0]) | 78 → 81 | 87% → 87% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +2.7% [-5.5, 11.7] | +0.6% [-3.3, 4.7] | 89 → 89 (0% [-6.4, 6.6]) | 88 → 92 | 75% → 76% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +0.6% [-7.8, 9.9] | +1.9% [-2.9, 6.5] | 96 → 98 (+2.1% [-5, 9.6]) | 99 → 106 | 82% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -6.3% [-14.4, 2.9] | -7% [-16, 3.6] | 72 → 69 (-4.2% [-11.4, 3.1]) | 104 → 102 | 0% → 0% | 1 → 0 | 0 → 0 |
| gemini-4-argon-high | 21/21 | +2.2% [-0.9, 5.9] | +2.4% [-0.8, 6.4] | 66 → 66 (0% [0, 0]) | 89 → 88 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/21 | -1.3% [-5.9, 3.7] | +20.1% [-1.4, 46.6] | 107 → 105 (-1.9% [-5.5, 1.9]) | 86 → 85 | 60% → 51% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.4% [0.2, 1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +3.7% [-0.1, 6.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.3% [-0.7, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +0.7% [-0.1, 1.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +21.8% [-1.3, 33.8] | 9 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +0.1% [-0.6, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.2% [-0.6, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.3, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.5, 0.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.8% [0, 2.3] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +2.1% [0, 3.1] | 11 → 11 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.1% [-0.3, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +1.9% [-0.1, 5.8] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -9.8% [-30.9, 3] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -9.9% [-24.6, -1.1] | 12 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -3.1% [-5.9, -1] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -0.2% [-10.9, 11.2] | 16 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +28.8% [5.4, 45.4] | 13 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +12.8% [-6.4, 50.9] | 13 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +1% [-28.9, 37.4] | 11 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +23.2% [18.9, 25.6] | 12 → 15 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -8.8% [-22.4, 23.2] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -1.3% [-2.9, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -7.7% [-17.9, -0.9] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -16.5% [-29.9, 5.1] | 18 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +13.3% [-0.6, 39.9] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +20.6% [0.4, 33.3] | 12 → 14 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -4.7% [-26.6, 37.7] | 10 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +1.8% [-1.1, 4.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -15.5% [-29, -0.3] | 11 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -23.2% [-32.9, -4.7] | 15 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +1.2% [-0.4, 3.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +0.6% [-2.5, 7.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +15.3% [-1.8, 45.4] | 9 → 10 | 0 → 0 |
| gemini-4-argon-high | multi_file_edit | 3/3 | -0.1% [-0.3, 0.1] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_site_edit | 3/3 | 0% [-0.2, 0.1] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | ordinary_edit | 3/3 | -0.2% [-0.3, 0] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | path_discovery | 3/3 | +13% [0, 23.9] | 12 → 12 | 0 → 0 |
| gemini-4-argon-high | shell_then_edit | 3/3 | -4.6% [-6.2, -1.5] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | stale_edit | 3/3 | +3% [0.1, 7.2] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | unprompted_edit | 3/3 | -0.6% [-1.9, 0.2] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -4.2% [-12.9, 0.2] | 21 → 20 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -8% [-20.9, 0.2] | 13 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -8.4% [-21.8, 0.1] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +12.8% [0, 31.8] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +0.1% [0, 0.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -0.1% [-0.1, 0.1] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -1.4% [-4.3, 3.2] | 15 → 15 | 0 → 0 |

</details>

## `c+d8c2e3c691` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 7555 | 12058 | 3.24 | 86% | 0 |
| claude-sonnet-5-5 | 24/24 | 7557 | 11603 | 3.24 | 87% | 0 |
| deepseek-flash | 24/24 | 7429 | 11001 | 4.24 | 76% | 0 |
| deepseek-v4-pro | 24/24 | 7430 | 12358 | 4.67 | 82% | 0 |
| gemini-3.8-flash-high | 24/24 | 7397 | 9436 | 3.29 | 0% | 0 |
| gemini-4-argon-high | 24/24 | 7397 | 6951 | 3.14 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 7389 | 9700 | 5 | 51% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 23 | 16 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 32 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 3 | 0 | 0 | 21/21 | 0 |
| gemini-4-argon-high | 21 | 0 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 39 | 21 | 0 | 21/21 | 0 |
