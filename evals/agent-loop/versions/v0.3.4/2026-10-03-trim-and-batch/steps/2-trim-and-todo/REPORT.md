# Step 2: description trim and todo_write note

Candidate `trim-and-batch` at `496d1b68ac`, compared with `guidance-only` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-03T07:34:47.576Z.

Attribution step from a three-arm paired run (base, guidance, candidate); this step adds the shorter tool descriptions and the todo_write note on top of the guidance. The flagged GPT failures (0 -> 4) all share one signature: the last request returned 200 and then delivered no usage event before the 180 s cap, after earlier requests in the same sample completed normally. The same signature hit the base arm 3 times in this run, and across the three GPT runs of this change it hit base 4 of 81 samples and candidate 7 of 81, guidance 0 of 27. The proxy records usage events but not stream timing, so it cannot tell a gateway stall from a model response that streams nothing for three minutes; the failures are reported as unresolved transport stalls, not as a confirmed effect of the change.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs guidance-only: failures 0 -> 4

## Against `guidance-only`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -22.4% [-22.6, -22.1] | +5.8% [-2.6, 13.9] | 90 → 90 | 90 → 89 | 96% → 95% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -24.8% [-28.4, -21] | -28.9% [-51.2, -4.8] | 120 → 118 | 115 → 110 | 96% → 96% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 24/24 | -22.1% [-27.1, -16.4] | -24.2% [-28.9, -18.7] | 95 → 96 | 113 → 113 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 20/24 | -31.5% [-38, -25.1] | -10.6% [-31.7, 16.8] | 103 → 99 | 84 → 84 | 67% → 56% | 0 → 0 | 0 → 4 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -22.4% [-22.6, -22.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -22.8% [-22.8, -22.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -22% [-22.2, -21.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -22% [-22.2, -21.9] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -22.8% [-23.5, -22.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -22.8% [-22.9, -22.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -23% [-23.1, -23] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 3/3 | -21.5% [-22.1, -20.7] | 24 → 24 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -23.4% [-23.7, -23.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -29.8% [-38.7, -24] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -24% [-24.2, -23.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -27.9% [-36.6, -18.6] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -30.9% [-41.1, -23.3] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -24.2% [-24.5, -24] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -24.4% [-24.9, -24.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | -14.4% [-22.9, 1] | 27 → 28 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -8.6% [-22.8, 29.3] | 11 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -25.3% [-25.4, -25.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -26.8% [-47.1, 0.3] | 10 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -23% [-25.5, -20] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -30% [-40.7, -22.2] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -23.8% [-25.2, -22.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -27% [-27.2, -26.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | workflow_script | 3/3 | -16.2% [-21.9, -3.5] | 25 → 25 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -15.6% [-28.3, -7.4] | 13 → 15 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -25.3% [-25.4, -25.1] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 2/3 | -33.3% [-41.9, -21.9] | 9 → 8 | 0 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | -37.4% [-40.8, -33.9] | 18 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -31.8% [-41.1, -23.4] | 16 → 16 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -34% [-36.4, -31.2] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -27.6% [-31.1, -21.3] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | -43.8% [-63, -8.9] | 19 → 17 | 0 → 1 |

</details>

## `trim-and-batch` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 14624 | 21519 | 3.75 | 95% | 0 |
| deepseek-v4-pro | 27/27 | 14484 | 20381 | 4.92 | 96% | 0 |
| gemini-3.8-flash-high | 27/27 | 14288 | 15480 | 4 | 0% | 0 |
| gpt-6.1-sol | 23/27 | 14270 | 14155 | 4.95 | 56% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 0 | trim-and-batch | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | v0.3.3+496d1b68ac-dirty | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 2 | v0.3.3+496d1b68ac-dirty | exit 1 |
| gpt-6.1-sol | ordinary_edit | 2 | trim-and-batch | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | trim-and-batch | wall_clock_limit |
| gpt-6.1-sol | workflow_script | 0 | trim-and-batch | wall_clock_limit |
| gpt-6.1-sol | workflow_script | 2 | v0.3.3+496d1b68ac-dirty | wall_clock_limit |
