# Code mode on by default

Candidate `codemode-default+17ddacbb4f` at `17ddacbb4f`, compared with `develop+bf32369ca6` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: fan_out, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-19T21:22:35.779Z.

Owner decision, 2026-10-20: codemode is a Bake default tool. Base is develop at bf32369ca6, where codemode is opt-in; candidate is 17ddacbb4f, which adds it to BAKE_DEFAULT_TOOL_NAMES. Both arms run their default tools, on the standard suite and fan_out, with the newest model of each gateway lab plus DeepSeek two, three trials. Every request carries about 2.1 KB more tool definitions (toolSchemaChars 4692-4860 to 6823-7004). Models that used it: claude-opus-5 (21 calls in 11 of 27 samples; requests -7.9%, tool calls 107 to 79, tokens +6.5% with an interval spanning zero), gpt-6-sol (16 calls in 9 of 27; requests -10.2%, tool calls 116 to 77, tokens -5.5%), gemini-3-flash (4 calls) and gemini-3.5-flash-lite (2 calls). claude-sonnet-5, deepseek-flash, deepseek-v4-pro, and kimi-k2.7-code never called it. Accepted regressions, all from the definition in requests the model never used it in, accepted because the owner chose the default: claude-sonnet-5 +18.9%, deepseek-flash +17.2%, deepseek-v4-pro +17.0%, kimi-k2.7-code +59.1%. Kimi rises 12-41% on the standard scenarios; its fan_out +154% is its approach, not the tool: the candidate printed every ledger through bash in fewer requests (19 to 14) where the base read one or two and summed them with a script. gemini-3-flash tool errors 24 to 26 are git and node checks run outside a repository, as in the base. The one failure, claude-sonnet-5 fan_out trial 2 in the candidate, wrote correct balances but then ran mv balances.txt ../balances.txt, assuming an earlier cd persisted between bash calls, which moved the file out of the workspace; it made no codemode call.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5 vs develop+bf32369ca6: total tokens 18.9% [12.8, 26.7]
- deepseek-flash vs develop+bf32369ca6: total tokens 17.2% [8.2, 29.9]
- deepseek-v4-pro vs develop+bf32369ca6: total tokens 17% [4.6, 29.8]
- gemini-3-flash vs develop+bf32369ca6: tool errors 24 -> 26
- kimi-k2.7-code vs develop+bf32369ca6: total tokens 59.1% [24.2, 92.7]

