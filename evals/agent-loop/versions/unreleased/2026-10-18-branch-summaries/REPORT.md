# Scope 10: /tree branch summaries, /clone, and compaction proof

Candidate `scope10+3e77d0897b` at `3e77d0897b`, compared with `develop+aa83c51d89` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-18T14:52:03.646Z.

Paired clean native builds: base origin/develop aa83c51d89 in its own worktree, candidate 3e77d0897b (branch summaries, /clone, compaction proof tests). Standard suite, 7 models, 3 trials, 336 samples, all successful. The change alters what the model sees only after an interactive /tree navigation where the user chooses a summary: the branch summary request and the branchSummary message in later context. No headless scenario can drive /tree, so that path is covered by faux-provider and PTY tests, and its summarization prompts are Pi's byte for byte (tests/fixtures/pi-compaction/golden.json). This record shows the default model surface unchanged: tool schemas are identical on every model, and every first-request difference is the executable path in the docs line (the candidate's worktree name is 4 characters longer, named twice, hence +8 bytes), the temporary workspace directory, and prompt_cache_key. The one flag, gemini-3.8-flash-high tool errors 1 -> 2, is git run outside a repository in ordinary_edit and path_discovery, which happened in both arms; it is model-generated and unrelated to the change, and is accepted.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-high vs develop+aa83c51d89: tool errors 1 -> 2

## Against `develop+aa83c51d89`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +0.8% [-1.6, 4.5] | -0.5% [-5.2, 4.9] | 66 → 67 (+1.5% [0, 4.7]) | 86 → 86 | 85% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +4.3% [-0.2, 9.7] | +0.6% [-4.7, 6.7] | 66 → 69 (+4.5% [0, 9.4]) | 79 → 81 | 86% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -0.3% [-7.1, 6.9] | +0.9% [-2.6, 4.6] | 91 → 89 (-2.2% [-6.5, 2.2]) | 89 → 82 | 76% → 76% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +1.2% [-2.8, 6.2] | +2.6% [-0.2, 5.7] | 93 → 94 (+1.1% [-2.2, 5.1]) | 99 → 98 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -3.2% [-12.8, 7.1] | -4.8% [-15.3, 6.7] | 76 → 73 (-3.9% [-12.3, 5.3]) | 109 → 105 | 0% → 0% | 1 → 2 | 0 → 0 |
| gemini-4-argon-high | 21/21 | -2.6% [-5.6, -0.3] | -2.3% [-5.4, -0.2] | 66 → 66 (0% [0, 0]) | 87 → 87 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/21 | -0.3% [-3.4, 3.2] | +5.5% [-17.6, 34.1] | 103 → 103 (0% [-2.9, 2.8]) | 85 → 84 | 57% → 55% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.1% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -2.6% [-6.6, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.1% [-1.3, 1.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -1.8% [-6.4, 0.9] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +11.3% [-0.3, 33.7] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.1% [-1.1, 1.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.1% [-0.3, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.2, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.3% [-1.1, 1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.1% [-2.7, 2.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +34% [33.2, 34.4] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.1% [-0.2, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -2% [-5.7, 0] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -11.5% [-32.3, 5.6] | 11 → 10 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | 0% [-1, 1.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +2% [-0.4, 5.7] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +0.9% [-13, 13.8] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +18.8% [3.1, 37.2] | 15 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -15.7% [-28.7, 6.7] | 14 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -1.1% [-6.2, 3.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +1.8% [-0.1, 5.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +10.8% [-1.1, 38.3] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +1% [0, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -1.7% [-4.7, 0.3] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +0.2% [-14.6, 25.9] | 17 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -1.5% [-5.5, 0.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -1.2% [-4.4, 3.2] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -14.1% [-38.8, 6] | 12 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -3.6% [-5, -2.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +0.7% [-18, 34.4] | 11 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -17.3% [-38.4, -3.8] | 16 → 14 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +2.6% [-21.1, 21.9] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +1.4% [-12.9, 12.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +28.4% [8.4, 48] | 9 → 11 | 0 → 0 |
| gemini-4-argon-high | multi_file_edit | 3/3 | -0.4% [-0.7, -0.1] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_site_edit | 3/3 | +1% [-0.3, 1.7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | ordinary_edit | 3/3 | -0.2% [-0.3, -0.1] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | path_discovery | 3/3 | -7.2% [-20.3, -0.1] | 12 → 12 | 0 → 0 |
| gemini-4-argon-high | shell_then_edit | 3/3 | -7.2% [-8.2, -5.4] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | stale_edit | 3/3 | -0.2% [-1.5, 0.9] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | unprompted_edit | 3/3 | -0.1% [-0.3, 0.1] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +7.2% [0.1, 20.6] | 18 → 19 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -8.3% [-21.3, 0] | 13 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | 0% [-0.1, 0] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -3.1% [-5.8, -0.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | 0% [0, 0] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -0.3% [-0.4, 0] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -0.1% [-4.7, 4.8] | 15 → 15 | 0 → 0 |

</details>

## `scope10+3e77d0897b` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6968 | 10985 | 3.19 | 85% | 0 |
| claude-sonnet-5-5 | 24/24 | 6970 | 10875 | 3.29 | 86% | 0 |
| deepseek-flash | 24/24 | 6842 | 10359 | 4.24 | 76% | 0 |
| deepseek-v4-pro | 24/24 | 6843 | 11081 | 4.48 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6836 | 9452 | 3.48 | 0% | 2 |
| gemini-4-argon-high | 24/24 | 6836 | 6366 | 3.14 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 6828 | 9010 | 4.9 | 55% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 3 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 23 | 18 | 2 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 28 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 7 | 0 | 1 | 21/21 | 0 |
| gemini-4-argon-high | 21 | 0 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 37 | 21 | 0 | 21/21 | 0 |
