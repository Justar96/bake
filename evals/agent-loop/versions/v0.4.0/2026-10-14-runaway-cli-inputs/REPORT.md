# Runaway tool-argument guard and print-mode CLI inputs in bake-rs

Candidate `v0.3.5+6461c965f0-native` at `6461c965f0`, compared with `v0.3.5+bd7d598e54-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-14T04:26:09.611Z.

Both arms are bake-rs -p from clean release builds: the base at the PR base (bd7d598e54) and the candidate at 6461c965f0 (PR #212), which adds the whitespace-runaway guard to bake-ai's Responses, Chat Completions, and Anthropic stream processors (maxToolArgumentWhitespace, default 2000) and Pi's print-mode CLI inputs (@file, --system-prompt, --append-system-prompt, --no-context-files, --models, prompt templates, and others). The evaluator passes none of the new flags, so the standard suite measures the guard and any change to the default request: the system prompts of all 144 pairs match once the executable and temporary paths are normalized, and each model's first no_tools request has the same size in both arms. Every cliproxyapi model ran through the gateway Pi's CLIProxyAPI provider uses (EVAL_GATEWAY), with gemini-3.8-flash-high at medium effort in place of gemini-3.8-flash-medium, as in the earlier native records; DeepSeek ran on its own API. No regressions. gpt-6.1-sol failures 5 -> 2: all five base failures are the whitespace runaway inside an edit call's arguments reaching the 180 s wall-clock limit after 45 KB to 652 KB of arguments; both candidate failures are the same runaway stopped by the guard (runawayAbort) at about 2,150 characters, 13 and 20 s into the sample. The guard classifies the error as retryable, but the evaluator turns agent retry off, so those samples still fail here. The one gemini base failure is a gateway socket closed before any response byte.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.3.5+bd7d598e54-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.8% [-3.5, 1.1] | +1% [-1.9, 3.9] | 70 → 69 (-1.4% [-4.2, 0]) | 79 → 79 | 84% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +1.7% [-0.1, 5] | +0.9% [0, 2.4] | 66 → 67 (+1.5% [0, 4.7]) | 67 → 70 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -2.9% [-9.2, 3.9] | +0.2% [-3.6, 4.8] | 91 → 87 (-4.4% [-9.2, 0]) | 101 → 101 | 78% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -0.4% [-10.7, 8.5] | -2.5% [-5.7, 0.8] | 95 → 97 (+2.1% [-6.1, 9.8]) | 104 → 102 | 81% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 20/21 | +0.1% [-5.2, 6.2] | -0.2% [-5.7, 6] | 67 → 67 (0% [-4.4, 4.3]) | 91 → 95 | 0% → 0% | 0 → 0 | 1 → 0 |
| gpt-6.1-sol | 14/21 | -0.1% [-11, 11.8] | +6.4% [-17.4, 34.7] | 68 → 67 (-1.5% [-4.3, 0]) | 55 → 55 | 51% → 48% | 0 → 0 | 5 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | 0% [-5.8, 6.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | 0% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.8% [0.1, 2.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +0.4% [-2.8, 2.8] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -6.1% [-16.8, 0.6] | 13 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +0.3% [-0.5, 0.9] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.4% [-3.5, 3.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +1.6% [-0.3, 4.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.2% [-0.2, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.1% [0, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | 0% [-0.2, 0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +11.2% [0.2, 33.1] | 9 → 10 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.1% [-0.3, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -0.5% [-0.9, 0] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +0.1% [-0.2, 0.6] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -8.2% [-21, 0.9] | 13 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -6.8% [-24.4, 5.7] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +1.1% [-18.7, 26.7] | 16 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -3.2% [-21.9, 24.9] | 15 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -5.7% [-24.8, 9.4] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +1.1% [-2.4, 4.4] | 10 → 10 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -1.4% [-21.1, 27] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +7.1% [-4.7, 24.9] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | 0% [-0.4, 0.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +9% [-5.6, 16.8] | 15 → 17 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -12.9% [-44.8, 26] | 18 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -2% [-4.8, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.4% [-7.4, 5.5] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -10% [-24.7, 1] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +1.5% [0.1, 4.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -3% [-3.8, -2.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 2/3 | -10.6% [-12.6, -8.4] | 8 → 8 | 1 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +15.1% [4.3, 38.1] | 13 → 14 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +3.8% [-1.5, 8.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -0.8% [-3.6, 1.4] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 1/3 | +24% [24, 24] | 6 → 6 | 2 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | +27.7% [27.5, 27.9] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +1.3% [-18.1, 27.4] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -21.2% [-23.3, -19.5] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 2/3 | +12.4% [0.6, 27.4] | 10 → 10 | 0 → 1 |
| gpt-6.1-sol | stale_edit | 1/3 | -0.5% [-0.5, -0.5] | 4 → 4 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -5.3% [-24.9, 28.2] | 9 → 9 | 1 → 0 |

</details>

## `v0.3.5+6461c965f0-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6836 | 10157 | 3.29 | 83% | 0 |
| claude-sonnet-5-5 | 24/24 | 6838 | 9527 | 3.19 | 84% | 0 |
| deepseek-flash | 24/24 | 6724 | 10436 | 4.14 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6725 | 11282 | 4.62 | 82% | 0 |
| gemini-3.8-flash-high | 24/24 | 6759 | 7053 | 3.38 | 0% | 0 |
| gpt-6.1-sol | 22/24 | 6749 | 9025 | 4.84 | 49% | 1 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 3 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 21 | 13 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 31 | 21 | 4 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 5 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 32 | 19 | 0 | 19/19 | 2 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-high | path_discovery | 2 | v0.3.5+bd7d598e54-native | empty final reply |
| gpt-6.1-sol | multi_file_edit | 0 | v0.3.5+bd7d598e54-native | wall_clock_limit |
| gpt-6.1-sol | multi_file_edit | 1 | v0.3.5+bd7d598e54-native | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 2 | v0.3.5+bd7d598e54-native | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 0 | v0.3.5+6461c965f0-native | runaway |
| gpt-6.1-sol | stale_edit | 1 | v0.3.5+6461c965f0-native | runaway |
| gpt-6.1-sol | stale_edit | 2 | v0.3.5+bd7d598e54-native | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 1 | v0.3.5+bd7d598e54-native | wall_clock_limit |
