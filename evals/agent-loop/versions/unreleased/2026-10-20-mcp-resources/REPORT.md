# MCP Streamable HTTP, resource tools, and server instructions

Candidate `mcp-resources+a2218136eb` at `a2218136eb`, compared with `develop+3b44945eb4` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: mcp_docs, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-20T01:13:38.731Z.

Candidate a2218136eb is the scope 12 MCP slices (Streamable HTTP servers, the three resource tools, the mcp_servers prompt section) plus the opt-in mcp_docs scenario, which the evaluator checkout supplies to both arms: base develop 3b44945eb4 starts the same stdio fixture server and offers its get_document tool, but neither resource tools nor the section. The review fixes landed after this run change only text a server could inject (angle brackets and control characters in a summary, a server key in a listing), which this fixture never sends. Standard cases send no MCP configuration, so their requests are byte-identical (first_request_matches_the_fixture); their deltas are run-to-run noise. On mcp_docs every sample of both arms succeeded and every model fetched the checklist with mcp__docs__get_document; no sample called a resource tool. The accepted cost is the three resource tool schemas and the one-line mcp_servers section in every request of a session whose server declares resources: about 1,200 to 2,600 more tokens per mcp_docs sample (DeepSeek V4 Pro and GPT 6 Sol least, Claude and Gemini most), with no added round trip except one Kimi sample that read its written file back. Sessions without an MCP server are unaffected. The flagged DeepSeek Flash tool error (0 -> 1) is shell_then_edit trial 1, a standard case with no MCP configuration: the model ran `ls -la; ...; cat package.json` in a fixture that has no package.json, so bash exited nonzero; the sample still succeeded. The case sends the base's exact requests, so the error is the model's choice, not this change.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-flash vs develop+3b44945eb4: tool errors 0 -> 1

