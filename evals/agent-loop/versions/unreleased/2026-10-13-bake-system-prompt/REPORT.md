# Bake's own system prompt for bake-rs against Pi's

Candidate `v0.3.5+626e3249b0-native` at `626e3249b0`, compared with `v0.3.5+8788481281-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-13T11:17:04.441Z.

Both arms are bake-rs -p from clean release builds: the base at the PR base (8788481281, Pi's prompt) and the candidate at 626e3249b0, which builds Bake's own prompt (D33) and runs a response's bash, edit, and write calls in order. Both keep Pi's tools, the danger-full-access preset, and agent retry off. gemini-3.8-flash-medium was not configured on the gateway on 2026-10-13, so the Gemini rows run gemini-3.8-flash-high at medium effort, as in the native-arm record. The candidate's first request is about 690 bytes smaller. Requests fall 16.5% on both Claude models, 12.5% on deepseek-flash, 54.5% on Gemini, and 4.9% on gpt-6.1-sol; deepseek-v4-pro is unchanged (94 -> 94) and still splits edits from their checks. Total tokens fall on every model (5 to 59%). Gemini ran git in the fixture workspace, which is not a repository, in 19 of 24 base samples and 3 candidate samples, after the prompt began stating 'Git repository: no'; its tool errors fall 20 -> 1. claude-sonnet-5-5 tool errors 0 -> 1: in one unprompted_edit sample Sonnet ran the failing test to reproduce the bug before fixing it, and Pi's bash reports a non-zero exit as a tool error; accepted. gpt-6.1-sol: 12/21 pairs. Its 10 failures (7 base, 3 candidate) are all the whitespace runaway after an edit call's arguments that reaches the wall-clock limit, because bake-ai has no runaway guard (the follow-up the native-arm record names).

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5-5 vs v0.3.5+8788481281-native: tool errors 0 -> 1

## Against `v0.3.5+8788481281-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -21.4% [-29.1, -13.2] | -12.9% [-16.9, -8.7] | 79 → 66 (-16.5% [-24.7, -7.1]) | 87 → 85 | 86% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -22.5% [-27.5, -17.1] | -13% [-16.8, -8.9] | 79 → 66 (-16.5% [-22, -10.7]) | 81 → 81 | 86% → 85% | 0 → 1 | 0 → 0 |
| deepseek-flash | 21/21 | -20.4% [-29.3, -10.6] | -7.3% [-10.9, -3.5] | 96 → 84 (-12.5% [-19.6, -4.7]) | 106 → 96 | 80% → 76% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -5.1% [-8.8, -1.8] | +2.4% [-0.7, 5.9] | 94 → 94 (0% [-3.1, 3.2]) | 96 → 101 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -59.2% [-64.5, -54] | -61% [-66.3, -55.7] | 165 → 75 (-54.5% [-58.6, -50.6]) | 144 → 102 | 0% → 0% | 20 → 1 | 0 → 0 |
| gpt-6.1-sol | 12/21 | -13.6% [-18.2, -8.9] | -16.5% [-37.5, 13.2] | 61 → 58 (-4.9% [-9.5, 0]) | 49 → 48 | 40% → 41% | 0 → 0 | 7 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -14.8% [-26.5, -7.1] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -8.7% [-9.3, -8.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -8.3% [-9.5, -6.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -4.5% [-7.4, -1.3] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -42.1% [-43.6, -40.2] | 15 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -45.4% [-45.5, -45.3] | 15 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -6.7% [-8.9, -5.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -31.2% [-31.2, -31.2] | 12 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -8.2% [-8.5, -8.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -24.4% [-30.8, -8.3] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -14.2% [-26.6, -5.2] | 13 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -34.6% [-43.3, -29.2] | 13 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -31.2% [-31.5, -31.1] | 12 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -6.1% [-8.1, -2.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -3.5% [-10.3, 2.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -22.2% [-46.1, -6.5] | 13 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -7.8% [-36.1, 30.7] | 11 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -37.1% [-49.3, -10.1] | 20 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -17.3% [-31.6, 5.5] | 15 → 13 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -22.4% [-39.7, 7.1] | 13 → 11 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -14.5% [-32.2, -4.2] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -3.3% [-6.2, 1.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -4.7% [-9.1, -1.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -7.7% [-9.2, -5.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -4.9% [-7.3, -0.4] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -0.8% [-6.1, 8.4] | 18 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.4% [-8.3, 12.5] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -15.9% [-29, -4.5] | 13 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -74.5% [-80.4, -65.7] | 29 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -58.4% [-64.3, -54.9] | 19 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -58.8% [-62, -57] | 21 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -44.7% [-50.7, -40] | 27 → 15 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -55% [-60.2, -52.6] | 29 → 14 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -61.7% [-70.3, -44.3] | 20 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -60.9% [-66.1, -55.7] | 20 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 1/3 | -4.6% [-4.6, -4.6] | 6 → 6 | 2 → 0 |
| gpt-6.1-sol | multi_site_edit | 0/3 | n/a | 0 → 0 | 2 → 1 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -7% [-8.4, -4.6] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | -14.9% [-17.2, -12.5] | 10 → 10 | 0 → 1 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -24.7% [-25.1, -24.5] | 18 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -7.9% [-8, -7.8] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 1/3 | -8.8% [-8.8, -8.8] | 4 → 4 | 2 → 0 |

</details>

## `v0.3.5+626e3249b0-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6832 | 10505 | 3.14 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6834 | 10190 | 3.14 | 85% | 1 |
| deepseek-flash | 24/24 | 6720 | 9732 | 4 | 76% | 0 |
| deepseek-v4-pro | 24/24 | 6721 | 11139 | 4.48 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6755 | 8297 | 3.57 | 0% | 1 |
| gpt-6.1-sol | 21/24 | 6745 | 8962 | 4.89 | 39% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 18 | 14 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 28 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 9 | 0 | 1 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 32 | 18 | 0 | 18/18 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 0 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | multi_file_edit | 1 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 0 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 1 | v0.3.5+626e3249b0-native | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 2 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 1 | v0.3.5+626e3249b0-native | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | v0.3.5+626e3249b0-native | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 1 | v0.3.5+8788481281-native | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 2 | v0.3.5+8788481281-native | wall_clock_limit |
