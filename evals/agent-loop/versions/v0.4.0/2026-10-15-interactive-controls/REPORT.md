# Rust interactive model, settings and account controls

Candidate `632f76e485` at `632f76e485`, compared with `cbabd50307` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T05:25:56.100Z.

Both arms are clean release builds of bake-rs: base cbabd5030793a8e9b9c912bacc88b2469b044cea and candidate 632f76e485fe8835cf89c24a24622352539fc5b8. The candidate adds interactive model, thinking and settings controls, a CLIProxyAPI account with generation-bound route/key storage, masked dialogs, keyless interactive startup and owned shutdown. Model/thinking changes affect the next request of an active run. The standard suite drives headless mode; compiled terminal scenarios and a separate real CLIProxyAPI Opus medium / Sonnet low smoke cover the interactive controls in fullscreen and inline modes, including saved defaults, settings, refresh, resume and logout across restart.

All eight standard scenarios ran three trials per arm for six models (288 samples). The exact gemini-3.8-flash-medium alias was obtained from the live CLIProxyAPI catalog; no model substitution was made. CLIProxyAPI cohorts used the Pi agent's gateway through EVAL_GATEWAY; DeepSeek used its official route at high effort. Private evaluator homes left the user's settings and credentials unchanged.

All 144 paired system prompts match after normalizing executable and temporary workspace paths. The six captured first no_tools requests match under the same normalization except for per-session prompt_cache_key values on Gemini and GPT Responses routes. This comparison covers those captured first requests, not every later request.

All 240 samples for Gemini, Sonnet, Opus, DeepSeek Flash and DeepSeek V4 Pro passed. GPT passed 19/24 on the base and 18/24 on the candidate. Its 5 base and 6 candidate failures hit the existing guard after the model streamed more than 2,000 trailing whitespace characters without closing a tool call's arguments. Those failed samples remain in this record; incomplete pairs are excluded from token comparisons. The report's measured-task failure totals exclude the no_tools warmup; these counts include all 24 samples per arm. No quality improvement is attributed to the interactive controls. Later documentation and qualification records do not alter the evaluated runtime.

Accepted regression flag: DeepSeek Flash tool errors rose from 0 to 1. In stale_edit trial 0, the candidate ran node test.cjs before editing and reproduced the fixture's known rounding assertion (1.23 versus 1.24). It then changed Math.floor to Math.round, reran the check successfully and preserved the injected comment. The base read the test but did not run it before editing. Both arms passed all 24 DeepSeek Flash samples. This flag is accepted because the extra error is a diagnostic check of the known-broken fixture; the repair and final verification succeeded. No token, request-count or task-failure regression was flagged.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-flash vs cbabd50307: tool errors 0 -> 1

## Against `cbabd50307`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -2.7% [-5.9, -0.5] | -2.9% [-5.7, -0.2] | 69 → 68 (-1.4% [-4.3, 0]) | 77 → 72 | 83% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -0.7% [-0.8, -0.3] | -1.1% [-1.8, 0.1] | 66 → 66 (0% [0, 0]) | 68 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -4.9% [-12.2, 3.1] | +4% [1.1, 7.3] | 90 → 86 (-4.4% [-10.9, 2.3]) | 102 → 100 | 78% → 76% | 0 → 1 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +3.1% [-3.4, 10.5] | +2% [-1, 5.2] | 96 → 99 (+3.1% [-2.9, 9.4]) | 102 → 111 | 82% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | +15.4% [-2.5, 46.7] | +16.1% [-2.8, 49.3] | 70 → 72 (+2.9% [-2.9, 10.1]) | 92 → 101 | 0% → 0% | 1 → 1 | 0 → 0 |
| gpt-6.1-sol | 12/21 | -0.4% [-10.1, 9.7] | +16.8% [-2.4, 39] | 59 → 59 (0% [0, 0]) | 50 → 50 | 62% → 56% | 0 → 0 | 5 → 6 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -2.7% [-6.5, 5.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.7% [-0.9, -0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -1.1% [-1.4, -0.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -1.9% [-3.8, -0.8] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -9.1% [-25.8, -0.7] | 12 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.8% [-1.7, -0.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.8% [-1.5, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.9% [-0.9, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.9% [-1.1, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.8% [-0.9, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.6% [-0.6, -0.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +0.3% [-0.7, 2.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.9% [-1, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1% [-1.1, -0.9] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -1.4% [-8.2, 3.7] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -10.3% [-29.4, 34.6] | 11 → 10 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -10.8% [-28.1, 1.9] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -21.3% [-23.8, -19.7] | 15 → 12 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +8.5% [-3.5, 25.3] | 15 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +13.6% [3.1, 26.7] | 13 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -11.1% [-21.9, 0.4] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +17.2% [1, 28.3] | 13 → 15 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -13.8% [-18.2, -4.1] | 14 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -1.8% [-6.1, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -0.9% [-2.5, 1.6] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -1.8% [-15.4, 28.7] | 18 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +8.6% [-0.3, 26.7] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +20.7% [-0.1, 33.4] | 12 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -1.4% [-1.8, -1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | +0.9% [-1.1, 3.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +0.5% [-1.8, 4.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +58.9% [-12.6, 242.1] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | +18.2% [-6.1, 64.3] | 12 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | +4.7% [2.9, 5.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -1.5% [-2, -1] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -2.5% [-23.6, 27.6] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -11.3% [-22.1, -0.6] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +25.7% [25.7, 25.7] | 5 → 5 | 1 → 2 |
| gpt-6.1-sol | path_discovery | 1/3 | 0% [0, 0] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | shell_then_edit | 2/3 | +11.2% [-0.7, 26.4] | 10 → 10 | 1 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -11.4% [-22, -0.8] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 1/3 | -0.5% [-0.5, -0.5] | 5 → 5 | 0 → 2 |

</details>

## `632f76e485` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6766 | 9816 | 3.24 | 83% | 0 |
| claude-sonnet-5-5 | 24/24 | 6768 | 9261 | 3.14 | 84% | 0 |
| deepseek-flash | 24/24 | 6654 | 10113 | 4.1 | 76% | 1 |
| deepseek-v4-pro | 24/24 | 6655 | 11577 | 4.71 | 82% | 0 |
| gemini-3.8-flash-medium | 24/24 | 6691 | 7841 | 3.43 | 0% | 1 |
| gpt-6.1-sol | 18/24 | 6679 | 9402 | 4.93 | 55% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 20 | 12 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 33 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 6 | 1 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 26 | 15 | 0 | 15/15 | 6 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 1 | 632f76e485 | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | cbabd50307 | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 632f76e485 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 632f76e485 | runaway |
| gpt-6.1-sol | path_discovery | 1 | cbabd50307 | runaway |
| gpt-6.1-sol | path_discovery | 2 | cbabd50307 | runaway |
| gpt-6.1-sol | shell_then_edit | 0 | cbabd50307 | runaway |
| gpt-6.1-sol | stale_edit | 2 | cbabd50307 | runaway |
| gpt-6.1-sol | stale_edit | 2 | 632f76e485 | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 632f76e485 | runaway |
| gpt-6.1-sol | unprompted_edit | 1 | 632f76e485 | runaway |