## Against `develop+3b44945eb4`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | -2.7% [-10.7, 5.6] | +3.3% [-2.9, 8.7] | 95 → 91 (-4.2% [-11.6, 3.4]) | 84 → 76 | 89% → 88% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 24/24 | +2.9% [-4.5, 11] | -1.4% [-9.8, 5.9] | 96 → 99 (+3.1% [-2.1, 10]) | 88 → 86 | 90% → 90% | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | +4.2% [-3.2, 12.8] | -1.9% [-7.8, 3.3] | 101 → 104 (+3% [-2.1, 9.3]) | 100 → 101 | 76% → 77% | 0 → 1 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +2.6% [-3.2, 8.6] | +2.8% [0, 6.1] | 104 → 105 (+1% [-3.8, 5.9]) | 105 → 108 | 80% → 80% | 0 → 0 | 0 → 0 |
| gemini-3-flash | 24/24 | +5.6% [-14, 31.2] | +6.8% [-9.5, 27.9] | 154 → 163 (+5.8% [-6.4, 21]) | 139 → 144 | 14% → 13% | 22 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | 24/24 | -1.7% [-19.2, 17.9] | -1% [-16, 15.8] | 163 → 158 (-3.1% [-16.3, 11]) | 141 → 139 | 14% → 14% | 20 → 20 | 0 → 0 |
| gpt-6-sol | 24/24 | -1.1% [-5.4, 3.3] | +0.3% [-16.2, 20] | 92 → 90 (-2.2% [-6.4, 2.1]) | 78 → 69 | 56% → 55% | 0 → 0 | 0 → 0 |
| kimi-k2.7-code | 24/24 | -0.5% [-6.8, 6.7] | -3.3% [-11.5, 6.5] | 104 → 104 (0% [-5.5, 5.7]) | 102 → 98 | 89% → 89% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | mcp_docs | 3/3 | +19.6% [19.5, 19.9] | 9 → 9 | 0 → 0 |
| claude-opus-5 | multi_file_edit | 3/3 | +3.8% [1.8, 6.1] | 9 → 9 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | +18.4% [1.6, 58.9] | 11 → 13 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | -8.4% [-24.1, 0.4] | 12 → 11 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | +1.9% [-2.3, 6.1] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | -17.4% [-44, -0.4] | 15 → 13 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | -9.1% [-21, 2.5] | 13 → 12 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | -17.8% [-39.5, 1.1] | 11 → 9 | 0 → 0 |
| claude-sonnet-5 | mcp_docs | 3/3 | +19.4% [19.2, 19.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5 | multi_file_edit | 3/3 | +9.3% [-2.5, 33.9] | 10 → 11 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | -0.1% [-0.3, 0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | +7.5% [-0.1, 26.8] | 11 → 12 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | -14.8% [-32.7, 0.3] | 15 → 14 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | +21.9% [-0.1, 67.5] | 14 → 17 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | -9.6% [-24, -0.1] | 13 → 12 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | +0.1% [-0.1, 0.2] | 12 → 12 | 0 → 0 |
| deepseek-flash | mcp_docs | 3/3 | +21.2% [19.5, 23.9] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -7.6% [-25.2, 1.4] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +1.4% [-1.5, 5.6] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.4% [-3, 0.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -17% [-25.8, -9.2] | 16 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +10.4% [3.4, 26.4] | 14 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +40.4% [12.6, 71] | 11 → 15 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +1.3% [-1.8, 2.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | mcp_docs | 3/3 | +5.9% [-12.9, 18.6] | 10 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -1.9% [-22.7, 26.4] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +5.6% [-5.2, 23.8] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.6% [-1.5, -0.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -0.9% [-21, 25.3] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +9.4% [0.4, 25] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +2.6% [-0.5, 4.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +2% [-1.4, 6.8] | 12 → 12 | 0 → 0 |
| gemini-3-flash | mcp_docs | 3/3 | +19.1% [14.6, 21.5] | 12 → 12 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | -3.4% [-43.3, 112.1] | 34 → 34 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | +25.5% [-0.9, 58.1] | 14 → 17 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | -16.5% [-31.5, 0.3] | 19 → 16 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | -5.8% [-17.4, 9.7] | 25 → 23 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | +27.1% [-42, 115.4] | 17 → 22 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | +64.7% [29.1, 87.3] | 13 → 19 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | -11.1% [-47.5, 94] | 20 → 20 | 0 → 0 |
| gemini-3.5-flash-lite | mcp_docs | 3/3 | +11.2% [8.8, 13] | 12 → 12 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | -2.2% [-43, 44.1] | 30 → 30 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | -13.7% [-33.7, 26.3] | 18 → 15 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | +37% [-23.3, 98.6] | 16 → 20 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | +24.4% [-1.1, 78.5] | 25 → 29 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | -20% [-60.7, 26] | 25 → 21 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | -37.4% [-65.3, 3.2] | 22 → 15 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | +9.4% [-0.4, 22.4] | 15 → 16 | 0 → 0 |
| gpt-6-sol | mcp_docs | 3/3 | +14.3% [10.7, 16.2] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | -1.7% [-3.7, 0.1] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | -6.7% [-16.5, -0.3] | 10 → 9 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | +0.3% [-2.2, 2.9] | 12 → 12 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | -9.4% [-21.5, 3.5] | 14 → 13 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | +0.1% [-16.1, 19.2] | 17 → 17 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | -0.5% [-1.2, -0.2] | 12 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | +0.4% [-2, 2] | 9 → 9 | 0 → 0 |
| kimi-k2.7-code | mcp_docs | 3/3 | +32.9% [19.3, 59.7] | 9 → 10 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | -5.6% [-28.5, 22.8] | 13 → 13 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | -0.3% [-21.4, 28.7] | 14 → 14 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | -1.3% [-3.4, 2.9] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | -3.3% [-7.1, 3] | 16 → 16 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | -13.5% [-22.1, -1.1] | 16 → 14 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | +8.6% [-1.9, 27.5] | 12 → 13 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | -3.3% [-7.2, 1] | 12 → 12 | 0 → 0 |

</details>

## `mcp-resources+a2218136eb` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 27/27 | 9829 | 18180 | 3.79 | 88% | 0 |
| claude-sonnet-5 | 27/27 | 9831 | 19024 | 4.13 | 90% | 0 |
| deepseek-flash | 27/27 | 9705 | 14054 | 4.33 | 77% | 1 |
| deepseek-v4-pro | 27/27 | 9706 | 14429 | 4.38 | 80% | 0 |
| gemini-3-flash | 27/27 | 9655 | 27187 | 6.79 | 13% | 18 |
| gemini-3.5-flash-lite | 27/27 | 9662 | 26462 | 6.58 | 14% | 20 |
| gpt-6-sol | 27/27 | 9650 | 9728 | 3.75 | 55% | 0 |
| kimi-k2.7-code | 27/27 | 9700 | 12718 | 4.33 | 89% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24 | 16 | 12 | 0 | 14/17 | 0 |
| claude-sonnet-5 | 24 | 24 | 19 | 0 | 21/24 | 0 |
| deepseek-flash | 24 | 29 | 19 | 0 | 21/24 | 0 |
| deepseek-v4-pro | 24 | 30 | 21 | 1 | 21/24 | 0 |
| gemini-3-flash | 24 | 88 | 19 | 1 | 21/24 | 0 |
| gemini-3.5-flash-lite | 24 | 83 | 18 | 0 | 21/24 | 0 |
| gpt-6-sol | 24 | 15 | 9 | 0 | 9/12 | 0 |
| kimi-k2.7-code | 24 | 29 | 20 | 1 | 21/24 | 0 |
