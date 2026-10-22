# Rust session navigation and terminal sheets

Candidate `74ff74e9d5` at `74ff74e9d5`, compared with `e9a07c5c53` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T06:42:03.915Z.

Paired clean native builds: base e9a07c5c53b473a739b2db9cd4d4c243c295fa13, candidate 74ff74e9d512949022bebf6398af7f0a8947eecc. Six standard/extended models, eight standard scenarios, three trials, medium thinking where available; DeepSeek uses its high default. This slice adds interactive session navigation and shared terminal sheets. The headless suite does not exercise session switching; compiled-provider and real-gateway fullscreen/inline PTY checks independently verify model/thinking/history restoration, log separation, cancellation and terminal restoration. No performance claim. The only flag is GPT failures 2 -> 4: the base succeeds in 22/24 tasks and the candidate in 20/24. All six failed samples report more than 2,000 trailing whitespace characters after tool arguments without a closing call, causing the existing guard to abort. The same failure class appears on both arms and in the preceding interactive-controls record. After normalizing executable and temporary paths, all 144 paired system prompts match; the six captured first-request pairs match except for prompt_cache_key in two models. This supports treating the failures as the existing provider-generation problem; it does not prove the extra candidate failures are unrelated. The flag is accepted for this opt-in UI/session slice because the protective guard is unchanged, no changed headless model input was found, and independent interactive checks pass. Provider reliability remains unresolved; failed samples are retained and no retry replaces them. All other models pass 24/24 on each arm, with no additional regression flags. The test-only completion-readiness follow-up 18fd5a21a3 leaves the evaluated Rust source and binary unchanged.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs e9a07c5c53: failures 2 -> 4

## Against `e9a07c5c53`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -3.2% [-6.4, -1.1] | -4.3% [-6.5, -2.1] | 70 → 69 (-1.4% [-4.3, 0]) | 78 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -1.3% [-1.8, -1.1] | -2.9% [-4.3, -2.2] | 66 → 66 (0% [0, 0]) | 69 → 69 | 84% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +7% [-1.3, 16.5] | +7% [3.3, 11.1] | 83 → 89 (+7.2% [0, 15.7]) | 100 → 111 | 76% → 76% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +1.7% [-2.6, 6.1] | -1.3% [-4.2, 1.6] | 95 → 97 (+2.1% [-2.1, 6.2]) | 105 → 105 | 81% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | +6% [-1.8, 15.3] | +5.8% [-2.6, 15.6] | 69 → 71 (+2.9% [-2.9, 10.3]) | 95 → 97 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 16/21 | +2.2% [-6.8, 12] | -8.6% [-26.8, 12.7] | 78 → 79 (+1.3% [0, 4.1]) | 65 → 65 | 48% → 53% | 0 → 0 | 2 → 4 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -3.4% [-6.7, -1.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -1.3% [-1.5, -1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -1.2% [-1.8, -0.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -1.3% [-5.1, 2] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -0.6% [-1.1, -0.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -11.7% [-26.8, -1.2] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -3.7% [-4.4, -2.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -2.6% [-5.5, -0.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -1.2% [-1.4, -1.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -1.2% [-1.2, -1.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -1.2% [-1.3, -1.1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -1.1% [-1.2, -0.9] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -1% [-1.2, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1.2% [-1.3, -1] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +14% [0.5, 33.1] | 11 → 13 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -0.7% [-2.7, 0.5] | 11 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.7% [-25.1, 32.2] | 11 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +2.3% [-3.7, 14.9] | 13 → 13 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +23.8% [-7.9, 81.9] | 13 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +22.3% [1.2, 36.6] | 11 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -11.5% [-27.4, 6.5] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +2.2% [-3.6, 10.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -1% [-20.5, 27.1] | 14 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -1.3% [-2.1, -0.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +4.5% [-6.1, 23.6] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +4.2% [-0.7, 7.9] | 18 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.8% [-1.6, -0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +1.7% [-0.4, 5.6] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | +20.9% [-1.6, 64.7] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -2.1% [-3.5, -1.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -1.4% [-2.7, 0.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +24% [1.5, 54.6] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -4.4% [-21.2, 5.5] | 12 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -0.5% [-4.2, 3.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | 0% [-2.5, 1.4] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -12.5% [-21.6, -3.7] | 12 → 12 | 1 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/3 | -1.1% [-1.1, -1.1] | 4 → 5 | 1 → 2 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +26.1% [26.1, 26.1] | 5 → 5 | 0 → 2 |
| gpt-6.1-sol | path_discovery | 3/3 | +17.5% [-1.5, 38.2] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +14.7% [-1.3, 25.4] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -0.8% [-22.8, 27.3] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -13.6% [-22.8, -1] | 15 → 15 | 0 → 0 |

</details>

## `74ff74e9d5` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6766 | 9942 | 3.29 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6768 | 9257 | 3.14 | 84% | 0 |
| deepseek-flash | 24/24 | 6654 | 10390 | 4.24 | 76% | 0 |
| deepseek-v4-pro | 24/24 | 6655 | 11346 | 4.62 | 82% | 0 |
| gemini-3.8-flash-medium | 24/24 | 6691 | 7055 | 3.38 | 0% | 0 |
| gpt-6.1-sol | 20/24 | 6679 | 9264 | 5 | 54% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 3 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 23 | 13 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 31 | 21 | 2 | 21/21 | 0 |
| gemini-3.8-flash-medium | 21 | 5 | 2 | 1 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 31 | 17 | 0 | 17/17 | 4 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 0 | e9a07c5c53 | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | e9a07c5c53 | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | 74ff74e9d5 | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | 74ff74e9d5 | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 74ff74e9d5 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 74ff74e9d5 | runaway |
