# Rust MCP CLI and Session ownership

Candidate `f91a2902be` at `f91a2902be`, compared with `a3b439177a` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T14:42:47.922Z.

Pairs clean native base a3b439177ae4405267c46cdc094c2295cb76306e with clean native candidate f91a2902beafa15f7691e7dbf66a1aaf0b244bac on all eight standard scenarios, six standard/extended models, and three trials: 288 samples, no invalid cells. The four CLIProxyAPI models request medium effort, including the exact gemini-3.8-flash-medium route; both official DeepSeek routes request high. The evaluator uses private EVAL_CONFIG_HOME at tooling revision 9d868a4c22cbac885e03e70bc0936fc5bf572767. User settings were unchanged.

The flagged GPT failure increase is retained and accepted for this stdio MCP change: 4 of 24 base samples and 8 of 24 candidate samples failed when the provider streamed unfinished edit arguments with excessive whitespace. The unchanged tool-argument guard stopped all 12; none was an evaluator-invalid sample or a tool error. The model/gateway cause of the unfinished JSON remains unresolved. No MCP server is configured in these samples, and the captured initial requests (no_tools and ordinary_edit, trial 0, for each model) are identical after replacing the two executable paths and temporary workspace paths and omitting prompt_cache_key. Those paths remain in the measured requests and token counts. The evidence supports accepting this observed failure imbalance without attributing it to MCP startup, but GPT task success remains a qualification limitation. All other models completed all 48 paired samples successfully; no other regression rule fired.

This suite measures the default empty-configuration path. Enabled MCP declarations, filtering, calls, refresh, startup admission, cancellation, and session replacement are exercised by the real-binary, real-request, and PTY fixtures described in rust/README.md. Live tasks using an enabled MCP server, Windows configuration trust, and the remainder of scope 12 are still open. Raw transcripts and request comparisons stay in ignored .preflight/.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs a3b439177a: failures 4 -> 8

## Against `a3b439177a`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -2% [-5.1, -0.1] | -1.4% [-3.4, 0.3] | 68 → 67 (-1.5% [-4.4, 0]) | 76 → 70 | 83% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +3% [-0.2, 7.7] | +0.1% [-1.6, 1.3] | 66 → 68 (+3% [0, 7.7]) | 67 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -6.8% [-13.4, 0.3] | -4.5% [-7.6, -1.2] | 90 → 86 (-4.4% [-9.1, 0]) | 83 → 85 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +1.7% [-5.5, 10.4] | +1.3% [-1.1, 3.8] | 95 → 95 (0% [-5.3, 6.1]) | 99 → 102 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | +1% [-5.9, 7.9] | +1.9% [-5.4, 9.3] | 70 → 71 (+1.4% [-2.9, 6.1]) | 97 → 98 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 10/21 | -3.2% [-13.3, 11] | -7% [-28, 17.2] | 48 → 49 (+2.1% [0, 6.7]) | 40 → 41 | 54% → 56% | 0 → 0 | 4 → 8 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -2% [-5.5, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.2% [0.1, 0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.4% [-1.2, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.3% [-1.4, 1.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -9.5% [-25.4, 0] | 11 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.1% [-0.3, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.6% [-2.3, 2.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.5% [-4.7, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.2, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.1% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.2% [-0.2, 0.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +22.7% [1.8, 33.2] | 9 → 11 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | 0% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +0.5% [-0.7, 1.1] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -8.5% [-29.8, 3.9] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -0.4% [-5.3, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -7.6% [-25, 3.4] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -6.1% [-16.4, 0.4] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -21.4% [-32.9, 1.5] | 16 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -5.2% [-26.2, 13.7] | 12 → 11 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +11.1% [-1, 39] | 11 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -7.3% [-21, 2.4] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -6.7% [-26, 12.4] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.1% [-0.5, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +1.2% [-1.7, 4.2] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +9.3% [-18.6, 52.8] | 18 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +2.9% [0.7, 7.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +11.2% [-0.9, 33.2] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -0.7% [-2.2, 0.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -2% [-6.5, 0.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +0.8% [0.1, 1.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +4.6% [-9.9, 27.9] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -4.3% [-26.8, 32.5] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | +9.4% [-3.4, 34.7] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | 0% [-4.3, 2.3] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -17.6% [-19.3, -15.8] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 2/3 | +12% [-0.2, 27.6] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -21.2% [-21.2, -21.2] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | path_discovery | 1/3 | +0.3% [0.3, 0.3] | 5 → 5 | 0 → 2 |
| gpt-6.1-sol | shell_then_edit | 2/3 | -10.5% [-20.9, -0.1] | 10 → 10 | 0 → 1 |
| gpt-6.1-sol | stale_edit | 1/3 | +0.1% [0.1, 0.1] | 4 → 4 | 2 → 1 |
| gpt-6.1-sol | unprompted_edit | 1/3 | +65% [65, 65] | 4 → 5 | 1 → 1 |

</details>

## `f91a2902be` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6822 | 9761 | 3.19 | 83% | 0 |
| claude-sonnet-5-5 | 24/24 | 6824 | 9635 | 3.24 | 84% | 0 |
| deepseek-flash | 24/24 | 6710 | 9751 | 4.1 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6711 | 11095 | 4.52 | 81% | 0 |
| gemini-3.8-flash-medium | 24/24 | 6747 | 7025 | 3.38 | 0% | 0 |
| gpt-6.1-sol | 16/24 | 6735 | 8841 | 4.85 | 58% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 20 | 17 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 29 | 21 | 2 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 5 | 0 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 23 | 13 | 0 | 13/13 | 8 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 2 | f91a2902be | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | f91a2902be | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | a3b439177a | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | f91a2902be | runaway |
| gpt-6.1-sol | path_discovery | 0 | f91a2902be | runaway |
| gpt-6.1-sol | path_discovery | 2 | f91a2902be | runaway |
| gpt-6.1-sol | shell_then_edit | 2 | f91a2902be | runaway |
| gpt-6.1-sol | stale_edit | 0 | a3b439177a | runaway |
| gpt-6.1-sol | stale_edit | 1 | a3b439177a | runaway |
| gpt-6.1-sol | stale_edit | 1 | f91a2902be | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | a3b439177a | runaway |
| gpt-6.1-sol | unprompted_edit | 2 | f91a2902be | runaway |
