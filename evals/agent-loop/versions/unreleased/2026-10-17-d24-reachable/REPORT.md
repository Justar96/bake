# D24 escalation parameters offered only where reachable

Candidate `43bb28841d-native` at `43bb28841d`, compared with `fa471e4700-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-16T20:52:25.408Z.

D35 candidate: bake-rs offers sandbox_permissions and justification only for modes an approval could grant, so print mode under the default workspace-write preset sends Pi schemas for bash, edit, and write, no retry sentences in the bash description, no escalation hint on a denial, and a never sentence without its sandbox_permissions clause. Both arms are native bake-rs builds under EVAL_NATIVE_PERMISSION=workspace-write with bash confined by bubblewrap on Linux. gemini-3.8-flash-high at medium effort substitutes for the standard gemini-3.8-flash-medium alias, which the gateway does not list. Findings: GPT filled sandbox_permissions throughout the base arm, always with the standing workspace-write, and never in the candidate, which has no such parameter. All six GPT base failures are the runaway guard: each streamed about 2,000 whitespace characters after the required arguments of an edit call, where only the D24 parameters remained; the candidate had no runaway and passes 21/21. Total tokens fall on every model, by 7.6% to 13.2%, with intervals below zero except DeepSeek Flash and Gemini, and requests are unchanged within their intervals. The one flag, Gemini tool errors 1 to 3, is accepted: three are git commands run in the fixture, which is not a repository (one base, two candidate), and one is the candidate running the test before its fix; none is a sandbox denial or concerns the parameters.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-high vs fa471e4700-native: tool errors 1 -> 3

## Against `fa471e4700-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -12% [-12.8, -11.3] | -2.7% [-4.7, -1.4] | 67 → 67 (0% [0, 0]) | 85 → 85 | 84% → 82% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -13.2% [-15.8, -11.7] | -1.7% [-2.3, -0.9] | 67 → 66 (-1.5% [-4.5, 0]) | 81 → 81 | 84% → 82% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -9.1% [-19.8, 3.3] | -12.3% [-16.9, -7.3] | 90 → 92 (+2.2% [-8, 13.6]) | 93 → 108 | 76% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -10.4% [-14.4, -5.8] | -12.8% [-16.4, -9] | 92 → 93 (+1.1% [-2.2, 5.2]) | 98 → 101 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -7.6% [-16, 2.6] | -8.7% [-17, 1.8] | 73 → 74 (+1.4% [-5.3, 9.2]) | 101 → 104 | 0% → 0% | 1 → 3 | 0 → 0 |
| gpt-6.1-sol | 15/21 | -11% [-16.2, -5.6] | -25.8% [-40.1, -8.9] | 75 → 76 (+1.3% [-2.7, 5.8]) | 63 → 64 | 51% → 59% | 1 → 0 | 6 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -10.9% [-11, -10.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -12.8% [-13.1, -12.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -11.5% [-11.9, -11.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -12.7% [-17.1, -9.8] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -11.5% [-12.4, -10.4] | 10 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -12% [-12, -11.9] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -12.3% [-13.2, -11.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -11.9% [-11.9, -11.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -12.4% [-12.6, -12.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -12.1% [-12.5, -11.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -11.1% [-11.9, -9.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -20.1% [-32.3, -12] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -12.8% [-13.1, -12.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -12.2% [-12.3, -12.1] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -16.2% [-30.7, -3.2] | 13 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -2.6% [-32.4, 47.4] | 11 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -13.4% [-36, 20.3] | 11 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -23.4% [-46.4, -2.2] | 15 → 14 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -1.3% [-18.6, 26.7] | 15 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -2.5% [-43.7, 62.1] | 13 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -0.3% [-15.3, 28.6] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -9% [-10.7, -6.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -1% [-20.2, 19.8] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -16.3% [-24.9, -9.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -8.6% [-14.2, -3.2] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -14.6% [-23.7, 4.1] | 17 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -12.6% [-13.1, -12] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -8.8% [-11.5, -3.4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +1.1% [-14.1, 28.9] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -13.5% [-13.7, -13] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -18.4% [-40.6, -0.7] | 11 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -16.4% [-18.1, -13.7] | 14 → 14 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +20.3% [-6.3, 60] | 11 → 13 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -10% [-19.8, -3.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -18.4% [-31.1, -8.2] | 10 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -11.7% [-12, -11.2] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -12% [-12.1, -12] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -11.7% [-11.7, -11.7] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -15.1% [-31.9, 6.9] | 16 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -12.8% [-13.5, -12.4] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +3.4% [-8.3, 18.5] | 9 → 10 | 1 → 0 |

</details>

## `43bb28841d-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 5761 | 9472 | 3.19 | 82% | 0 |
| claude-sonnet-5-5 | 24/24 | 5763 | 9001 | 3.14 | 82% | 0 |
| deepseek-flash | 24/24 | 5649 | 9638 | 4.38 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 5650 | 9587 | 4.43 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 5684 | 7404 | 3.52 | 0% | 3 |
| gpt-6.1-sol | 24/24 | 5674 | 8008 | 4.9 | 59% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 26 | 17 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 27 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 8 | 0 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 37 | 21 | 0 | 21/21 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | ordinary_edit | 1 | fa471e4700-native | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | fa471e4700-native | runaway |
| gpt-6.1-sol | stale_edit | 0 | fa471e4700-native | runaway |
| gpt-6.1-sol | stale_edit | 1 | fa471e4700-native | runaway |
| gpt-6.1-sol | stale_edit | 2 | fa471e4700-native | runaway |
| gpt-6.1-sol | unprompted_edit | 2 | fa471e4700-native | runaway |
