# Durable job journal paired native evaluation

Candidate `0.4.0-dev+69158ef267` at `69158ef267`, compared with `0.4.0-dev+2a3a0647ec` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: background_test, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T21:34:05.486Z.

This paired native run compares clean built base 2a3a0647ec25eb66b906aab1be7db9f400bd0ce1 with clean built candidate 69158ef2676452ad237587bf4b3c650eebd2aa54. It retains all 324 unique samples: eight standard scenarios plus background_test, three trials, and six available standard/extended-set models. No samples are invalid or excluded. Five candidate models pass all 27 tasks; GPT passes 24/27 in both arms. All six GPT failures are malformed edit streams stopped by the whitespace guard. The seventh failure is a base Sonnet background_test that ran the waiting check in the foreground and reached its 60-second tool timeout; this is a model choice, not evidence that the journal caused the outcome. The seven failed samples lack complete usage and remain in failure counts. No regression rule is flagged. Token and request differences include model variability and distinct binary paths; this record establishes no causal efficiency gain. Gemini tool errors on successful tasks are git inspection outside a repository; the base additionally records expected failing tests before repair. The required gemini-3.8-flash-medium route was unavailable; the owner approved gemini-3.8-flash-high at high effort. That substitution leaves required medium coverage missing. The runner uses --no-session, so it exercises live job/tool flow but cannot qualify crash durability or recovery; separate real child-kill/reopen tests prove started-before-producer, terminal-before-publication, and observation-before-acknowledgement ordering. The candidate full preflight passed all 35 gates. macOS was attempted but SSH authentication failed before checks. These samples do not measure the later shared RunControl, typed task documents, or background compaction changes.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `0.4.0-dev+2a3a0647ec`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -0.2% [-0.5, 0] | -0.5% [-4.8, 4] | 80 → 80 (0% [0, 0]) | 105 → 104 | 87% → 87% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 23/24 | -0.7% [-1.9, 0.3] | -1.9% [-7.7, 4.1] | 77 → 77 (0% [0, 0]) | 93 → 93 | 88% → 88% | 2 → 0 | 1 → 0 |
| deepseek-flash | 24/24 | -3.7% [-10.9, 3.6] | -0.7% [-4.3, 3.1] | 108 → 105 (-2.8% [-7.8, 2]) | 103 → 108 | 78% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +3.5% [-3.8, 11.7] | -2.8% [-6.7, 0.7] | 110 → 116 (+5.5% [0, 11.8]) | 122 → 120 | 81% → 83% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 24/24 | -1.5% [-17.5, 16.7] | -1% [-17.7, 18.6] | 85 → 85 (0% [-10.6, 11.5]) | 125 → 124 | 0% → 0% | 4 → 3 | 0 → 0 |
| gpt-6.1-sol | 19/24 | +1% [-4.3, 6.5] | +2.1% [-17.1, 26.2] | 98 → 99 (+1% [-3.1, 5.4]) | 84 → 83 | 68% → 67% | 0 → 0 | 3 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | background_test | 3/3 | -0.1% [-0.7, 0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | multi_file_edit | 3/3 | -0.3% [-0.7, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.4% [0, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.4% [-0.8, -0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.2% [-0.9, 0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -0.5% [-2.2, 0.3] | 11 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.4% [-0.5, -0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.2% [-0.6, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | background_test | 2/3 | -0.1% [-0.2, -0.1] | 8 → 8 | 1 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.1% [-0.2, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.1, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.1% [-0.2, 0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.1% [-2.3, 2.1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +1% [-0.3, 3.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.1% [-0.4, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -6.3% [-10.4, -0.1] | 9 → 9 | 0 → 0 |
| deepseek-flash | background_test | 3/3 | -12.9% [-35.3, 7.7] | 18 → 16 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +2.9% [0.1, 6] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +0.9% [-1.1, 3.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -0.3% [-4.3, 3.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +1.1% [-9.5, 13.9] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -21% [-29, -3.9] | 16 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +16.8% [10, 25.8] | 12 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -2.7% [-29.6, 37.9] | 11 → 11 | 0 → 0 |
| deepseek-v4-pro | background_test | 3/3 | +27.1% [-1.7, 48] | 16 → 20 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -0.7% [-1.4, -0.3] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +2% [-8.8, 22.1] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -2.1% [-4.9, 0.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +14.2% [-5.2, 24.6] | 15 → 17 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -14.4% [-26.5, -6.9] | 18 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -2.8% [-8.9, 1.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.3% [-3.5, 4.3] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | background_test | 3/3 | +9.8% [-55.9, 71.3] | 13 → 15 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -31.1% [-59.5, 6.5] | 12 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -2.5% [-4.6, -0.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -5.8% [-29.4, 57.9] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +7.8% [-1.7, 24.9] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +2.3% [-4.6, 16.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -5.3% [-14.5, 8.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +7.9% [6.8, 9.3] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | background_test | 3/3 | +12.3% [-0.4, 20.3] | 19 → 21 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -1.9% [-3.8, 0.1] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 1/3 | -25.1% [-25.1, -25.1] | 5 → 4 | 1 → 1 |
| gpt-6.1-sol | ordinary_edit | 2/3 | -1.6% [-2.9, -0.3] | 10 → 10 | 1 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -5.8% [-19.3, 6.5] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +0.1% [-0.3, 0.5] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +0.3% [0, 0.6] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +14.5% [4.2, 27.6] | 9 → 10 | 1 → 1 |

</details>

## `0.4.0-dev+69158ef267` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 8033 | 12853 | 3.33 | 87% | 0 |
| claude-sonnet-5-5 | 27/27 | 8035 | 12584 | 3.38 | 88% | 0 |
| deepseek-flash | 27/27 | 7907 | 12193 | 4.38 | 77% | 0 |
| deepseek-v4-pro | 27/27 | 7908 | 13447 | 4.83 | 83% | 0 |
| gemini-3.8-flash-high | 27/27 | 7901 | 11391 | 3.54 | 0% | 3 |
| gpt-6.1-sol | 24/27 | 7893 | 10659 | 5.14 | 68% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24 | 2 | 0 | 0 | 24/24 | 0 |
| claude-sonnet-5-5 | 24 | 3 | 0 | 0 | 24/24 | 0 |
| deepseek-flash | 24 | 27 | 20 | 1 | 24/24 | 0 |
| deepseek-v4-pro | 24 | 38 | 24 | 1 | 24/24 | 0 |
| gemini-3.8-flash-high | 24 | 8 | 0 | 1 | 24/24 | 0 |
| gpt-6.1-sol | 24 | 40 | 21 | 0 | 21/21 | 3 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-sonnet-5-5 | background_test | 0 | 0.4.0-dev+2a3a0647ec | empty final reply |
| gpt-6.1-sol | multi_file_edit | 2 | 0.4.0-dev+69158ef267 | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | 0.4.0-dev+69158ef267 | runaway |
| gpt-6.1-sol | multi_site_edit | 2 | 0.4.0-dev+2a3a0647ec | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+2a3a0647ec | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 0.4.0-dev+2a3a0647ec | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 0.4.0-dev+69158ef267 | runaway |
