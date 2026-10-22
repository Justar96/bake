# Opt-in code mode off and on, current gateway models, with fan_out

Candidate `codemode-on+b3bcb94a9c` at `b3bcb94a9c`, compared with `codemode-off+b3bcb94a9c` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: fan_out, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-19T20:17:36.002Z.

Supersedes 2026-10-19-codemode, which reached three of seven models because the gateway had stopped serving the other four. Both arms run the same bake-rs build of b3bcb94a9c; on adds codemode to defaultTools, where it is opt-in. The models are the newest of each lab the gateway serves, plus DeepSeek two through its own API. kimi-k2.7-code reaches the gateway only from 7281a9694f, which gives the Chat Completions families the system role; before it every Kimi request failed with 400 "role developer is not allowed", so it has no earlier pair. The scenarios are the standard suite and fan_out, new here. Accepted regressions: total tokens on claude-sonnet-5 (+36.5%), deepseek-v4-pro (+27.1%), gpt-6-sol (+20.0%), and deepseek-flash (+17.8%). Offering codemode adds about 2.1 KB of tool definitions to every request (toolSchemaChars 4692-4860 to 6823-7004). Only claude-opus-5 (29 calls in 16 of 27 samples) and gpt-6-sol (24 calls in 15 of 27) chose it. The other six never did, so their rises are the definition alone. Opus batched reads and edits in scripts: requests -11.8%, tool calls 109 to 68, tokens -6.7% with an interval that spans zero. gpt-6-sol cut requests 11.3% and tool calls 106 to 78, but its scripts return whole outputs, so the fewer round trips carry more tokens. On fan_out with codemode merely offered, the models summed the ledgers with a shell command. Only gpt-6-sol fanned out reads inside a script, and only twice: one script read all sixteen ledgers. The paired codemode-only-fan-out record measures fan_out when the model must script. No sample failed in either arm. The Gemini tool errors are git and node checks the models ran outside a repository, alike in both arms.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5 vs codemode-off+b3bcb94a9c: total tokens 36.5% [20, 52.9]
- deepseek-flash vs codemode-off+b3bcb94a9c: total tokens 17.8% [0.4, 39.7]
- deepseek-v4-pro vs codemode-off+b3bcb94a9c: total tokens 27.1% [11.8, 53.3]
- gpt-6-sol vs codemode-off+b3bcb94a9c: total tokens 20% [4.5, 38.9]

