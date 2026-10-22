# Opt-in session reminders (D39): the setting off and on

Candidate `schedules-on+c8f7aa9223` at `c8f7aa9223`, compared with `schedules-off+c8f7aa9223` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, reminder, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-18T11:52:37.815Z.

Both arms are bake-rs at c8f7aa9223; the on arm sets `schedules: true` through EVAL_SETTINGS_ON. The setting is off by default and the default model surface is unchanged from develop (the first-request fixtures are identical), so this record measures what turning it on costs. On the opt-in reminder scenario every on sample called schedule_create once and wrote the returned id, 21/21; without the setting there is no schedule tool, so every off sample fails the predicate, which requires a schedule_create call. Those failures are the point of the comparison, not regressions. The flagged total-token rises, 17 to 27% on every model, come from about 1.9 KB of tool definitions in every request (toolSchemaChars 4.1-4.3k to 6.0-6.2k), with no change in standard-suite failures; that is the cost of the opt-in, and the reason it stays off by default. The gemini-3.8-flash-high tool errors, 0 to 2, are a failing node test.cjs before the fix and git run outside a repository in path_discovery, unrelated to the schedule tools. The first run stopped at the 5M-token limit after 360 of 378 samples, since off reminder samples search at length for a tool they lack; deepseek-v4-pro's trial 2 comes from a second run of the same binary (sha256 9aa4aee7e3c77479…, built at 41f1388d9c, which changes only scripts/release), keeping only its trial-2 samples.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs schedules-off+c8f7aa9223: total tokens 24.4% [20.2, 30.3]
- claude-sonnet-5-5 vs schedules-off+c8f7aa9223: total tokens 27.4% [21.8, 33.9]
- deepseek-flash vs schedules-off+c8f7aa9223: total tokens 16.8% [8.2, 25.6]
- deepseek-v4-pro vs schedules-off+c8f7aa9223: total tokens 21.6% [14.9, 28.2]
- gemini-3.8-flash-high vs schedules-off+c8f7aa9223: total tokens 25.1% [10.6, 39.8]
- gemini-3.8-flash-high vs schedules-off+c8f7aa9223: tool errors 0 -> 2
- gemini-4-argon-high vs schedules-off+c8f7aa9223: total tokens 23% [19.7, 26.9]
- gpt-6.1-sol vs schedules-off+c8f7aa9223: total tokens 17.1% [12.6, 21.9]

