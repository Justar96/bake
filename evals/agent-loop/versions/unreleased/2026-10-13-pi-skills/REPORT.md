# Pi's skills and the ~/.agents locations in bake-rs

Candidate `v0.3.5+1bfc72be8f-native` at `1bfc72be8f`, compared with `v0.3.5+b74d1b8c99-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-13T13:59:53.028Z.

Both arms are bake-rs -p from clean release builds: the base at the PR base (b74d1b8c99) and the candidate at 1bfc72be8f, which loads Pi's skills and the global AGENTS.md from the universal .agents locations and no longer reads CLAUDE.md. Every cliproxyapi model ran through the gateway Pi's CLIProxyAPI provider uses (EVAL_GATEWAY), so this record is not comparable with the earlier native records, which used another gateway; DeepSeek ran on its own API. The evaluator's model catalog does not list gemini-3.8-flash-medium, so the Gemini rows run gemini-3.8-flash-high at medium effort, as in the earlier native records. Each sample ran with an empty user home of its own, and no fixture holds a skill or an .agents directory, so the candidate's skills section is absent: the system prompts of all 144 pairs match once the executable path in the docs line (7 characters longer in the base worktree, named twice) and temporary paths are normalized, the captured first requests of ordinary_edit trial 0 match on all six models, and no sample ran bake-rs docs. The record therefore measures no model-visible change, and the standard suite cannot measure the skills section itself. Its flagged differences are run-to-run variance, accepted: gemini-3.8-flash-high total tokens +20.2% come from path_discovery (+70.6%, requests 12 -> 15) and shell_then_edit (+32.2%, 12 -> 15), with the other scenarios within 2.3%; deepseek-v4-pro +5.6% comes from shell_then_edit (+24%, 18 -> 19) and multi_site_edit (+11.2%), with the rest within 1.6%. gpt-6.1-sol failures 1 -> 3: all four failed samples (three candidate stale_edit, one base multi_site_edit) are the whitespace runaway inside an edit call's arguments that reaches the wall-clock limit, because bake-ai has no runaway guard (the follow-up the native-arm record names).

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-v4-pro vs v0.3.5+b74d1b8c99-native: total tokens 5.6% [0.7, 11.4]
- gemini-3.8-flash-high vs v0.3.5+b74d1b8c99-native: total tokens 20.2% [3.8, 44.4]
- gpt-6.1-sol vs v0.3.5+b74d1b8c99-native: failures 1 -> 3

## Against `v0.3.5+b74d1b8c99-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.4% [-1.1, 0.4] | -0.8% [-2.9, 1.6] | 69 → 69 (0% [0, 0]) | 75 → 75 | 83% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -0.5% [-1, -0.1] | -1.5% [-3.2, -0.3] | 66 → 66 (0% [0, 0]) | 69 → 67 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +6.6% [-3.3, 17.5] | +5% [-0.5, 12.1] | 85 → 87 (+2.4% [-5.9, 11.1]) | 99 → 115 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +5.6% [0.7, 11.4] | +3.6% [-0.2, 8] | 96 → 98 (+2.1% [0, 5.2]) | 100 → 106 | 81% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +20.2% [3.8, 44.4] | +21.7% [4.4, 47.5] | 69 → 75 (+8.7% [3.1, 13.7]) | 94 → 102 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 17/21 | -10.3% [-19.3, -1] | +8.4% [-13.3, 42.1] | 84 → 83 (-1.2% [-3.6, 0]) | 69 → 69 | 39% → 26% | 0 → 0 | 1 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +2% [-0.8, 6.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.5% [-0.8, -0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.3% [-0.8, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -1.4% [-2.6, -0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +0.2% [-0.5, 0.9] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -2.6% [-4, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.9% [-4.7, -0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.2, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.2% [-0.2, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.2% [-0.4, 0] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -1.2% [-3.2, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.2% [-0.3, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +0.1% [-0.3, 0.6] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -2.9% [-4.4, 0.1] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +26.3% [3.7, 77.8] | 11 → 13 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -9.5% [-26.6, 2.6] | 11 → 10 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +7.8% [-24.4, 32.8] | 17 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +20.2% [-3, 40.1] | 12 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +10.2% [-7, 40.5] | 12 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -8.8% [-29.5, 7.9] | 10 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +1.2% [-2, 8.4] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +11.2% [-2.7, 39.6] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.5% [-0.7, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -1.6% [-5.2, 4.6] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +24% [18, 28.8] | 18 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.2% [-0.9, 0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -0.9% [-4.6, 2] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +1.1% [0.3, 1.8] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +2.3% [-5.9, 6.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -0.9% [-4.2, 3.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +70.6% [6.6, 189.8] | 12 → 15 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +32.2% [30.5, 33.9] | 12 → 15 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +0.7% [-4.3, 3.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -1.1% [-2.2, 0.5] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -12.6% [-20.2, 0] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -11% [-21.7, -0.4] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -21.1% [-21.3, -20.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -10.1% [-43.2, 24.5] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -14.3% [-21.7, -0.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 0/3 | n/a | 0 → 0 | 0 → 3 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +16.5% [-0.6, 27.8] | 12 → 12 | 0 → 0 |

</details>

## `v0.3.5+1bfc72be8f-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6828 | 10075 | 3.29 | 83% | 0 |
| claude-sonnet-5-5 | 24/24 | 6830 | 9346 | 3.14 | 84% | 0 |
| deepseek-flash | 24/24 | 6716 | 10680 | 4.14 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6717 | 11716 | 4.67 | 82% | 0 |
| gemini-3.8-flash-high | 24/24 | 6751 | 8173 | 3.57 | 0% | 0 |
| gpt-6.1-sol | 21/24 | 6741 | 8411 | 4.83 | 26% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 3 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 21 | 11 | 3 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 32 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 9 | 2 | 1 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 30 | 18 | 0 | 18/18 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 2 | v0.3.5+b74d1b8c99-native | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | v0.3.5+1bfc72be8f-native | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 1 | v0.3.5+1bfc72be8f-native | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 2 | v0.3.5+1bfc72be8f-native | wall_clock_limit |
