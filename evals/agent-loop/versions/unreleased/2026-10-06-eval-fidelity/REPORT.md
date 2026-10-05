# Baseline under the shipped system prompt (eval fidelity)

Candidate `v0.3.5+286e8c894e` at `286e8c894e`, compared with `v0.3.5+ea221e7bfb` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-05T18:55:53.126Z.

First record under the fixed eval composition (#48): each arm now runs its headless bundle system-prompt row with only the persona swapped, so the prompt is what Bake ships (no DeepSeek Harness opener, with the working-directory line). Both arms carry the same model surface: #48 changed only the eval harness and tests, and #49 only the installers. This is a same-surface paired run that sets the absolute baseline for the prompt and tool-surface phases; absolute counts are not comparable with earlier records, whose prompts differed by two lines. It also measures the noise floor: on identical code, the token intervals of deepseek-flash (-7.9%), gemini-3.8-flash-medium (-4.1%), and gpt-6.1-sol (-8.2%) exclude zero, so for those models a paired token change within about 10% is not evidence of an effect; Claude stays within 2%. All 7 failures are gpt-6.1-sol whitespace runaways after edit arguments (5 base, 2 candidate), caught by the llm-pi-ai guard: model behavior on identical code. Loop shape baseline: edit/check splits 16, 21, and 19 of 21 for deepseek-flash, deepseek-v4-pro, and gpt-6.1-sol, against 0 to 1 for Claude and Gemini. Standard suite, standard and extended model sets, 3 trials, headless roster.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.3.5+ea221e7bfb`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +1.7% [-0.2, 5.3] | +1.8% [-0.8, 5.3] | 66 → 67 (+1.5% [0, 4.7]) | 72 → 75 | 92% → 92% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +0.1% [-0.1, 0.2] | +0.7% [-0.8, 2] | 66 → 66 (0% [0, 0]) | 67 → 66 | 93% → 93% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -7.9% [-14.1, -1.6] | -1.8% [-4.3, 1] | 89 → 83 (-6.7% [-12.2, -1.2]) | 97 → 90 | 75% → 73% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +0.6% [-1.9, 4.1] | 0% [-1, 1.1] | 91 → 92 (+1.1% [0, 3.3]) | 97 → 91 | 76% → 76% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | -4.1% [-7.6, -1] | -3.7% [-7.6, -0.6] | 71 → 69 (-2.8% [-6.8, 0]) | 99 → 96 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 15/21 | -8.2% [-12.9, -2.4] | -1.1% [-26.5, 30.9] | 69 → 68 (-1.4% [-4.3, 0]) | 57 → 56 | 61% → 57% | 1 → 0 | 5 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +1% [-0.2, 3.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.2% [-0.3, 0.9] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.2% [-0.3, 0] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +11.5% [-1.7, 36.5] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.3% [-0.4, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.1% [-0.2, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | 0% [0, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.3% [-0.1, 1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | 0% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.5% [0.4, 0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -0.3% [-0.8, 0] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +9.2% [0.2, 33.1] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -0.9% [-2.5, 0.2] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.7% [-5.3, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -7.8% [-26, 10.5] | 15 → 14 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -15% [-22.4, -1.1] | 15 → 13 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -20.4% [-31.9, -0.4] | 12 → 10 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -16.5% [-25.7, 0.7] | 12 → 10 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -0.1% [-2.5, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -1.3% [-4.8, 3.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | 0% [-0.1, 0] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -0.4% [-2.4, 2.7] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +5.4% [-8.3, 25.9] | 16 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -1.2% [-2.5, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.2% [-1.8, 4.2] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -0.5% [-1.8, 1.3] | 11 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -0.4% [-1.5, 0.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -10.3% [-25.1, -0.3] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -3.6% [-8.9, 0] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -12.6% [-20.3, -5.8] | 11 → 10 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -1.7% [-4.4, 0.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | +2.5% [-0.4, 6.8] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -8.7% [-14.9, 1.3] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | 0% [-15.2, 17.9] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -10% [-15, -0.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | -18% [-20.9, -14.4] | 11 → 10 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -10.3% [-15.6, -0.3] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | +8.1% [0, 17.6] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 0/3 | n/a | 0 → 0 | 2 → 1 |

</details>

## `v0.3.5+286e8c894e` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 12156 | 16768 | 3.19 | 92% | 0 |
| claude-sonnet-5-5 | 24/24 | 12160 | 16171 | 3.14 | 93% | 0 |
| deepseek-flash | 24/24 | 12014 | 15075 | 3.95 | 73% | 0 |
| deepseek-v4-pro | 24/24 | 12016 | 17194 | 4.38 | 76% | 0 |
| gemini-3.8-flash-medium | 24/24 | 11878 | 11412 | 3.29 | 0% | 0 |
| gpt-6.1-sol | 22/24 | 11854 | 11680 | 4.47 | 57% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 17 | 16 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 26 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 3 | 1 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 25 | 19 | 0 | 19/19 | 2 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 1 | v0.3.5+ea221e7bfb | runaway |
| gpt-6.1-sol | path_discovery | 0 | v0.3.5+ea221e7bfb | runaway |
| gpt-6.1-sol | stale_edit | 0 | v0.3.5+ea221e7bfb | runaway |
| gpt-6.1-sol | stale_edit | 0 | v0.3.5+286e8c894e | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | v0.3.5+ea221e7bfb | runaway |
| gpt-6.1-sol | unprompted_edit | 1 | v0.3.5+286e8c894e | runaway |
| gpt-6.1-sol | unprompted_edit | 2 | v0.3.5+ea221e7bfb | runaway |
