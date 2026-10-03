# Strict tool schemas for GPT on the Responses wire

Candidate `strict-tools` at `496d1b68ac`, compared with `pre-guard` and `runaway-guard` in the same run.
Models: gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 5. Started: 2026-10-03T16:23:39.199Z.

Third arm of the runaway-guard run, GPT 6.1 Sol only, 5 trials: guard plus strictTools on the cliproxyapi route, with compat.supportsStrictMode set, so every tool is sent with strict true and optional fields become nullable; the adapter drops the introduced nulls before recording. 40 of 40 samples succeeded, with no runaway in any of them, against 11 failures in the base and 1 in the guard arm. The flagged regression against the guard arm (total tokens +6.3%, interval 1.3 to 11.2) is the cost of the larger strict schemas: the first request grows from about 11.7k to 13.8k bytes and the cache-read share falls from 56% to 45%. strictTools stays off by default; turning it on for GPT routes trades about 6% more tokens for no runaways.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs runaway-guard: total tokens 6.3% [1.3, 11.2]

## Against `pre-guard`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| gpt-6.1-sol | 24/35 | +1% [-4.6, 7] | +17.3% [-2.5, 41.7] | 109 → 108 | 87 → 87 | 54% → 47% | 0 → 0 | 11 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 5/5 | -4.4% [-12.7, 8] | 25 → 24 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 4/5 | -6.8% [-10.9, 0.6] | 16 → 16 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 2/5 | +4.4% [-10.5, 21.7] | 8 → 8 | 3 → 0 |
| gpt-6.1-sol | path_discovery | 4/5 | +10% [-7.6, 22.7] | 21 → 21 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/5 | +3.3% [-11.7, 21] | 15 → 15 | 2 → 0 |
| gpt-6.1-sol | stale_edit | 3/5 | +10.8% [5.3, 21.6] | 12 → 12 | 2 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/5 | -7.4% [-13.7, 4.4] | 12 → 12 | 2 → 0 |

</details>

## Against `runaway-guard`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| gpt-6.1-sol | 29/35 | +6.3% [1.3, 11.2] | +35.9% [9.1, 69.6] | 131 → 130 | 107 → 105 | 56% → 45% | 0 → 0 | 1 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 5/5 | +5.1% [-3, 13.9] | 24 → 24 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 5/5 | +1.1% [-8.2, 12.4] | 20 → 20 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/5 | +21.8% [21.3, 22.2] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 4/5 | +5.5% [-10.3, 25.1] | 22 → 21 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 5/5 | +10.4% [3.3, 18] | 25 → 25 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 4/5 | +8.6% [-3.5, 22.7] | 16 → 16 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/5 | -6.7% [-13.3, 4.5] | 12 → 12 | 0 → 0 |

</details>

## `strict-tools` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| gpt-6.1-sol | 40/40 | 13838 | 11909 | 4.43 | 45% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 2 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 0 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | runaway-guard | exit 1 |
| gpt-6.1-sol | stale_edit | 2 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 1 | pre-guard | exit 1 |
| gpt-6.1-sol | unprompted_edit | 4 | pre-guard | wall_clock_limit |