## Against `develop+bf32369ca6`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | +6.5% [-6.2, 19.7] | -5.9% [-13.7, 1.4] | 101 → 93 (-7.9% [-15.2, 1]) | 107 → 79 | 80% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 23/24 | +18.9% [12.8, 26.7] | +2.5% [0.2, 6] | 96 → 95 (-1% [-5.3, 3.3]) | 86 → 86 | 82% → 84% | 0 → 0 | 0 → 1 |
| deepseek-flash | 24/24 | +17.2% [8.2, 29.9] | +10.6% [6.3, 18.9] | 104 → 102 (-1.9% [-6.4, 3]) | 103 → 102 | 72% → 74% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +17% [4.6, 29.8] | +14.5% [8.7, 28.7] | 114 → 114 (0% [-5.4, 5.6]) | 119 → 122 | 80% → 80% | 0 → 0 | 0 → 0 |
| gemini-3-flash | 24/24 | -18% [-47.6, 37.7] | +0.5% [-16.6, 22.7] | 182 → 174 (-4.4% [-16, 9.1]) | 173 → 160 | 37% → 22% | 24 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | 24/24 | +4% [-16.8, 32.6] | +0.2% [-13, 17.7] | 169 → 158 (-6.5% [-18.1, 8.3]) | 163 → 150 | 15% → 18% | 31 → 23 | 0 → 0 |
| gpt-6-sol | 24/24 | -5.5% [-17.2, 10.8] | -6.8% [-23.2, 12.6] | 108 → 97 (-10.2% [-18.3, -1.9]) | 116 → 77 | 59% → 60% | 0 → 0 | 0 → 0 |
| kimi-k2.7-code | 24/24 | +59.1% [24.2, 92.7] | +69.9% [-8.3, 131.6] | 113 → 106 (-6.2% [-12.5, 0]) | 107 → 102 | 84% → 83% | 2 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | fan_out | 3/3 | +1.4% [-29.2, 32.9] | 12 → 13 | 0 → 0 |
| claude-opus-5 | multi_file_edit | 3/3 | -8.9% [-11.3, -7.3] | 12 → 9 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | +48.3% [23.4, 114.2] | 11 → 13 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | -7% [-7.4, -6.6] | 12 → 9 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | +17.8% [13.9, 22.7] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | +17.8% [1.1, 31.4] | 15 → 14 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | +10% [-12.1, 24.3] | 12 → 11 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | -7.4% [-9.7, -3.9] | 12 → 9 | 0 → 0 |
| claude-sonnet-5 | fan_out | 2/3 | +5.7% [5, 6.4] | 6 → 6 | 0 → 1 |
| claude-sonnet-5 | multi_file_edit | 3/3 | +24.1% [-7.1, 66.2] | 11 → 11 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | +25.3% [24.9, 25.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | +26% [25.9, 26] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | +7.4% [-10.1, 27.8] | 15 → 13 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | +33.1% [25.1, 50.8] | 16 → 17 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | +24.8% [23.3, 27.8] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | +26.4% [26.2, 26.7] | 12 → 12 | 0 → 0 |
| deepseek-flash | fan_out | 3/3 | +11.8% [-0.8, 50.6] | 13 → 12 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +38% [29.6, 53.8] | 12 → 13 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +25.3% [23.5, 27.7] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +12.5% [-4.8, 21.4] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +11.7% [-2.7, 22.2] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +29% [22.1, 32.9] | 15 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +8.4% [-13.1, 30.8] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +26.9% [22.8, 30.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | fan_out | 3/3 | +11.6% [-9.8, 37.7] | 19 → 20 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +24.4% [0.8, 48.6] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +7.1% [-2.5, 22.4] | 15 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +24.1% [23.5, 24.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +22% [19.1, 25.7] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +43.5% [27.7, 59.5] | 15 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +16.2% [-3.9, 30.4] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +22.4% [17.5, 25.3] | 12 → 12 | 0 → 0 |
| gemini-3-flash | fan_out | 3/3 | -56.1% [-78.7, 136.5] | 34 → 30 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | +33.2% [-33.2, 99.9] | 30 → 30 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | +56.4% [40.8, 76.1] | 13 → 15 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | +50.3% [18.8, 176.4] | 13 → 17 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | -26.6% [-52.1, -12] | 28 → 19 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | +0.9% [-25.4, 22.7] | 22 → 19 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | +0.8% [-35.4, 40.4] | 24 → 20 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | +81.6% [39, 156.3] | 18 → 24 | 0 → 0 |
| gemini-3.5-flash-lite | fan_out | 3/3 | -32.9% [-57.5, 27.8] | 29 → 22 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | +110.6% [34, 237.6] | 17 → 23 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | +27.6% [-21.6, 106.3] | 13 → 14 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | -0.8% [-27.8, 49.2] | 20 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | +15.3% [0.7, 45.2] | 26 → 25 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | +56.9% [14.1, 148.7] | 17 → 23 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | -27.9% [-36.6, -12.8] | 24 → 16 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | -16.4% [-47.1, 65.2] | 23 → 17 | 0 → 0 |
| gpt-6-sol | fan_out | 3/3 | -27% [-50, -12.3] | 11 → 11 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | -26.5% [-27.7, -25.6] | 15 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | +20.5% [-3.9, 39.9] | 12 → 11 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | +26.7% [26.5, 26.9] | 12 → 12 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | +15.3% [11.2, 21.1] | 15 → 15 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | +27.6% [26.9, 28.1] | 18 → 18 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | +28.4% [28, 29.2] | 12 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | -10.8% [-21.1, -2.6] | 13 → 9 | 0 → 0 |
| kimi-k2.7-code | fan_out | 3/3 | +154.1% [122.2, 209.1] | 19 → 14 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | +41.5% [27, 64.6] | 12 → 13 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | +23% [-7.2, 66.1] | 13 → 13 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | +28.3% [23.9, 33.7] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | +11.9% [-2.2, 21.6] | 16 → 15 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | +24.1% [7.2, 38.7] | 16 → 15 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | +18.5% [4.1, 28.6] | 13 → 12 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | +23.6% [14.8, 34.7] | 12 → 12 | 0 → 0 |

</details>

## `codemode-default+17ddacbb4f` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 27/27 | 9833 | 26716 | 3.88 | 83% | 0 |
| claude-sonnet-5 | 26/27 | 9835 | 22813 | 4.13 | 84% | 0 |
| deepseek-flash | 27/27 | 9709 | 19992 | 4.25 | 74% | 0 |
| deepseek-v4-pro | 27/27 | 9710 | 25780 | 4.75 | 80% | 0 |
| gemini-3-flash | 27/27 | 9659 | 32971 | 7.25 | 22% | 26 |
| gemini-3.5-flash-lite | 27/27 | 9666 | 27937 | 6.58 | 18% | 23 |
| gpt-6-sol | 27/27 | 9654 | 13563 | 4.04 | 60% | 0 |
| kimi-k2.7-code | 27/27 | 9704 | 19744 | 4.42 | 83% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24 | 21 | 8 | 0 | 11/11 | 0 |
| claude-sonnet-5 | 24 | 27 | 17 | 0 | 21/22 | 0 |
| deepseek-flash | 24 | 30 | 20 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 24 | 42 | 21 | 4 | 21/23 | 0 |
| gemini-3-flash | 24 | 102 | 16 | 3 | 20/23 | 0 |
| gemini-3.5-flash-lite | 24 | 86 | 17 | 0 | 21/24 | 0 |
| gpt-6-sol | 24 | 25 | 14 | 0 | 14/17 | 0 |
| kimi-k2.7-code | 24 | 34 | 21 | 0 | 21/22 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-sonnet-5 | fan_out | 2 | codemode-default+17ddacbb4f | validation failed |
