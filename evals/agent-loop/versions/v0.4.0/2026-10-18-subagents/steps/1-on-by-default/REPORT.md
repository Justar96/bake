# Subagent tools on by default, standard suite

Candidate `subagents-default+d00ef91be1` at `d00ef91be1`, compared with `develop+c9e8065e9c` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-18T09:39:02.879Z.

The candidate shipped the subagent tools in every session. Every sample of both arms succeeded and requests did not change; total tokens rose 12 to 19% on every model, the cost of about 1.7 KB of tool definitions (toolSchemaChars 4.1-4.3k to 5.8-6.0k) on these short tasks. The gemini-3.8-flash-high tool-error rise (1 to 3) is `git` run outside a repository, a bash exit the native arm counts as an error, unrelated to the change. This cost is why D38 puts the tools behind the subagents setting; this step is superseded by the record above it.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs develop+c9e8065e9c: total tokens 19.4% [16.5, 23.9]
- claude-sonnet-5-5 vs develop+c9e8065e9c: total tokens 18.9% [18.3, 19.6]
- deepseek-flash vs develop+c9e8065e9c: total tokens 19.2% [9.9, 30]
- deepseek-v4-pro vs develop+c9e8065e9c: total tokens 15.9% [7.7, 24.9]
- gemini-3.8-flash-high vs develop+c9e8065e9c: total tokens 16% [7.5, 26.2]
- gemini-3.8-flash-high vs develop+c9e8065e9c: tool errors 1 -> 3
- gemini-4-argon-high vs develop+c9e8065e9c: total tokens 15.8% [11.2, 19.7]
- gpt-6.1-sol vs develop+c9e8065e9c: total tokens 11.6% [6.5, 16.3]

## Against `develop+c9e8065e9c`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +19.4% [16.5, 23.9] | -1.9% [-4.8, 0.9] | 66 → 67 (+1.5% [0, 4.7]) | 84 → 86 | 85% → 88% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +18.9% [18.3, 19.6] | -0.7% [-2.1, 1.4] | 68 → 68 (0% [0, 0]) | 80 → 81 | 87% → 89% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +19.2% [9.9, 30] | +23.6% [19.4, 28.2] | 87 → 88 (+1.1% [-5.7, 9.8]) | 84 → 88 | 76% → 75% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +15.9% [7.7, 24.9] | +19.5% [14.4, 24.7] | 97 → 96 (-1% [-6.9, 5.4]) | 105 → 98 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +16% [7.5, 26.2] | +18.2% [9, 29.9] | 69 → 70 (+1.4% [-5.1, 9.4]) | 103 → 103 | 0% → 0% | 1 → 3 | 0 → 0 |
| gemini-4-argon-high | 21/21 | +15.8% [11.2, 19.7] | +16.7% [11.9, 20.7] | 66 → 66 (0% [0, 0]) | 87 → 88 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/21 | +11.6% [6.5, 16.3] | +9.6% [-5.5, 26.3] | 103 → 101 (-1.9% [-5.8, 1.9]) | 86 → 83 | 59% → 60% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +16.4% [16, 16.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +18.5% [17.8, 18.9] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +17.7% [17.5, 18.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +15.7% [9.1, 23.2] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +31.7% [18.1, 57.2] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +19.1% [18.8, 19.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +18.7% [18.5, 19.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +18% [17.9, 18.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +18.8% [18.8, 18.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +18.3% [18, 18.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +18.7% [17.8, 20.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +18.1% [17.8, 18.7] | 11 → 11 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +19% [18.9, 19.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +21.7% [19.6, 25.7] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +15% [-19.1, 60.3] | 11 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +27.4% [-12.2, 58.2] | 10 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +17.9% [14.4, 21.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +22% [11.8, 40.8] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +37.5% [14, 108] | 13 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +5.7% [-13.1, 17.1] | 14 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +9.7% [-13.2, 26] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +6.3% [-10.7, 46.1] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +14.5% [-14.4, 49.6] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +15.4% [14.8, 16.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +16.8% [-2.4, 40.6] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +28.9% [14.9, 53.2] | 16 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +13.6% [-9.4, 42.1] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +11.8% [8.8, 15.1] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +44.9% [11.3, 102.1] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +18.2% [15.6, 21.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +13.8% [1, 37] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +14.4% [8.4, 24.1] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +14.1% [4.6, 27.3] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +22.6% [11, 34.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -9.5% [-21.3, 17.7] | 11 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_file_edit | 3/3 | +18.6% [18.2, 19] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_site_edit | 3/3 | +22.2% [21, 24.7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | ordinary_edit | 3/3 | +19.7% [19.2, 20.2] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | path_discovery | 3/3 | +2.7% [-5.7, 15.1] | 12 → 12 | 0 → 0 |
| gemini-4-argon-high | shell_then_edit | 3/3 | +17.7% [15.8, 20.8] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | stale_edit | 3/3 | +19.2% [16.6, 23.7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | unprompted_edit | 3/3 | +20.3% [20.1, 20.4] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +22% [14.2, 31.9] | 18 → 19 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +5.4% [-9.8, 15.1] | 13 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +13.4% [10.8, 14.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +12.1% [2.9, 21.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +15% [15, 15.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +15.5% [15.1, 15.7] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -4.7% [-14, 9.4] | 15 → 13 | 0 → 0 |

</details>

## `subagents-default+d00ef91be1` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 8659 | 12922 | 3.19 | 88% | 0 |
| claude-sonnet-5-5 | 24/24 | 8661 | 12774 | 3.24 | 89% | 0 |
| deepseek-flash | 24/24 | 8519 | 11879 | 4.19 | 75% | 0 |
| deepseek-v4-pro | 24/24 | 8520 | 13433 | 4.57 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 8461 | 10092 | 3.33 | 0% | 3 |
| gemini-4-argon-high | 24/24 | 8461 | 7500 | 3.14 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 8453 | 10086 | 4.81 | 60% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 22 | 18 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 30 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 4 | 0 | 2 | 21/21 | 0 |
| gemini-4-argon-high | 21 | 0 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 35 | 21 | 1 | 21/21 | 0 |
