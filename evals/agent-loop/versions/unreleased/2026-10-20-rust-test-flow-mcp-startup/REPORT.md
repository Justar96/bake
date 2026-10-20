# Atomic MCP startup with parallel Rust checks

Candidate `6e92fe6403+mcp-startup-atomic` at `6e92fe6403`, compared with `develop@6e92fe6403` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: mcp_docs, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-20T16:55:51.605Z.

The candidate publishes the first MCP tools, resources, instructions, and connected state atomically. The clean base is develop at 6e92fe6403964a2139eaba4720566081182824ba; both arms use their own locked release build. The standard and extended model sets ran the eight standard scenarios plus the existing mcp_docs scenario, with three trials and one process per model. All 432 samples passed, including all 48 MCP samples; every sample has complete usage and none is invalid. Pair order alternates within each model. The recorded design consolidates the eight processes' identical settings and all eight model routes; their original raw designs remain in .preflight/.

The first-request audit reproduced the startup race in deepseek-v4-pro.mcp_docs.0.base: its three resource tools and server instructions were absent. First declarations were complete in 23/24 base MCP samples and 24/24 candidate samples. The deterministic publication test separately fails when the split updates are restored. Outside MCP, all 192 paired first prompts and tool schemas match after normalizing checkout and temporary workspace paths. The base's longer executable path adds 86 request bytes; it is not an instruction change. Provider cache use is observed, not assumed cold.

Two tool-error regressions are retained and accepted. DeepSeek v4 Pro rose from 0 to 1 because one candidate shell probe read an absent package.json. Gemini 3.5 Flash Lite rose from 17 to 21: expected failing checks before the edit increased from 8 to 12, while unsuccessful git probes outside a repository stayed at 9 in each arm. Both arms recovered and passed every task. These errors occur in standard scenarios with matching normalized first prompts and tool schemas, outside the changed MCP path, and are consistent with model-run variation. No total-token or request-count regression was flagged. No samples were retried or removed to clear these flags.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-v4-pro vs develop@6e92fe6403: tool errors 0 -> 1
- gemini-3.5-flash-lite vs develop@6e92fe6403: tool errors 17 -> 21

