# Rust interactive approvals

Candidate `cfcafb2ef9` at `cfcafb2ef9`, compared with `44ff68c676` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T01:53:12.043Z.

Both arms are clean release builds of bake-rs: base 44ff68c676fecdb27fad3e05dc60f42c710d4d74 and candidate cfcafb2ef901cc008de7f13261699e7d2439c8cf. The candidate adds interactive D24 approval prompts with one-call grants, ordered request ownership, cancellation, terminal input barriers, and teardown. Under ask, the interactive permissions prompt says approval prompts are enabled. This standard headless suite does not exercise that changed prompt or approval waiting; compiled PTYs and a real CLIProxyAPI Opus 5.5 medium fullscreen allow-once / inline rejection smoke cover the interactive behavior separately.

All eight standard scenarios ran three trials per arm for six models (288 samples). The exact gemini-3.8-flash-medium alias was resolved from the live CLIProxyAPI catalog; no alias substitution was made. CLIProxyAPI models used the Pi agent's gateway through EVAL_GATEWAY; DeepSeek used its official route at high effort. Temporary evaluator configuration did not change the user's settings or credentials.

All 144 paired system prompts match after normalizing the executable and temporary workspace paths. The six captured first no_tools requests match under the same normalization except for per-session prompt_cache_key values on Gemini and GPT Responses routes. This compares the captured first requests, not every later request.

Retained failures and successes: deepseek-flash — base: 9 passed, 15 provider 503; candidate: 13 passed, 11 provider 503. deepseek-v4-pro — base: 23 passed, 1 network failure; candidate: 24 passed. gpt-6.1-sol — base: 19 passed, 5 whitespace guard; candidate: 19 passed, 5 whitespace guard. HTTP 503 responses report an overloaded provider; network failures report transport errors; GPT failures hit the existing whitespace-runaway guard after unclosed tool arguments. These samples remain in the record. Token comparisons exclude incomplete pairs. DeepSeek Flash has no usable successful measured pairs, so its token and request effects are unmeasured. The report's aggregate measured-task failure totals exclude the no_tools warmup, while the counts in this note include all 24 samples per arm. No quality improvement is attributed to the approval UI.

