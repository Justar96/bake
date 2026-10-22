# Native bake command rename

Candidate `develop@3cfa3afac8` at `3cfa3afac8`, compared with `develop@fcfab51739` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-20T18:36:59.601Z.

Paired clean native builds: develop@fcfab51739 before the command rename and develop@3cfa3afac8 after it, with locked dependencies. The standard eight scenarios ran on all eight standard and extended model routes, three trials per arm: 384 successful samples, complete usage accounting, no invalid samples. The prompt names each arm’s own executable in its documentation command.

The one flagged regression is DeepSeek v4 Pro tool errors, 0 to 1. In shell_then_edit trial 1, the candidate ran `ls -la && ls -la src scripts 2>/dev/null; cat package.json 2>/dev/null`; the optional package.json is absent, so the shell returned 1. The agent then ran the stamping script, edited the source, and passed the fixture check. This is accepted as model-run variation in an exploratory shell call: all 192 paired system prompts match after normalizing the workspace and native executable paths, tool-schema lengths match in every pair, and all eight captured initial requests match after those substitutions and normalization of per-session prompt_cache_key values. No token, request-count, or task-failure regression was flagged.

Raw samples and the surface audit remain in ignored .preflight/. The subsequent writer-lock release fix changes persisted session teardown; these native eval arms run in memory and the fix is validated with native tests separately.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-v4-pro vs develop@fcfab51739: tool errors 0 -> 1

