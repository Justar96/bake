# Step 2: on-demand tool details and plan mode removal

Candidate `tool-details`, compared with `anchored-edits` in the same run.
Models: claude-sonnet-5-5, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: unknown.

Intermediate step measured on uncommitted trees: step 1 against step 1 plus on-demand tool details, shorter subagent descriptions, and plan mode removed. workflow_script costs more by design, because the model reads tool_help before writing a script.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-medium vs anchored-edits: failures 0 -> 3
- gpt-6.1-sol vs anchored-edits: tool errors 0 -> 1

## Against `anchored-edits`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 24/24 | -6% [-11, -0.3] | +10% [-1.1, 24] | 91 → 93 | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/24 | -10.3% [-18.8, -0.4] | -9.9% [-18.2, 0.1] | 114 → 113 | 0 → 0 | 0 → 3 |
| gpt-6.1-sol | 23/24 | -4.4% [-14.2, 9.7] | +1% [-25.4, 40.8] | 120 → 126 | 0 → 1 | 1 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -9.3% [-10.8, -7.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -9.3% [-9.3, -9.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -7.4% [-9.1, -6.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -9.3% [-9.3, -9.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -9.3% [-9.3, -9.3] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -18.4% [-32.1, -9.3] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -9.1% [-9.5, -8.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | workflow_script | 3/3 | +27.9% [27.6, 28] | 21 → 24 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -8.7% [-9.1, -8.3] | 15 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -10.7% [-12.1, -10] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -2.9% [-9.8, 12.3] | 13 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -25.9% [-39.7, 15.2] | 19 → 16 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 2/3 | -16.6% [-23.4, -9.8] | 14 → 13 | 0 → 1 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -14.5% [-40.2, 8.5] | 16 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 2/3 | -9.6% [-9.8, -9.5] | 9 → 9 | 0 → 1 |
| gemini-3.8-flash-medium | workflow_script | 2/3 | +32.2% [-2.4, 88.8] | 16 → 19 | 0 → 1 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +1.8% [-31.4, 23.2] | 14 → 16 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -11.5% [-11.6, -11.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -13.9% [-19.5, -11.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -15.3% [-28.1, -7.1] | 18 → 17 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -11% [-11.2, -10.9] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -8.8% [-11.3, -3.6] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -16.4% [-34.6, 21.1] | 15 → 14 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | +79.9% [21.2, 138] | 16 → 22 | 1 → 0 |

</details>

## `tool-details` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 26/26 | 20941 | 26878 | 3.88 | 98% | 0 |
| gemini-3.8-flash-medium | 23/26 | 20607 | 27102 | 5.38 | 0% | 0 |
| gpt-6.1-sol | 26/26 | 20583 | 22499 | 5.63 | 61% | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | shell_then_edit | 1 | tool-details | empty final reply |
| gemini-3.8-flash-medium | unprompted_edit | 1 | tool-details | empty final reply |
| gemini-3.8-flash-medium | workflow_script | 0 | tool-details | empty final reply |
| gpt-6.1-sol | workflow_script | 1 | anchored-edits | wall_clock_limit |
