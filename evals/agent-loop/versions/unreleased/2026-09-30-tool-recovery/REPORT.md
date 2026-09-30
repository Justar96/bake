# 99d94e7 tool recovery and terminal startup

Candidate `v0.2.0+99d94e7`, compared with `v0.2.0` in the same run.
Models: claude-sonnet-5-5, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: duplicate_recovery, no_tools, ordinary_edit, path_discovery, stale_edit. Trials: 3. Started: unknown.

Commit 99d94e7 (fix: improve tool recovery and terminal startup) against v0.2.0. Its longer tool guidance made every first request 1,940 bytes larger.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-medium vs v0.2.0: total tokens 8.8% [3.3, 14.7]

## Against `v0.2.0`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 12/12 | +6.6% [-1.4, 15.4] | -4.9% [-13.8, 2.1] | 53 → 53 | 15 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | 11/12 | +8.8% [3.3, 14.7] | +9.1% [2.8, 15.8] | 64 → 65 | 11 → 11 | 0 → 1 |
| gpt-6.1-sol | 11/12 | -4.4% [-12.4, 4.4] | -2.4% [-26, 31.2] | 76 → 68 | 21 → 15 | 1 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | duplicate_recovery | 3/3 | +15% [7.2, 34.3] | 14 → 15 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +6.9% [-20.6, 43.8] | 10 → 10 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -1% [-15, 6.9] | 14 → 13 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +5.4% [2.8, 7.1] | 15 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | duplicate_recovery | 3/3 | +7.5% [2.4, 11.8] | 21 → 21 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +7.1% [-13.7, 33.1] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +6.7% [6.4, 7.1] | 18 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 2/3 | +16.6% [7.1, 26.1] | 12 → 13 | 0 → 1 |
| gpt-6.1-sol | duplicate_recovery | 3/3 | +6.6% [2.7, 9] | 21 → 21 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +1.5% [-19, 22.2] | 17 → 16 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -12.7% [-19.7, -1.9] | 24 → 19 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -13.8% [-25.4, 1.9] | 14 → 12 | 1 → 1 |

</details>

## `v0.2.0+99d94e7` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 15/15 | 25080 | 41937 | 4.42 | 97% | 14 |
| gemini-3.8-flash-medium | 14/15 | 24746 | 38161 | 5.91 | 1% | 12 |
| gpt-6.1-sol | 14/15 | 24722 | 32470 | 6.18 | 68% | 15 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | stale_edit | 2 | v0.2.0+99d94e7 | empty final reply |
| gpt-6.1-sol | stale_edit | 1 | v0.2.0 | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 1 | v0.2.0+99d94e7 | wall_clock_limit |
