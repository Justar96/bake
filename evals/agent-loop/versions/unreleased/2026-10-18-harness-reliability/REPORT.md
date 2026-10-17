# Rust harness lifecycle and output reliability

Candidate `751c2ec329-native` at `751c2ec329`, compared with `0ab01cf2f8-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T21:06:37.010Z.

Paired clean native release builds, eight standard scenarios and three trials per model (288 samples). The owner approved gemini-3.8-flash-high at high effort because the gateway lacks gemini-3.8-flash-medium; standard Gemini coverage remains missing. Five models succeeded in all 24 candidate tasks. GPT failed 7 candidate and 4 base tasks: 6 versus 4 malformed streamed edit calls stopped by the existing whitespace guard, plus one candidate provider-overload response. These are retained as failures; provider stochasticity and differing executable paths prevent attributing the imbalance to this change. Sonnet added one expected failing pre-repair test and DeepSeek Flash added one failed exploratory read of an absent package.json; both tasks subsequently passed unchanged independent checks. DeepSeek Flash used 7.6% more tokens (95% interval 0.7–15.9) with five extra requests, concentrated in shell_then_edit and stale_edit. That cost increase is accepted for this security/lifecycle patch with all 24 tasks passing; no efficiency gain or proven causal attribution is claimed. The record remains regression-flagged. Deterministic fixtures separately establish output cancellation, provider idle/drop cleanup, observer isolation, and persistence ordering. JSON exit-zero after a model error was also observed in both arms and is addressed by the subsequent typed-run-outcome change, outside this measured candidate.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5-5 vs 0ab01cf2f8-native: tool errors 0 -> 1
- deepseek-flash vs 0ab01cf2f8-native: total tokens 7.6% [0.7, 15.9]
- deepseek-flash vs 0ab01cf2f8-native: tool errors 0 -> 1
- gpt-6.1-sol vs 0ab01cf2f8-native: failures 4 -> 7

## Against `0ab01cf2f8-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +1.9% [0, 5.1] | +2.2% [-2.7, 7.3] | 67 → 68 (+1.5% [0, 4.6]) | 85 → 86 | 84% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +0.5% [-0.2, 1.4] | +2.6% [-2.9, 8] | 66 → 66 (0% [0, 0]) | 79 → 80 | 85% → 85% | 0 → 1 | 0 → 0 |
| deepseek-flash | 21/21 | +7.6% [0.7, 15.9] | +1.6% [-0.9, 4.1] | 87 → 92 (+5.7% [0, 12.9]) | 82 → 96 | 76% → 77% | 0 → 1 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -0.3% [-7, 6.4] | -0.2% [-3.2, 2.7] | 96 → 97 (+1% [-4.3, 6.5]) | 102 → 104 | 81% → 81% | 1 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -5% [-10.8, 0.8] | -5.9% [-11.7, -0.2] | 70 → 67 (-4.3% [-10.4, 0]) | 98 → 98 | 0% → 0% | 2 → 0 | 0 → 0 |
| gpt-6.1-sol | 12/21 | +1% [-0.1, 3] | -9.1% [-27.9, 15.7] | 60 → 60 (0% [0, 0]) | 51 → 52 | 62% → 65% | 0 → 0 | 4 → 7 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.1% [0, 0.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.2% [-0.6, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.5% [-0.7, 1.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +2.2% [0, 6.6] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +9.7% [0.1, 31.4] | 10 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.2% [-0.5, 0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.2% [-0.6, 1.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +0.1% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +0.1% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.4, 0.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.9% [-2.3, 0.3] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +0.4% [-0.1, 1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.1% [-0.2, 0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +4.4% [0.3, 6.4] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +11.7% [-0.4, 44.5] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +0.9% [-0.3, 1.6] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -10.4% [-25.5, -2.8] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +0.3% [-4.5, 7.4] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +18.3% [2.9, 51.7] | 14 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +26.7% [-0.1, 40.4] | 12 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +8.5% [-1.3, 35.1] | 11 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +1.4% [-0.7, 5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +6.7% [-22.9, 27.5] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.2% [-0.7, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +3.7% [2.7, 5] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -4.2% [-26.4, 22.9] | 18 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -1.5% [-6.6, 1.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -6.6% [-22.9, 8.2] | 14 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +1.6% [-5.5, 5.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +2.7% [-3.5, 8.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +8.9% [-0.2, 15.9] | 10 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -22.8% [-30.8, -15.8] | 14 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -5.5% [-10.4, -0.9] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -2.9% [-5.2, -1.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -1.9% [-12.5, 8.1] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +2.9% [-0.2, 8.9] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/3 | -0.2% [-0.2, -0.2] | 4 → 4 | 0 → 2 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +0.4% [0.4, 0.4] | 5 → 5 | 1 → 2 |
| gpt-6.1-sol | path_discovery | 1/3 | -0.4% [-0.4, -0.4] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -0.1% [-0.7, 0.5] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | +0.5% [0.2, 0.9] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 1/3 | +0.7% [0.7, 0.7] | 5 → 5 | 1 → 1 |

</details>

## `751c2ec329-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6812 | 10809 | 3.24 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6814 | 10138 | 3.14 | 85% | 1 |
| deepseek-flash | 24/24 | 6700 | 10615 | 4.38 | 77% | 1 |
| deepseek-v4-pro | 24/24 | 6701 | 11235 | 4.62 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6733 | 8040 | 3.19 | 0% | 0 |
| gpt-6.1-sol | 17/24 | 6725 | 9234 | 4.93 | 64% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 26 | 20 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 31 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 1 | 0 | 0 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 25 | 14 | 0 | 14/14 | 6 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 1 | 751c2ec329-native | runaway |
| gpt-6.1-sol | multi_site_edit | 2 | 751c2ec329-native | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 0ab01cf2f8-native | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 751c2ec329-native | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 751c2ec329-native | runaway |
| gpt-6.1-sol | path_discovery | 0 | 751c2ec329-native | empty final reply |
| gpt-6.1-sol | path_discovery | 2 | 0ab01cf2f8-native | runaway |
| gpt-6.1-sol | stale_edit | 1 | 0ab01cf2f8-native | runaway |
| gpt-6.1-sol | stale_edit | 1 | 751c2ec329-native | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 0ab01cf2f8-native | runaway |
| gpt-6.1-sol | unprompted_edit | 2 | 751c2ec329-native | runaway |