## Against `codemode-off+b3bcb94a9c`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | -6.7% [-31.2, 32.9] | -4% [-38.6, 52.2] | 102 → 90 (-11.8% [-19.6, -4.9]) | 109 → 68 | 83% → 82% | 2 → 0 | 0 → 0 |
| claude-sonnet-5 | 24/24 | +36.5% [20, 52.9] | +3.7% [2.1, 7.9] | 104 → 103 (-1% [-6.7, 6.1]) | 88 → 91 | 79% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | +17.8% [0.4, 39.7] | +18.4% [3.6, 45.9] | 105 → 109 (+3.8% [-1.9, 10.8]) | 106 → 103 | 76% → 76% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +27.1% [11.8, 53.3] | +15.6% [10.5, 27.9] | 114 → 118 (+3.5% [-1.8, 10.9]) | 114 → 119 | 80% → 82% | 0 → 0 | 0 → 0 |
| gemini-3-flash | 24/24 | +6.9% [-15.7, 34] | -2.9% [-16.4, 12.6] | 197 → 182 (-7.6% [-19.4, 4.7]) | 181 → 161 | 15% → 23% | 34 → 25 | 0 → 0 |
| gemini-3.5-flash-lite | 24/24 | +13.6% [-8.1, 39.6] | +2.8% [-10.9, 20.4] | 185 → 181 (-2.2% [-14.5, 12]) | 169 → 163 | 14% → 23% | 30 → 28 | 0 → 0 |
| gpt-6-sol | 24/24 | +20% [4.5, 38.9] | +11.7% [-8.5, 34.3] | 106 → 94 (-11.3% [-18.9, -3.6]) | 106 → 78 | 56% → 59% | 0 → 0 | 0 → 0 |
| kimi-k2.7-code | 24/24 | -0.9% [-19.9, 20.4] | -20.2% [-44.3, -5] | 109 → 106 (-2.8% [-8, 2.8]) | 126 → 101 | 80% → 84% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | fan_out | 3/3 | -27.4% [-56.9, 139.3] | 16 → 13 | 0 → 0 |
| claude-opus-5 | multi_file_edit | 3/3 | -17.8% [-29.5, -8.3] | 13 → 9 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | +19.3% [-0.6, 33.5] | 10 → 9 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | +25.5% [21.4, 29.7] | 9 → 9 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | +15.7% [-2.4, 29.2] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | +14.5% [-3.5, 24.4] | 15 → 14 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | +20.2% [12.6, 28] | 12 → 12 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | -3.9% [-9.4, 1] | 12 → 9 | 0 → 0 |
| claude-sonnet-5 | fan_out | 3/3 | +70.9% [54.4, 103.7] | 9 → 13 | 0 → 0 |
| claude-sonnet-5 | multi_file_edit | 3/3 | +13.7% [-7.1, 24.2] | 12 → 11 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | +25.4% [25.1, 25.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | +26.3% [26.1, 26.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | +20% [8.1, 26.6] | 15 → 14 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | +19.8% [8.2, 25.7] | 18 → 17 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | +6.6% [-1.2, 26.1] | 14 → 12 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | +26.5% [26.1, 27] | 12 → 12 | 0 → 0 |
| deepseek-flash | fan_out | 3/3 | +9.3% [-24.5, 66.2] | 16 → 15 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +36.1% [21.5, 59.3] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +33.2% [16.9, 70.1] | 11 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +22.8% [18.7, 25.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +5.7% [-17, 87.6] | 15 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +38.6% [25.2, 64.7] | 15 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +26.9% [13.8, 36.5] | 13 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +23.4% [19.1, 27.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | fan_out | 3/3 | +30.7% [5.3, 105.4] | 19 → 22 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +12.7% [-3.7, 25.4] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +32.4% [22.3, 55.2] | 14 → 15 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +24.2% [23.3, 25.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +18.3% [12.5, 21.5] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +11.9% [-11, 27.3] | 17 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +37.1% [25.5, 59.2] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +35% [18.8, 62.9] | 12 → 13 | 0 → 0 |
| gemini-3-flash | fan_out | 3/3 | +33.2% [-14.1, 130.4] | 29 → 31 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | -7.8% [-55.4, 90.1] | 33 → 28 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | +36.3% [7.6, 146.8] | 15 → 16 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | -13% [-58.4, 48.3] | 22 → 18 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | +31.3% [8.6, 68.3] | 25 → 26 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | +1.8% [-25.7, 34.7] | 31 → 30 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | -14.8% [-68.3, 99.2] | 21 → 17 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | -19.2% [-38.9, -1] | 21 → 16 | 0 → 0 |
| gemini-3.5-flash-lite | fan_out | 3/3 | +19.5% [-39.4, 124.5] | 27 → 28 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | -27.8% [-50.2, 34.9] | 34 → 25 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | +71.1% [0.4, 130.2] | 14 → 19 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | +28.6% [-17.9, 121.1] | 17 → 17 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | +36.4% [-0.6, 75.1] | 24 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | +22.1% [-3.2, 76.6] | 26 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | +21.4% [-7.9, 52.3] | 21 → 22 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | -9.2% [-43.2, 83.3] | 22 → 18 | 0 → 0 |
| gpt-6-sol | fan_out | 3/3 | +40.1% [-4.3, 141.6] | 12 → 12 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | -15.3% [-25.2, -5.3] | 13 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | -6.3% [-25.3, 7.4] | 13 → 9 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | +18.4% [0.9, 27.2] | 12 → 11 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | +17.2% [5.2, 27.8] | 15 → 14 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | +27% [5.5, 52.7] | 17 → 17 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | +28.7% [28.4, 29] | 12 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | +6.9% [-6.6, 26.8] | 12 → 10 | 0 → 0 |
| kimi-k2.7-code | fan_out | 3/3 | -28.3% [-64.4, -2.3] | 15 → 14 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | +19.3% [-3.4, 55.9] | 13 → 13 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | +14.4% [-7, 29.1] | 13 → 12 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | +23.9% [17.6, 29.4] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | +23.3% [19.9, 28.4] | 15 → 15 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | +13.6% [0.4, 41.6] | 17 → 16 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | +30.8% [26.6, 37.3] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | +30.5% [25.6, 33.4] | 12 → 12 | 0 → 0 |

</details>

## `codemode-on+b3bcb94a9c` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 27/27 | 9827 | 23388 | 3.75 | 82% | 0 |
| claude-sonnet-5 | 27/27 | 9829 | 29283 | 4.29 | 84% | 0 |
| deepseek-flash | 27/27 | 9703 | 21500 | 4.54 | 76% | 0 |
| deepseek-v4-pro | 27/27 | 9704 | 27820 | 4.92 | 82% | 0 |
| gemini-3-flash | 27/27 | 9653 | 33813 | 7.58 | 23% | 25 |
| gemini-3.5-flash-lite | 27/27 | 9660 | 32997 | 7.54 | 23% | 28 |
| gpt-6-sol | 27/27 | 9648 | 13918 | 3.92 | 59% | 0 |
| kimi-k2.7-code | 27/27 | 9698 | 16541 | 4.42 | 84% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24 | 18 | 8 | 0 | 8/8 | 0 |
| claude-sonnet-5 | 24 | 31 | 19 | 0 | 21/21 | 0 |
| deepseek-flash | 24 | 37 | 20 | 0 | 21/22 | 0 |
| deepseek-v4-pro | 24 | 46 | 21 | 1 | 21/22 | 0 |
| gemini-3-flash | 24 | 110 | 18 | 1 | 21/24 | 0 |
| gemini-3.5-flash-lite | 24 | 109 | 20 | 3 | 21/24 | 0 |
| gpt-6-sol | 24 | 22 | 11 | 0 | 11/14 | 0 |
| kimi-k2.7-code | 24 | 34 | 21 | 0 | 21/21 | 0 |
