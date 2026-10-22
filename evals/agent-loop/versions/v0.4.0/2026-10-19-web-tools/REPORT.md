# Scope 12 web tools on by default, hooks, and /share, /bug, /changelog

Candidate `s12+062aecb209` at `062aecb209`, compared with `develop+4703b27537` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-18T18:23:39.930Z.

Paired clean native builds: base origin/develop 4703b27537 in its own worktree, candidate 062aecb209. Standard suite, 7 models, 3 trials, 336/336 successful. The model-visible change is web_search and web_fetch joining the default tools by owner decision: about 600 more characters of tool definitions in every request (toolSchemaChars 4.1-4.3k to 4.7-4.9k); the system prompt is unchanged. No sample called either web tool, and failures, requests, and tool errors are unchanged. The flagged total-token rises, claude-opus-5-5 +8.2%, claude-sonnet-5-5 +9.7%, and gemini-4-argon-high +9.0% (the other models +2 to +5%, intervals spanning zero for four), are the cost of the definitions and are accepted as the owner's on-by-default choice. Hooks run only when configured and /share, /bug, and /changelog send nothing to the model, so neither changes this suite's requests. No scenario exercises web search or fetch; live print-mode runs on the CLIProxyAPI route called both and answered with a cited URL.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs develop+4703b27537: total tokens 8.2% [5.6, 12.3]
- claude-sonnet-5-5 vs develop+4703b27537: total tokens 9.7% [7.6, 13.5]
- gemini-4-argon-high vs develop+4703b27537: total tokens 9% [6.6, 12.8]

## Against `develop+4703b27537`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +8.2% [5.6, 12.3] | -1.4% [-4.4, 1.6] | 66 → 67 (+1.5% [0, 4.7]) | 85 → 87 | 84% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +9.7% [7.6, 13.5] | +1.3% [0, 3.3] | 66 → 67 (+1.5% [0, 4.7]) | 78 → 80 | 85% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +5.2% [-1.1, 11.8] | +6.8% [3.5, 10.2] | 90 → 89 (-1.1% [-6.7, 4.5]) | 89 → 98 | 76% → 76% | 1 → 1 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +2.8% [-4.2, 9.7] | +8.9% [5.4, 12.3] | 97 → 94 (-3.1% [-8.1, 2.1]) | 102 → 96 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +2.3% [-5.1, 9.6] | +3% [-4.4, 10.4] | 71 → 69 (-2.8% [-8.2, 2.8]) | 100 → 98 | 0% → 0% | 0 → 0 | 0 → 0 |
| gemini-4-argon-high | 21/21 | +9% [6.6, 12.8] | +9.5% [7.2, 13.4] | 66 → 66 (0% [0, 0]) | 87 → 89 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/21 | +4.9% [-0.2, 10] | -4.8% [-22.5, 17.2] | 104 → 105 (+1% [-3, 5]) | 87 → 84 | 55% → 59% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +6.8% [6.6, 7.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +9.4% [6.9, 13.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +8% [6.9, 9.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +2.2% [-0.1, 6.2] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +19.3% [7.6, 42.6] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +7.6% [7.3, 8.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +7.6% [6.4, 8.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +7.4% [7.4, 7.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +7.9% [7.8, 8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +7.7% [7.5, 7.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +7.3% [7.2, 7.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +20.2% [7.5, 45] | 9 → 10 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +8.2% [8, 8.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +10.1% [8, 14.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +19.5% [2.5, 47.2] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +8.9% [6.8, 12.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +7.6% [6.6, 8.9] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -2.4% [-9.5, 6.9] | 16 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +8.2% [5.6, 9.8] | 15 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -1% [-24.8, 17.5] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +0.1% [-18.3, 48.5] | 11 → 10 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +5.6% [5.2, 6.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +5.8% [-22.5, 43.4] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +6.6% [6.3, 6.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +8.1% [2.6, 12.9] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -12.5% [-30.9, 2.3] | 20 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +9.6% [7.3, 14.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +8.6% [6.4, 11.9] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +17.4% [3.4, 40.2] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +3.6% [1.5, 5.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -6% [-23, 5.6] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +13% [6, 19.3] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -5.6% [-13.5, 5.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -3.4% [-25.7, 28.8] | 11 → 10 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -2.3% [-29.6, 21.3] | 10 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_file_edit | 3/3 | +6.9% [6.8, 7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | multi_site_edit | 3/3 | +6.9% [5.8, 7.6] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | ordinary_edit | 3/3 | +7.6% [7.2, 8.2] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | path_discovery | 3/3 | +16.7% [6.3, 37.2] | 12 → 12 | 0 → 0 |
| gemini-4-argon-high | shell_then_edit | 3/3 | +6.7% [1.3, 9.6] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | stale_edit | 3/3 | +8% [7.4, 8.7] | 9 → 9 | 0 → 0 |
| gemini-4-argon-high | unprompted_edit | 3/3 | +7.2% [7, 7.3] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +15.6% [3.6, 25.8] | 19 → 21 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +4.8% [4.7, 4.8] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +2.7% [-18.4, 27.9] | 16 → 16 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +3% [0, 5.2] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +4.7% [4.6, 4.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +4.7% [4.3, 5.3] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -4.3% [-21.8, 4.5] | 15 → 14 | 0 → 0 |

</details>

## `s12+062aecb209` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 7559 | 11788 | 3.19 | 85% | 0 |
| claude-sonnet-5-5 | 24/24 | 7561 | 11424 | 3.19 | 86% | 0 |
| deepseek-flash | 24/24 | 7433 | 10951 | 4.24 | 76% | 1 |
| deepseek-v4-pro | 24/24 | 7434 | 11873 | 4.48 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 7401 | 9315 | 3.29 | 0% | 0 |
| gemini-4-argon-high | 24/24 | 7401 | 6809 | 3.14 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 7393 | 9660 | 5 | 59% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 23 | 17 | 1 | 20/20 | 0 |
| deepseek-v4-pro | 21 | 28 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 3 | 0 | 1 | 21/21 | 0 |
| gemini-4-argon-high | 21 | 0 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 39 | 21 | 0 | 21/21 | 0 |