## Against `develop@6e92fe6403`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | +4.2% [-0.7, 9.7] | -0.9% [-4.2, 2.6] | 88 → 92 (+4.5% [-1, 10.2]) | 82 → 91 | 88% → 89% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 24/24 | +0.6% [-3.4, 5.1] | -5.1% [-8.2, -2.7] | 99 → 101 (+2% [-1.9, 6]) | 87 → 84 | 91% → 92% | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | -2.4% [-8.1, 3.9] | -1.4% [-3.6, 1.2] | 103 → 102 (-1% [-5.8, 4]) | 107 → 104 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -5.9% [-11.1, -0.8] | -1.1% [-4.3, 2.3] | 108 → 103 (-4.6% [-9, -0.9]) | 108 → 106 | 80% → 79% | 0 → 1 | 0 → 0 |
| gemini-3-flash | 24/24 | -8.9% [-24.2, 11.3] | -4.7% [-18, 11.2] | 162 → 150 (-7.4% [-19.4, 7.4]) | 143 → 134 | 13% → 8% | 20 → 17 | 0 → 0 |
| gemini-3.5-flash-lite | 24/24 | -5.1% [-22.8, 15.7] | +0.2% [-15, 18] | 149 → 147 (-1.3% [-15, 14.4]) | 137 → 134 | 13% → 8% | 17 → 21 | 0 → 0 |
| gpt-6-sol | 24/24 | -4.7% [-9.2, 0] | -28% [-42.4, -11.2] | 94 → 90 (-4.3% [-9.1, 0]) | 76 → 73 | 57% → 68% | 1 → 0 | 0 → 0 |
| kimi-k2.7-code | 24/24 | -1.8% [-8.8, 4.9] | -11.1% [-20.6, -0.2] | 103 → 103 (0% [-6.5, 5.9]) | 99 → 99 | 88% → 89% | 1 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | mcp_docs | 3/3 | -0.9% [-1.1, -0.8] | 9 → 9 | 0 → 0 |
| claude-opus-5 | multi_file_edit | 3/3 | +21.7% [-1.6, 35.6] | 9 → 11 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | -1.8% [-7.4, 1.3] | 10 → 10 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | +9.6% [-0.8, 30] | 10 → 11 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | -2.4% [-4.7, 1] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | +7.7% [0.3, 26.1] | 14 → 15 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | +11% [1.4, 32.2] | 11 → 12 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | -8.2% [-20.8, 0.6] | 10 → 9 | 0 → 0 |
| claude-sonnet-5 | mcp_docs | 3/3 | -1.1% [-1.3, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5 | multi_file_edit | 3/3 | -0.8% [-1, -0.6] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | -0.9% [-1, -0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | -3.1% [-7, -0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | +13.2% [-7.4, 27.7] | 13 → 15 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | +5.1% [-1.6, 19.3] | 16 → 17 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | -9.6% [-22, -1.1] | 13 → 12 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | -0.8% [-0.9, -0.7] | 12 → 12 | 0 → 0 |
| deepseek-flash | mcp_docs | 3/3 | -10.3% [-28.6, 1.1] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -5.9% [-7.8, -2.8] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -1.3% [-1.6, -1] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -3.8% [-6.1, -1.1] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +25.6% [18.9, 32.4] | 14 → 17 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -14.3% [-23.8, 1.4] | 15 → 13 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -10.5% [-18.7, -5.7] | 14 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -0.1% [-4.5, 5.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | mcp_docs | 3/3 | -8.3% [-27.5, 13.7] | 10 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -8.7% [-20.3, -0.5] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -7% [-19.7, 1] | 15 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -1.2% [-2.2, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -13.6% [-19.5, -4.7] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +3.4% [-16.3, 25.9] | 17 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.7% [-1.3, 0.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -10.3% [-25.7, 1.4] | 13 → 12 | 0 → 0 |
| gemini-3-flash | mcp_docs | 3/3 | +9% [3.6, 17.2] | 12 → 12 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | -30% [-38.3, -15.4] | 33 → 26 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | -7.2% [-19.6, 17.9] | 17 → 16 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | +37.7% [-4.3, 57.8] | 17 → 21 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | -11.4% [-39.6, 45.2] | 24 → 22 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | +16.1% [-53.1, 110.5] | 19 → 20 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | -18.4% [-57.9, 83.4] | 17 → 15 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | -26.7% [-49.5, 51.8] | 23 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | mcp_docs | 3/3 | +3.1% [-1.1, 5.7] | 12 → 12 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | -11.1% [-62.3, 54.8] | 27 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | -20.9% [-38.1, -0.9] | 17 → 14 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | +74.5% [31.3, 145.4] | 14 → 21 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | -26.4% [-50.1, -12] | 26 → 20 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | -25.6% [-60.2, 82.9] | 22 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | +16.3% [-22, 110.7] | 13 → 16 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | +12.4% [-4.3, 41.2] | 18 → 20 | 0 → 0 |
| gpt-6-sol | mcp_docs | 3/3 | -0.8% [-0.8, -0.7] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | -0.4% [-3.3, 1.1] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | -11.5% [-27.3, -0.7] | 10 → 9 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | -1.1% [-2.6, 0] | 12 → 12 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | -9.9% [-19.3, 3.3] | 15 → 13 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | -0.9% [-18, 19.3] | 17 → 17 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | -1.1% [-1.3, -0.6] | 12 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | -11.2% [-23.7, -0.4] | 10 → 9 | 0 → 0 |
| kimi-k2.7-code | mcp_docs | 3/3 | -1.1% [-1.5, -0.6] | 9 → 9 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | -27.5% [-38.1, -19.3] | 16 → 12 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | +8.3% [0.5, 22.2] | 12 → 13 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | -3% [-8.5, 0.7] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | +3.3% [-7.7, 20.1] | 15 → 16 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | +3.9% [-11, 28.8] | 15 → 16 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | +7.4% [-3.8, 27.4] | 12 → 13 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | +0.6% [-1.9, 2.8] | 12 → 12 | 0 → 0 |

</details>

## `6e92fe6403+mcp-startup-atomic` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 27/27 | 9791 | 18491 | 3.83 | 89% | 0 |
| claude-sonnet-5 | 27/27 | 9793 | 19254 | 4.21 | 92% | 0 |
| deepseek-flash | 27/27 | 9667 | 13741 | 4.25 | 77% | 0 |
| deepseek-v4-pro | 27/27 | 9668 | 14089 | 4.29 | 79% | 1 |
| gemini-3-flash | 27/27 | 9617 | 24014 | 6.25 | 8% | 17 |
| gemini-3.5-flash-lite | 27/27 | 9624 | 23354 | 6.13 | 8% | 21 |
| gpt-6-sol | 27/27 | 9612 | 9734 | 3.75 | 68% | 0 |
| kimi-k2.7-code | 27/27 | 9662 | 12485 | 4.29 | 89% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24 | 17 | 12 | 0 | 16/19 | 0 |
| claude-sonnet-5 | 24 | 26 | 21 | 0 | 21/24 | 0 |
| deepseek-flash | 24 | 27 | 19 | 0 | 21/24 | 0 |
| deepseek-v4-pro | 24 | 28 | 21 | 1 | 21/24 | 0 |
| gemini-3-flash | 24 | 75 | 18 | 2 | 21/24 | 0 |
| gemini-3.5-flash-lite | 24 | 72 | 17 | 3 | 21/24 | 0 |
| gpt-6-sol | 24 | 15 | 10 | 0 | 10/13 | 0 |
| kimi-k2.7-code | 24 | 28 | 21 | 1 | 21/24 | 0 |