## Against `develop@fcfab51739`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 21/21 | -0.3% [-4.5, 4.3] | -3.5% [-7.4, -0.1] | 80 → 81 (+1.3% [-2.6, 5.6]) | 78 → 74 | 89% → 90% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 21/21 | -0.5% [-8, 7.1] | -2.9% [-6, 0.1] | 88 → 88 (0% [-7.6, 7.4]) | 83 → 81 | 92% → 92% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -2.6% [-8.1, 4.2] | -4.4% [-7.4, -1.2] | 91 → 93 (+2.2% [-3.3, 8.7]) | 86 → 92 | 77% → 78% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -3.4% [-8.2, 0.9] | -1.6% [-4, 0.5] | 96 → 94 (-2.1% [-6.1, 2.1]) | 100 → 99 | 81% → 80% | 0 → 1 | 0 → 0 |
| gemini-3-flash | 21/21 | -16.3% [-32.1, 3.6] | -14.2% [-26.1, -0.4] | 146 → 127 (-13% [-25.2, 0.8]) | 134 → 122 | 15% → 12% | 20 → 19 | 0 → 0 |
| gemini-3.5-flash-lite | 21/21 | -6.8% [-21.5, 7.4] | -6.7% [-20, 6.6] | 144 → 137 (-4.9% [-17.9, 7.5]) | 128 → 126 | 12% → 12% | 21 → 20 | 0 → 0 |
| gpt-6-sol | 21/21 | -6.4% [-12.6, -0.1] | -43.7% [-57.7, -26.1] | 83 → 79 (-4.8% [-10.6, 1.3]) | 68 → 62 | 58% → 75% | 1 → 0 | 0 → 0 |
| kimi-k2.7-code | 21/21 | +2.5% [-4.7, 10.3] | +3.5% [-5.7, 14.2] | 92 → 93 (+1.1% [-5.4, 7.9]) | 94 → 94 | 89% → 89% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | multi_file_edit | 3/3 | -0.6% [-2.6, 1.8] | 9 → 9 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | -9.5% [-26.8, 2.1] | 11 → 10 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | -1% [-2.3, 1.1] | 10 → 10 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | -2.4% [-6.1, 0] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | -2.7% [-7.4, 0.2] | 15 → 15 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | +5.7% [-1.1, 23.9] | 11 → 12 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | +12% [-1.5, 35.5] | 9 → 10 | 0 → 0 |
| claude-sonnet-5 | multi_file_edit | 3/3 | +8% [-1.2, 32] | 11 → 12 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | -0.9% [-1.2, -0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | -9.4% [-29.5, 3.8] | 12 → 11 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | +7.2% [-3.1, 29.3] | 14 → 15 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | -11.5% [-31.6, 16.1] | 17 → 15 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | +7.3% [-1, 23.5] | 12 → 13 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | -1% [-25.4, 31.3] | 10 → 10 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +7.2% [-5.4, 37.5] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -8.7% [-21.7, 1] | 13 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.7% [-3.3, -0.8] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -7.2% [-14.1, 5.4] | 15 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -4.7% [-14.6, 0.9] | 15 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +10.9% [-6.9, 67.1] | 13 → 15 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -11.2% [-27.5, -2.2] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +6.5% [-0.6, 19] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -9.9% [-22.1, 1.8] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.1% [-1.4, 0.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +1.2% [0, 2.5] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -8.8% [-23.9, 4.7] | 18 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -10.2% [-22.7, -2] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | 0% [-3.7, 4.6] | 12 → 12 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | -22.9% [-48.9, 35.1] | 36 → 30 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | -15.7% [-39.2, -2.9] | 15 → 13 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | +8.2% [-38.3, 115.9] | 17 → 19 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | +2.4% [-43.6, 30.9] | 20 → 19 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | -46.1% [-68, -9.3] | 26 → 15 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | -13.9% [-55.1, 56] | 15 → 13 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | +5.8% [-20.7, 61] | 17 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | +18.4% [3.4, 41.9] | 26 → 32 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | -11.3% [-40.4, 16.7] | 16 → 14 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | -18.1% [-51.4, 27.9] | 19 → 17 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | +14.1% [-0.2, 28.6] | 23 → 25 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | -31.7% [-62.9, -2.1] | 22 → 16 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | +15.3% [-17.1, 73.3] | 13 → 15 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | -39% [-49.6, -31.1] | 25 → 18 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | +1.3% [-0.7, 5.5] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | -22.2% [-30.8, -1] | 11 → 9 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | +8.6% [-0.4, 28.8] | 11 → 12 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | -15.2% [-24.6, -0.6] | 14 → 12 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | -12.2% [-19.1, -0.5] | 18 → 16 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | +7.4% [-1.9, 31.1] | 11 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | -1.9% [-5.4, 1.2] | 9 → 9 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | -15% [-28.7, 8.5] | 13 → 11 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | +7.7% [-18.6, 28.3] | 13 → 14 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | +0.5% [-1.9, 1.7] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | -1.3% [-2.7, 1] | 15 → 15 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | +4.7% [-6.3, 34.7] | 15 → 15 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | +7.8% [-1.6, 26.1] | 12 → 13 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | +17.3% [-2.3, 48.9] | 12 → 13 | 0 → 0 |

</details>

## `develop@3cfa3afac8` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | 9785 | 18259 | 3.86 | 90% | 0 |
| claude-sonnet-5 | 24/24 | 9787 | 19061 | 4.19 | 92% | 0 |
| deepseek-flash | 24/24 | 9661 | 14117 | 4.43 | 78% | 0 |
| deepseek-v4-pro | 24/24 | 9662 | 14409 | 4.48 | 80% | 1 |
| gemini-3-flash | 24/24 | 9611 | 23427 | 6.05 | 12% | 19 |
| gemini-3.5-flash-lite | 24/24 | 9618 | 25507 | 6.52 | 12% | 20 |
| gpt-6-sol | 24/24 | 9606 | 9601 | 3.76 | 75% | 0 |
| kimi-k2.7-code | 24/24 | 9656 | 12962 | 4.43 | 89% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 21 | 15 | 11 | 0 | 15/15 | 0 |
| claude-sonnet-5 | 21 | 22 | 17 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 27 | 20 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 28 | 21 | 0 | 21/21 | 0 |
| gemini-3-flash | 21 | 61 | 13 | 1 | 21/21 | 0 |
| gemini-3.5-flash-lite | 21 | 71 | 15 | 1 | 21/21 | 0 |
| gpt-6-sol | 21 | 13 | 8 | 0 | 8/8 | 0 |
| kimi-k2.7-code | 21 | 27 | 20 | 0 | 21/21 | 0 |