## Against `schedules-off+c8f7aa9223`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/24 | +24.4% [20.2, 30.3] | -1.2% [-4.6, 1.6] | 66 → 68 (+3% [0, 7.7]) | 87 → 87 | 84% → 87% | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | 21/24 | +27.4% [21.8, 33.9] | -0.8% [-3.5, 1.7] | 66 → 69 (+4.5% [0, 9.4]) | 81 → 82 | 85% → 89% | 0 → 0 | 3 → 0 |
| deepseek-flash | 21/24 | +16.8% [8.2, 25.6] | +19.2% [15.4, 23] | 92 → 92 (0% [-6.1, 6.5]) | 99 → 88 | 76% → 76% | 1 → 0 | 3 → 0 |
| deepseek-v4-pro | 21/24 | +21.6% [14.9, 28.2] | +22% [18, 26.6] | 95 → 97 (+2.1% [-3.1, 7.1]) | 102 → 104 | 81% → 81% | 0 → 0 | 3 → 0 |
| gemini-3.8-flash-high | 21/24 | +25.1% [10.6, 39.8] | +28.1% [12.8, 43.4] | 69 → 74 (+7.2% [-1.5, 17.1]) | 101 → 106 | 0% → 0% | 0 → 2 | 3 → 0 |
| gemini-4-argon-high | 21/24 | +23% [19.7, 26.9] | +24.3% [20.8, 28.2] | 66 → 66 (0% [0, 0]) | 87 → 87 | 0% → 0% | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | 21/24 | +17.1% [12.6, 21.9] | -8.8% [-26.6, 14.5] | 103 → 105 (+1.9% [-1.8, 5.9]) | 84 → 87 | 53% → 63% | 0 → 0 | 3 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +21.1% [20.7, 21.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +15% [14.7, 15.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +21.5% [19.8, 24] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +19.8% [18.9, 20.7] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +49.6% [23.7, 63.4] | 9 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +23.4% [23.1, 23.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +23.3% [22.4, 23.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +21.4% [21.3, 21.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +22.9% [22.8, 22.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +22.4% [22.1, 22.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +21.7% [19, 24.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +63.8% [63.4, 64.1] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +23.3% [22.9, 23.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +18.4% [16.4, 22.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +3.5% [-17, 15.1] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +18.7% [12, 22.8] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +32.3% [19.7, 54.7] | 11 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +9.3% [-2, 17.9] | 16 → 15 | 0 → 0 |
| deepseek-flash | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +13% [-16.8, 42.6] | 16 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +19.9% [-15.5, 45.2] | 14 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +30.5% [16.4, 62.4] | 11 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +39.2% [17.6, 58] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +0.8% [-11.9, 17.3] | 15 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +19.3% [15.1, 22.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +24% [15.6, 40] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +29.5% [18.3, 39.9] | 17 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +20.9% [20.4, 21.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +16.8% [12.3, 19.7] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +25.3% [-0.2, 56.8] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +16.7% [15.2, 18] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +34.4% [15.7, 69.8] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +29.5% [-25, 72.1] | 14 → 16 | 0 → 0 |
| gemini-3.8-flash-high | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +17% [2.8, 31.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +17.2% [4.8, 25.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +30.8% [-20.9, 121.4] | 10 → 11 | 0 → 0 |
| gemini-4-argon-high | multi_file_edit | 3/3 | +18.2% [17.8, 18.5] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_site_edit | 3/3 | +19.9% [18.5, 20.7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | ordinary_edit | 3/3 | +19.5% [19.4, 19.6] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | path_discovery | 3/3 | +37.1% [28.1, 45.6] | 12 → 12 | 0 → 0 |
| gemini-4-argon-high | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gemini-4-argon-high | shell_then_edit | 3/3 | +19.3% [16.2, 24.4] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | stale_edit | 3/3 | +21.2% [19.3, 24.1] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | unprompted_edit | 3/3 | +20% [19.8, 20.1] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +7.8% [-4.6, 15.4] | 19 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +26.5% [16, 47.3] | 12 → 13 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +24% [15.6, 40.7] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +20.5% [14.5, 28.2] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | reminder | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +16% [15.9, 16.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +16.4% [16.1, 16.6] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +13.5% [10.7, 19.4] | 15 → 15 | 0 → 0 |

</details>

## `schedules-on+c8f7aa9223` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 8860 | 13266 | 3.21 | 88% | 0 |
| claude-sonnet-5-5 | 27/27 | 8862 | 13040 | 3.25 | 89% | 0 |
| deepseek-flash | 27/27 | 8720 | 12087 | 4.29 | 76% | 0 |
| deepseek-v4-pro | 27/27 | 8721 | 12925 | 4.46 | 81% | 0 |
| gemini-3.8-flash-high | 27/27 | 8675 | 21549 | 3.96 | 37% | 2 |
| gemini-4-argon-high | 27/27 | 8675 | 7466 | 3.13 | 0% | 0 |
| gpt-6.1-sol | 27/27 | 8667 | 9919 | 4.75 | 61% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24 | 2 | 0 | 0 | 21/24 | 0 |
| claude-sonnet-5-5 | 24 | 3 | 0 | 0 | 21/24 | 0 |
| deepseek-flash | 24 | 28 | 20 | 1 | 21/24 | 0 |
| deepseek-v4-pro | 24 | 32 | 21 | 0 | 21/24 | 0 |
| gemini-3.8-flash-high | 24 | 20 | 0 | 1 | 21/24 | 0 |
| gemini-4-argon-high | 24 | 0 | 0 | 0 | 21/24 | 0 |
| gpt-6.1-sol | 24 | 39 | 21 | 1 | 21/24 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | reminder | 0 | schedules-off+c8f7aa9223 | validation failed |
| claude-opus-5-5 | reminder | 1 | schedules-off+c8f7aa9223 | validation failed |
| claude-opus-5-5 | reminder | 2 | schedules-off+c8f7aa9223 | validation failed |
| claude-sonnet-5-5 | reminder | 0 | schedules-off+c8f7aa9223 | validation failed |
| claude-sonnet-5-5 | reminder | 1 | schedules-off+c8f7aa9223 | validation failed |
| claude-sonnet-5-5 | reminder | 2 | schedules-off+c8f7aa9223 | validation failed |
| deepseek-flash | reminder | 0 | schedules-off+c8f7aa9223 | validation failed |
| deepseek-flash | reminder | 1 | schedules-off+c8f7aa9223 | validation failed |
| deepseek-flash | reminder | 2 | schedules-off+c8f7aa9223 | request_limit |
| deepseek-v4-pro | reminder | 0 | schedules-off+c8f7aa9223 | request_limit |
| deepseek-v4-pro | reminder | 1 | schedules-off+c8f7aa9223 | request_limit |
| deepseek-v4-pro | reminder | 2 | schedules-off+c8f7aa9223 | request_limit |
| gemini-3.8-flash-high | reminder | 0 | schedules-off+c8f7aa9223 | request_limit |
| gemini-3.8-flash-high | reminder | 1 | schedules-off+c8f7aa9223 | wall_clock_limit |
| gemini-3.8-flash-high | reminder | 2 | schedules-off+c8f7aa9223 | request_limit |
| gemini-4-argon-high | reminder | 0 | schedules-off+c8f7aa9223 | validation failed |
| gemini-4-argon-high | reminder | 1 | schedules-off+c8f7aa9223 | validation failed |
| gemini-4-argon-high | reminder | 2 | schedules-off+c8f7aa9223 | validation failed |
| gpt-6.1-sol | reminder | 0 | schedules-off+c8f7aa9223 | validation failed |
| gpt-6.1-sol | reminder | 1 | schedules-off+c8f7aa9223 | validation failed |
| gpt-6.1-sol | reminder | 2 | schedules-off+c8f7aa9223 | validation failed |
