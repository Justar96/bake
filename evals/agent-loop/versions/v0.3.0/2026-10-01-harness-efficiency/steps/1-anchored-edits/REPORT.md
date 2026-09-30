# Step 1: content-anchored edits and lenient arguments

Candidate `anchored-edits`, compared with `v0.2.0+99d94e7` and `anchored-edits-version-guard` in the same run.
Models: claude-sonnet-5-5, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: unknown.

Intermediate step of the harness-efficiency change, measured on the uncommitted tree that was later squashed into the PR commit. The version-guard arm is the same tree with editGuard: version and ran one trial, so it separates the text cuts from the edit-guard change. A prefix-derived GPT prompt_cache_key was also tried (-1.2% [-8.9, +6.3]) and reverted.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../../../README.md#regressions).

## Against `v0.2.0+99d94e7`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 21/21 | -29.6% [-37.6, -20.1] | -32% [-43.6, -17.3] | 91 → 69 | 14 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 18/21 | -10.3% [-18.6, -0.8] | -10.4% [-18.5, -1.1] | 99 → 93 | 3 → 0 | 3 → 0 |
| gpt-6.1-sol | 17/21 | -21.4% [-28.4, -14.3] | -9.7% [-42, 35] | 103 → 85 | 13 → 0 | 4 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -22.2% [-28.9, -5.4] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -23% [-29.7, -5.4] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -14.9% [-29.5, -5.1] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -4.9% [-5.1, -4.8] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -51.3% [-55.2, -41.2] | 22 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -45.4% [-46.1, -43.9] | 15 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -14.3% [-28.4, -4.4] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -16% [-31.7, -4.7] | 17 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 2/3 | +6.3% [-5.2, 17.4] | 8 → 9 | 1 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -11.4% [-23.5, -5.2] | 15 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -16.8% [-24.9, -5.1] | 18 → 16 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 1/3 | -5.5% [-5.5, -5.5] | 6 → 6 | 2 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -20.4% [-38, 12.4] | 19 → 16 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | +6.9% [-23.9, 43.6] | 16 → 17 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -17.3% [-21.2, -14.2] | 14 → 12 | 1 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -31.7% [-43.9, -6.9] | 17 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 2/3 | -14% [-20.2, -6.7] | 11 → 10 | 1 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | -5.7% [-18.5, 8.3] | 13 → 13 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -21% [-26.6, -14.6] | 22 → 18 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -42.4% [-48.1, -35.1] | 13 → 8 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -14.5% [-25.5, 0.4] | 13 → 12 | 0 → 0 |

</details>

## Against `anchored-edits-version-guard`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 7/7 | -25.6% [-40.2, -8.6] | +5.6% [-28.8, 89.8] | 30 → 23 | 3 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 6/7 | -5.4% [-28.6, 31.8] | -3.4% [-27.8, 34.7] | 35 → 33 | 0 → 0 | 1 → 0 |
| gpt-6.1-sol | 7/7 | -24.9% [-36.3, -10.4] | -13.3% [-49.6, 56.9] | 43 → 33 | 3 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 1/1 | 0% [0, 0] | 3 → 3 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 1/1 | 0% [0, 0] | 3 → 3 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 1/1 | 0% [0, 0] | 3 → 3 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 1/1 | 0% [0, 0] | 4 → 4 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 1/1 | -36.3% [-36.3, -36.3] | 6 → 4 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 1/1 | -43.2% [-43.2, -43.2] | 5 → 3 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 1/1 | -53.4% [-53.4, -53.4] | 6 → 3 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 1/1 | -0.5% [-0.5, -0.5] | 5 → 5 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 1/1 | +27.1% [27.1, 27.1] | 4 → 5 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| gemini-3.8-flash-medium | path_discovery | 1/1 | -25.9% [-25.9, -25.9] | 8 → 6 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 1/1 | -15.2% [-15.2, -15.2] | 7 → 6 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 1/1 | -45% [-45, -45] | 7 → 4 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 1/1 | +88.2% [88.2, 88.2] | 4 → 7 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 1/1 | +1.3% [1.3, 1.3] | 5 → 5 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/1 | -20.3% [-20.3, -20.3] | 5 → 4 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/1 | -27.8% [-27.8, -27.8] | 5 → 4 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 1/1 | +0.1% [0.1, 0.1] | 6 → 6 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 1/1 | -41.9% [-41.9, -41.9] | 10 → 6 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 1/1 | -28.6% [-28.6, -28.6] | 6 → 4 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 1/1 | -40.8% [-40.8, -40.8] | 6 → 4 | 0 → 0 |

</details>

## `anchored-edits` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 22/22 | 23418 | 28605 | 3.29 | 98% | 0 |
| gemini-3.8-flash-medium | 22/22 | 23084 | 31157 | 5.19 | 1% | 0 |
| gpt-6.1-sol | 22/22 | 23060 | 24103 | 4.95 | 66% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | multi_site_edit | 2 | v0.2.0+99d94e7 | empty final reply |
| gemini-3.8-flash-medium | ordinary_edit | 0 | anchored-edits-version-guard | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 1 | v0.2.0+99d94e7 | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 2 | v0.2.0+99d94e7 | empty final reply |
| gpt-6.1-sol | multi_file_edit | 0 | v0.2.0+99d94e7 | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | v0.2.0+99d94e7 | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 0 | v0.2.0+99d94e7 | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | v0.2.0+99d94e7 | wall_clock_limit |
