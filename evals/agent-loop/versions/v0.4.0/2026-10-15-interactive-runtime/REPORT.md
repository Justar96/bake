# Rust interactive runtime

Candidate `8961e64b0d` at `8961e64b0d`, compared with `b68aa56aab` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T00:20:15.364Z.

Both arms are clean release builds of bake-rs: base b68aa56aabeebacd82e1f151368ecd283992ccb7 and candidate 8961e64b0d1d1c42b87c4742c6981bc95baf3c37. The candidate connects the existing Rust terminal frontend to AgentSession and adds ordered steering/follow-up queues, shared input preparation, and shutdown ownership. This headless standard suite measures shared CLI/session behavior; it does not exercise the interactive queue or terminal. Those are covered separately by session tests, Linux/macOS PTYs, and a live Opus 5.5 medium fullscreen read, second prompt, and inline resume smoke.

All eight standard scenarios ran three trials per arm for six models (288 samples). The local evaluator catalog lacks gemini-3.8-flash-medium, so that attempt failed before sending any request. As in prior native records, gemini-3.8-flash-high at medium effort substitutes for that alias; the exact standard Gemini alias remains unmeasured. CLIProxyAPI models used the same gateway as the Pi agent through EVAL_GATEWAY; DeepSeek used its official route at high effort.

All 144 paired system prompts match after normalizing the executable and temporary workspace paths. The six captured first no_tools requests match under the same normalization except for per-session prompt_cache_key values on Responses routes. This compares the captured first requests, not every later request.

GPT failures: base 4, candidate 0; each failed sample is marked runawayAbort by the existing whitespace-runaway guard. Failures remain recorded, and incomplete pairs are excluded from token comparisons. No other model sample failed. These observations do not attribute a quality improvement to the TUI change. Follow-up commit f5753041043b367b6f93144096eb9e19fbd5ba42 adds Pi queue-mode and pre-message_start tests plus their source notice; later edits correct documentation and add this evidence. These follow-ups do not change runtime implementation. The evaluated source commit remains 8961e64b0d1d1c42b87c4742c6981bc95baf3c37.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `b68aa56aab`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +1.1% [-1.2, 4.9] | 0% [-3.1, 3] | 69 → 70 (+1.4% [0, 4.5]) | 76 → 82 | 83% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -1.2% [-1.9, -0.7] | -2.5% [-4.6, -0.9] | 66 → 66 (0% [0, 0]) | 66 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -8.1% [-15.2, 0.5] | +2.3% [-1.2, 6] | 92 → 89 (-3.3% [-9.5, 3.6]) | 110 → 100 | 78% → 76% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -4.2% [-9.4, 0.8] | -2.1% [-7.8, 2.5] | 97 → 93 (-4.1% [-8.1, -1]) | 102 → 104 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | 0% [-2.8, 2.5] | -0.6% [-2.9, 1.6] | 69 → 69 (0% [0, 0]) | 96 → 97 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 17/21 | -1.5% [-9.8, 8.5] | -4.9% [-21.4, 14.4] | 85 → 85 (0% [-4.5, 4.9]) | 71 → 72 | 53% → 55% | 1 → 1 | 4 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +1% [-6.6, 5.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.9% [-1, -0.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.8% [-0.9, -0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.4% [-2, 1.7] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -0.6% [-1, -0.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +11.5% [-1.6, 35.6] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.5% [-4, 2.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -4% [-5.4, -1.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.8% [-0.9, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.7% [-0.8, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.9% [-1.3, -0.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | 0% [-0.6, 1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.8% [-0.9, -0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1.5% [-1.9, -0.8] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -5.6% [-8.2, -3] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -11% [-25.1, 0.1] | 13 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +20.5% [-3.6, 40.8] | 10 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -25.5% [-29, -23.1] | 17 → 14 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -18.9% [-25.8, -13.3] | 16 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +19.2% [3.1, 35] | 12 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -10.4% [-26.6, 1.7] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -9.9% [-24.4, 0.1] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -13.6% [-22.3, 5.8] | 14 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.6% [-3, 1.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -3.7% [-15, 8.1] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +3.7% [-2.8, 11.1] | 18 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.1% [-1.3, 1.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -7% [-24.9, 14.7] | 13 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -0.9% [-2.3, 1.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +0.3% [-3.9, 4.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +0.5% [-0.2, 1.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +1.5% [-3.4, 9.3] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -0.8% [-14.3, 9] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -1.5% [-6.2, 8.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +0.4% [-3.9, 3.7] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -1% [-19.9, 23] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/3 | +26.5% [26.5, 26.5] | 4 → 5 | 2 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -21.9% [-21.9, -21.9] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -10% [-19.6, -0.2] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +7% [-0.9, 26.3] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -3% [-22.9, 27.1] | 13 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +1.2% [-27.6, 54.5] | 14 → 15 | 0 → 0 |

</details>

## `8961e64b0d` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6766 | 10204 | 3.33 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6768 | 9263 | 3.14 | 84% | 0 |
| deepseek-flash | 24/24 | 6654 | 10361 | 4.24 | 76% | 0 |
| deepseek-v4-pro | 24/24 | 6655 | 11032 | 4.43 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6689 | 6862 | 3.29 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 6679 | 9082 | 4.9 | 55% | 1 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 4 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 23 | 13 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 27 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 3 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 37 | 21 | 0 | 21/21 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 0 | b68aa56aab | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | b68aa56aab | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | b68aa56aab | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | b68aa56aab | runaway |
