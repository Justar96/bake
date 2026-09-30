# Harness efficiency: fewer round trips, on-demand tool details, no plan mode

Candidate `v0.2.0+d8d0b96` at `27ddbb529e`, compared with `v0.2.0+decc7e7` in the same run.
Models: claude-sonnet-5-5, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-09-30T17:56:25.142Z.

The PR commit against its base, develop at decc7e7, both clean worktrees. It was measured as 27ddbb5 and rebased unchanged onto 20a890a, a lockfile-only commit, as d8d0b96; summary.json keeps the measured commit. The PR bundles content-anchored edits, lenient arguments, error-side recovery hints, shorter tool descriptions, on-demand tool details (tool_help), and the removal of plan mode; steps/ holds the two intermediate measurements. workflow_script costs more by design: the model reads the script reference with tool_help before writing a script, one extra request.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.2.0+decc7e7`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 24/24 | -33.1% [-41.3, -23.1] | -28.3% [-41.5, -11.3] | 115 → 93 | 16 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 20/24 | -24.5% [-33.4, -15.6] | -22.5% [-31.1, -14.1] | 125 → 111 | 3 → 0 | 2 → 2 |
| gpt-6.1-sol | 19/24 | -36.7% [-43.4, -28.4] | -37.5% [-53, -17] | 122 → 99 | 7 → 0 | 4 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -30.2% [-35.9, -15.7] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -29.7% [-35.5, -14.2] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -23.4% [-35.9, -13.8] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -13.6% [-13.7, -13.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -57.5% [-59.3, -53.5] | 23 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -50.5% [-51.2, -49.1] | 15 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -29.8% [-36, -13.3] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | workflow_script | 3/3 | +9% [-9.6, 21.4] | 22 → 24 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -27.8% [-37.8, -14.3] | 18 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 1/3 | -29.4% [-29.4, -29.4] | 5 → 4 | 1 → 1 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -8.4% [-14.5, 6.4] | 14 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -18.5% [-26.6, -13.7] | 19 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 2/3 | -36.8% [-56.1, 0.7] | 17 → 13 | 1 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -47.1% [-52.2, -44] | 19 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -14.6% [-14.6, -14.5] | 15 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | workflow_script | 2/3 | -0.1% [-10, 9.8] | 18 → 19 | 0 → 1 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -45.5% [-51.2, -35.6] | 23 → 16 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -36.3% [-37.9, -34.7] | 10 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 2/3 | -35.5% [-41.1, -30.9] | 11 → 9 | 1 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | -27.6% [-35.1, -20.2] | 12 → 11 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -41% [-57, -29.6] | 21 → 16 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -52% [-58.2, -46] | 19 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -21.4% [-25.1, -17.7] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | workflow_script | 2/3 | -2.3% [-27.2, 35.7] | 18 → 19 | 0 → 1 |

</details>

## `v0.2.0+d8d0b96` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 27/27 | 20941 | 26808 | 3.88 | 98% | 0 |
| gemini-3.8-flash-medium | 25/27 | 20607 | 28124 | 5.55 | 0% | 0 |
| gpt-6.1-sol | 25/27 | 20583 | 20111 | 5.14 | 70% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | multi_site_edit | 0 | v0.2.0+decc7e7 | empty final reply |
| gemini-3.8-flash-medium | multi_site_edit | 2 | v0.2.0+d8d0b96 | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 1 | v0.2.0+decc7e7 | empty final reply |
| gemini-3.8-flash-medium | workflow_script | 2 | v0.2.0+d8d0b96 | empty final reply |
| gpt-6.1-sol | multi_site_edit | 2 | v0.2.0+decc7e7 | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | v0.2.0+decc7e7 | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 0 | v0.2.0+decc7e7 | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 2 | v0.2.0+decc7e7 | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 2 | v0.2.0+d8d0b96 | wall_clock_limit |
| gpt-6.1-sol | workflow_script | 0 | v0.2.0+d8d0b96 | wall_clock_limit |
