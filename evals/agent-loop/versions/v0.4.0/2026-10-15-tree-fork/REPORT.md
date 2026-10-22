# Rust session tree navigation and forking

Candidate `89aa827dee` at `89aa827dee`, compared with `9a2baf1a83` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T13:03:48.625Z.

Paired clean native builds: base 9a2baf1a835269e8bb8e2a3e03f76e4e2aa86952, candidate 89aa827dee46ae5415476e8127d70dd4d92b6eab. Six standard/extended models, eight standard scenarios, three trials; medium thinking where available and DeepSeek's high default. The slice changes interactive history selection through /tree and /fork. The standard headless suite does not exercise those commands; separate compiled-provider and real-gateway fullscreen/inline scenarios check selected ancestry, draft restoration, excluded history, source-file preservation, fork writes and terminal restoration. No performance claim. The sole flag is GPT tool errors 0 -> 1. In candidate ordinary_edit trial 0, the model sent overlapping edits; the unchanged edit tool rejected them, the model corrected the call, and the task succeeded. This flag is accepted for this opt-in UI/navigation slice as a model-generated invalid call followed by successful recovery: no changed tool implementation or headless input was found. All 144 paired system prompts match after executable/temporary-path normalization; the six captured first-request pairs match except prompt_cache_key for two models. That evidence supports the disposition but does not establish identical generated responses. GPT succeeds in 23/24 candidate and 19/24 baseline samples: candidate ordinary_edit trial 1 and four baseline samples hit the existing trailing-whitespace tool-call guard; baseline multi_file_edit trial 1 hit the 180-second wall-clock limit after one request and no tool call. These failures remain unresolved provider reliability evidence; all samples are retained, with no retry replacement. Every other model passes 24/24 on each arm. DeepSeek Flash has two baseline tool errors from shell discovery and a diagnostic test before successful repair, versus zero candidate tool errors. There are no additional regression flags.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs 9a2baf1a83: tool errors 0 -> 1

## Against `9a2baf1a83`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -2.5% [-5.9, -0.3] | -2.1% [-5, 0.7] | 68 → 67 (-1.5% [-4.5, 0]) | 78 → 78 | 83% → 83% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +0.4% [-1.1, 3.6] | -2% [-2.4, -1.6] | 66 → 67 (+1.5% [0, 4.7]) | 69 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +10.4% [-2.8, 24.2] | +10% [3.1, 16.4] | 86 → 93 (+8.1% [-2.2, 19]) | 98 → 109 | 77% → 77% | 2 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +3% [-3.4, 10.3] | -2.5% [-5.3, 0.1] | 95 → 97 (+2.1% [-3.2, 8.2]) | 100 → 107 | 81% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | -2.5% [-6.2, 0.3] | -2.2% [-6, 0.4] | 69 → 69 (0% [-4.2, 4.2]) | 95 → 96 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 16/21 | +0.1% [-9, 10.7] | -1% [-22.8, 23] | 76 → 80 (+5.3% [1.3, 9.9]) | 62 → 65 | 53% → 54% | 0 → 1 | 5 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -1.5% [-8.3, 5.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -1.2% [-1.4, -1.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -1% [-1.9, -0.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.7% [-3, 1.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -10.2% [-26.3, -0.4] | 11 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -1% [-1.7, -0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.6% [-3.5, 1.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.2% [-1.4, -1.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -1.1% [-1.4, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -1.1% [-1.1, -1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -1.1% [-1.3, -1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +9.8% [-1.1, 31.5] | 9 → 10 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -1% [-1.2, -0.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1% [-1.3, -0.4] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -1.5% [-6.6, 5.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -10.3% [-44.3, 37.9] | 12 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +22.7% [1.7, 79.4] | 11 → 13 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +34.9% [26.5, 47.5] | 13 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +13.3% [-0.6, 35.3] | 14 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -1.5% [-31.3, 73.5] | 14 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +15.8% [-9.7, 38.9] | 10 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -0.3% [-21.1, 25.8] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +3.3% [-21.3, 36.3] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.5% [-1.7, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -0.5% [-2.7, 0.9] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +15.2% [-4.1, 37.9] | 17 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.9% [-1.7, -0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -0.5% [-6.1, 4.1] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -0.3% [-2.4, 2.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -2.5% [-6.5, 0.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -2% [-5, 0.8] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +2.7% [-1.6, 8.9] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -10.3% [-27.3, -0.5] | 12 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -0.5% [-2.8, 3.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -4.8% [-5.2, -4.5] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | +16.1% [-4.3, 42.7] | 12 → 13 | 1 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | +25.4% [25.1, 25.6] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -0.7% [-0.7, -0.7] | 5 → 6 | 2 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | -6.5% [-19.5, 8.8] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 2/3 | -11.8% [-22.5, -1.1] | 10 → 10 | 1 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -15.6% [-23.3, -1] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +6.1% [-23.2, 26.3] | 14 → 15 | 0 → 0 |

</details>

## `89aa827dee` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6766 | 9736 | 3.19 | 83% | 0 |
| claude-sonnet-5-5 | 24/24 | 6768 | 9403 | 3.19 | 84% | 0 |
| deepseek-flash | 24/24 | 6654 | 11166 | 4.43 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6655 | 11467 | 4.62 | 82% | 0 |
| gemini-3.8-flash-medium | 24/24 | 6691 | 6724 | 3.29 | 0% | 0 |
| gpt-6.1-sol | 23/24 | 6679 | 9084 | 5 | 51% | 1 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 27 | 17 | 2 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 31 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 3 | 0 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 37 | 20 | 0 | 20/20 | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 1 | 9a2baf1a83 | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 2 | 9a2baf1a83 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 9a2baf1a83 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 89aa827dee | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 9a2baf1a83 | runaway |
| gpt-6.1-sol | shell_then_edit | 2 | 9a2baf1a83 | runaway |