Follow-up 0cdf7c30fbcd9c19a1cf35a97308d860b73a3103 changes only waiting-composer hints. It passed focused Linux, Intel macOS and native Windows rendering checks and all eight approval PTYs, and changes no model-visible path. The evaluated runtime remains cfcafb2ef901cc008de7f13261699e7d2439c8cf; later edits add documentation and this evidence.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `44ff68c676`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +0.5% [-3.9, 4.6] | +1.6% [-1.1, 4.7] | 70 → 70 (0% [-4.2, 4.3]) | 70 → 80 | 84% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -1.3% [-1.9, -0.8] | -2.9% [-4.9, -1.6] | 66 → 66 (0% [0, 0]) | 68 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 0/21 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 15 → 10 |
| deepseek-v4-pro | 20/21 | +0.3% [-4.8, 6.8] | -2.4% [-4.9, 0.4] | 90 → 91 (+1.1% [-4.2, 7.1]) | 96 → 98 | 81% → 81% | 0 → 0 | 1 → 0 |
| gemini-3.8-flash-medium | 21/21 | -4.5% [-13, 4] | -5.5% [-14.3, 3.4] | 72 → 68 (-5.6% [-13, 1.4]) | 96 → 97 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 14/21 | -1.9% [-10.8, 7.8] | -4% [-16.7, 11.3] | 69 → 69 (0% [0, 0]) | 58 → 58 | 48% → 49% | 0 → 0 | 5 → 5 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +3.8% [-0.3, 6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.8% [-0.9, -0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.6% [-0.8, -0.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +1.7% [-0.7, 5.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -0.8% [-1.1, -0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.3% [-26.4, 34.6] | 10 → 10 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.1% [-1.3, 2.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -3.7% [-5.3, -0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -1% [-1.1, -1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.8% [-0.9, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.8% [-0.8, -0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -0.7% [-0.8, -0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.9% [-1.2, -0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1.1% [-1.9, -0.6] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 0/3 | n/a | 0 → 0 | 2 → 2 |
| deepseek-flash | multi_site_edit | 0/3 | n/a | 0 → 0 | 2 → 2 |
| deepseek-flash | ordinary_edit | 0/3 | n/a | 0 → 0 | 1 → 2 |
| deepseek-flash | path_discovery | 0/3 | n/a | 0 → 0 | 3 → 1 |
| deepseek-flash | shell_then_edit | 0/3 | n/a | 0 → 0 | 2 → 0 |
| deepseek-flash | stale_edit | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | unprompted_edit | 0/3 | n/a | 0 → 0 | 2 → 3 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +17.1% [-2, 30.4] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +1.4% [-18.9, 33.6] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 2/3 | +12.3% [-2.1, 26.5] | 8 → 9 | 1 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -3.1% [-4.6, -0.8] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -10.2% [-14.7, -5.6] | 18 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -2% [-5.4, 0.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -1.9% [-6.7, 2.3] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -20% [-41.9, -0.5] | 11 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | 0% [-3.3, 5.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +2.4% [0.2, 5.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +11.1% [-1.3, 37] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -12.2% [-21.7, 1.2] | 12 → 10 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -14% [-31.8, -1.2] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | 0% [-1.5, 1.2] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -1% [-21.2, 24.2] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -1.1% [-22.6, 26.3] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +26% [26, 26] | 5 → 5 | 1 → 2 |
| gpt-6.1-sol | path_discovery | 1/3 | -27.5% [-27.5, -27.5] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -8.1% [-21.8, -1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | +10.8% [-0.5, 25.2] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -0.4% [-3.2, 2.4] | 10 → 10 | 1 → 1 |

</details>

## `cfcafb2ef9` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6766 | 10230 | 3.33 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6768 | 9259 | 3.14 | 84% | 0 |
| deepseek-flash | 13/24 | 6654 | 9201 | 4.09 | 73% | 0 |
| deepseek-v4-pro | 24/24 | 6655 | 11088 | 4.52 | 81% | 0 |
| gemini-3.8-flash-medium | 24/24 | 6691 | 6756 | 3.24 | 0% | 0 |
| gpt-6.1-sol | 19/24 | 6679 | 8979 | 4.88 | 49% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 4 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 10 | 5 | 1 | 9/11 | 0 |
| deepseek-v4-pro | 21 | 29 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 2 | 1 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 28 | 16 | 0 | 16/16 | 5 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-flash | multi_file_edit | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | multi_file_edit | 0 | cfcafb2ef9 | empty final reply |
| deepseek-flash | multi_file_edit | 2 | 44ff68c676 | empty final reply |
| deepseek-flash | multi_file_edit | 2 | cfcafb2ef9 | empty final reply |
| deepseek-flash | multi_site_edit | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | multi_site_edit | 0 | cfcafb2ef9 | empty final reply |
| deepseek-flash | multi_site_edit | 2 | 44ff68c676 | empty final reply |
| deepseek-flash | multi_site_edit | 2 | cfcafb2ef9 | empty final reply |
| deepseek-flash | no_tools | 1 | cfcafb2ef9 | empty final reply |
| deepseek-flash | ordinary_edit | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | ordinary_edit | 1 | cfcafb2ef9 | empty final reply |
| deepseek-flash | ordinary_edit | 2 | cfcafb2ef9 | empty final reply |
| deepseek-flash | path_discovery | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | path_discovery | 0 | cfcafb2ef9 | empty final reply |
| deepseek-flash | path_discovery | 1 | 44ff68c676 | empty final reply |
| deepseek-flash | path_discovery | 2 | 44ff68c676 | empty final reply |
| deepseek-flash | shell_then_edit | 1 | 44ff68c676 | empty final reply |
| deepseek-flash | shell_then_edit | 2 | 44ff68c676 | empty final reply |
| deepseek-flash | stale_edit | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | stale_edit | 1 | 44ff68c676 | empty final reply |
| deepseek-flash | stale_edit | 2 | 44ff68c676 | empty final reply |
| deepseek-flash | unprompted_edit | 0 | 44ff68c676 | empty final reply |
| deepseek-flash | unprompted_edit | 0 | cfcafb2ef9 | empty final reply |
| deepseek-flash | unprompted_edit | 1 | 44ff68c676 | empty final reply |
| deepseek-flash | unprompted_edit | 1 | cfcafb2ef9 | empty final reply |
| deepseek-flash | unprompted_edit | 2 | cfcafb2ef9 | empty final reply |
| deepseek-v4-pro | ordinary_edit | 0 | 44ff68c676 | empty final reply |
| gpt-6.1-sol | multi_site_edit | 1 | 44ff68c676 | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | cfcafb2ef9 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 44ff68c676 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | cfcafb2ef9 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | cfcafb2ef9 | runaway |
| gpt-6.1-sol | path_discovery | 0 | cfcafb2ef9 | runaway |
| gpt-6.1-sol | path_discovery | 2 | 44ff68c676 | runaway |
| gpt-6.1-sol | stale_edit | 1 | 44ff68c676 | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 44ff68c676 | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | cfcafb2ef9 | runaway |
