# Step 1: round-trip guidance alone

Candidate `guidance-only` at `496d1b68ac`, compared with `v0.3.3+496d1b68ac-dirty` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-03T07:34:47.576Z.

Attribution step from a three-arm paired run (base, guidance, candidate). GPT failures in this run are gateway streams that returned 200 and then sent no events until the 180 s cap, on both sides of the change. Guidance is the persona paragraph only, without the description trim or the todo_write note.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../../../README.md#regressions).

## Against `v0.3.3+496d1b68ac-dirty`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -9.6% [-16.3, -3.2] | +10.5% [-1.4, 25.8] | 100 → 90 | 84 → 90 | 97% → 96% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -4% [-9.8, 2.5] | +29.2% [-6.9, 99.7] | 124 → 120 | 115 → 115 | 97% → 96% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 24/24 | -43.3% [-49.7, -36.9] | -45.1% [-51.7, -38.8] | 165 → 95 | 133 → 113 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/24 | +5.8% [-0.7, 13] | +10.1% [-21.4, 55.8] | 106 → 107 | 81 → 86 | 67% → 66% | 0 → 0 | 3 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +3.2% [3, 3.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -18.4% [-25.8, 1.1] | 11 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +1.1% [0.8, 1.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +4.3% [4.1, 4.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -34.6% [-39.3, -23] | 14 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -24% [-24.1, -23.9] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +1.6% [1.1, 2.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 3/3 | +1.2% [0.6, 2] | 24 → 24 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -7% [-20.5, 1.8] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +8.9% [-1.4, 24.1] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +1.1% [0.8, 1.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -4% [-25.6, 20.1] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -9.4% [-24.4, 23.6] | 18 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -7.4% [-20.3, 0.8] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +2.4% [-0.5, 3.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | -9.4% [-17.8, -2.8] | 28 → 27 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -40.8% [-48.7, -35.5] | 19 → 11 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -38.5% [-38.6, -38.4] | 15 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -29.6% [-38.2, -12.7] | 15 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -38.9% [-43.7, -33.1] | 20 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -58.2% [-71.9, -36.5] | 27 → 10 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -54.7% [-56.7, -49.9] | 20 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -51% [-56.2, -36.6] | 19 → 9 | 0 → 0 |
| gemini-3.8-flash-high | workflow_script | 3/3 | -23.4% [-31.2, -10.6] | 30 → 25 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -4.1% [-21.4, 10.4] | 14 → 13 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +1.5% [-4.1, 7.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +11.5% [11.5, 11.5] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +11% [0.5, 37.7] | 17 → 18 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +3.2% [-7.9, 11.3] | 17 → 16 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +1.4% [-7.9, 11.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -0.5% [-7.3, 14.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | +31.7% [8.7, 49.1] | 17 → 19 | 1 → 0 |

</details>

## `guidance-only` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 21221 | 27713 | 3.75 | 96% | 0 |
| deepseek-v4-pro | 27/27 | 21081 | 27102 | 5 | 96% | 0 |
| gemini-3.8-flash-high | 27/27 | 20885 | 19873 | 3.96 | 0% | 0 |
| gpt-6.1-sol | 27/27 | 20867 | 20322 | 5.17 | 64% | 0 |

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
