# Rust interactive manual compaction

Candidate `e7ac981426` at `e7ac981426`, compared with `39e787be35` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: long_session, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-15T16:05:10.584Z.

Paired native 39e787be35b1189cc39a2490b43644bf8007998f against e7ac98142655431d617aeff697d952cd5fb4bcc5: eight standard scenarios plus long_session, six standard/extended models, three trials, 324 samples. All arms were clean and their binary hashes verified. The headless suite exercises shared automatic compaction; the interactive /compact command is covered separately by six real Opus 5.5 medium requests across fullscreen and inline modes, checking one saved summary, subsequent context and restart. All 162 paired system prompts match after executable/temp-path normalization. Captured no_tools first requests match except for prompt_cache_key on Gemini and GPT.

Gemini, Sonnet and Opus pass 27/27 per arm. DeepSeek Flash passes 26/27 in each arm; V4 Pro passes 24/27 baseline and 25/27 candidate. Those seven failures are long_session turn-prefix summaries exhausting the output token cap; the summary prompts and budget calculation are unchanged. Opus completes all three long_session trials in each arm with three successful compactions. The DeepSeek summary limit remains an open scope 10 qualification issue.

Three flags are accepted for this bounded interactive-command change. DeepSeek Flash tool errors rise 0 to 1: path_discovery trial 1 reads the missing src/money.js before finding the moved file and completing the task. Gemini tool errors rise 0 to 1: long_session trial 2 runs a test that reports 1.23 instead of 1.24, then repairs the rounding and passes. GPT failures rise 5 to 7 (22/27 baseline, 20/27 candidate): all twelve failures are the gateway whitespace guard stopping unclosed streamed edit/write arguments. None of these flagged samples ran a compaction; the affected tool and provider code is unchanged, and the paired system prompts match. This supports treating the flags as model-response variation rather than a consequence of the compaction wiring; it does not establish GPT route reliability. No token or request-count regression is flagged. All failures remain in the record. Long-session summary limits, provider qualification, compaction crash/intent recovery, Windows ConPTY and Apple Silicon remain open; this is not 0.4 release qualification.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-flash vs 39e787be35: tool errors 0 -> 1
- gemini-3.8-flash-medium vs 39e787be35: tool errors 0 -> 1
- gpt-6.1-sol vs 39e787be35: failures 5 -> 7

## Against `39e787be35`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -2.1% [-4.7, -0.8] | -1.6% [-3.6, -0.5] | 112 → 110 (-1.8% [-4.8, 0]) | 120 → 114 | 83% → 82% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 24/24 | -7.1% [-17, -0.6] | -16.3% [-39.6, -0.9] | 108 → 106 (-1.9% [-5.3, 0]) | 106 → 107 | 83% → 84% | 0 → 0 | 0 → 0 |
| deepseek-flash | 23/24 | -11.5% [-29.9, 7.3] | +2.5% [-2.9, 9.3] | 119 → 115 (-3.4% [-15.9, 8.8]) | 130 → 135 | 81% → 78% | 0 → 1 | 1 → 1 |
| deepseek-v4-pro | 21/24 | +0.3% [-5.3, 6.4] | -0.9% [-4.5, 2.8] | 95 → 96 (+1.1% [-3.1, 5.7]) | 99 → 105 | 81% → 81% | 0 → 0 | 3 → 2 |
| gemini-3.8-flash-medium | 24/24 | +5.9% [-0.4, 10.6] | +5.2% [-0.7, 8.9] | 105 → 108 (+2.9% [0, 5.6]) | 139 → 140 | 1% → 2% | 0 → 1 | 0 → 0 |
| gpt-6.1-sol | 14/24 | -7.7% [-18, 3.9] | -7.1% [-26, 18.7] | 72 → 70 (-2.8% [-6.6, 0]) | 62 → 61 | 51% → 51% | 0 → 0 | 5 → 7 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | long_session | 3/3 | -0.8% [-1.2, -0.4] | 42 → 42 | 0 → 0 |
| claude-opus-5-5 | multi_file_edit | 3/3 | -0.5% [-5.9, 5.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.6% [-0.7, -0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -1% [-1.9, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -3% [-3.6, -2.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -8.7% [-25.5, -0.1] | 12 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -11.1% [-26.6, -0.6] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -2% [-4.4, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | long_session | 3/3 | -11% [-31.7, -0.5] | 42 → 40 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.6% [-0.7, -0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -1% [-1.1, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -0.7% [-0.8, -0.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.7% [-0.9, -0.7] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -1.5% [-2.7, -0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.6% [-0.8, -0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1% [-1.7, -0.6] | 9 → 9 | 0 → 0 |
| deepseek-flash | long_session | 2/3 | -25.2% [-51.3, 1.9] | 33 → 25 | 1 → 1 |
| deepseek-flash | multi_file_edit | 3/3 | -0.4% [-8.8, 10.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -2.8% [-4.7, -1.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +8.5% [-3.7, 33.5] | 11 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -13.3% [-27.7, 17.1] | 15 → 14 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -13% [-19.5, -3.3] | 14 → 13 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +15.3% [-5.6, 32.6] | 12 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +37.4% [-2.5, 81.8] | 10 → 13 | 0 → 0 |
| deepseek-v4-pro | long_session | 0/3 | n/a | 0 → 0 | 3 → 2 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +17.9% [-0.3, 27.7] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +12.2% [0.5, 35.3] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.2% [-0.9, 0.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +0.2% [-6.2, 6.9] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -14.9% [-24, -9.2] | 19 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -4.2% [-10.7, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.8% [-5.6, 11.2] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | long_session | 3/3 | +9.3% [0.5, 16.2] | 36 → 38 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | +0.5% [-0.5, 1.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -1.6% [-7.2, 2.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | -0.3% [-2.9, 3.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -1.4% [-4.7, 0.3] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | +0.7% [-5, 8.1] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | +4% [1.4, 5.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -3.2% [-6, -0.5] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | long_session | 0/3 | n/a | 0 → 0 | 2 → 2 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -3.8% [-8.6, -1] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/3 | +26% [26, 26] | 4 → 4 | 1 → 2 |
| gpt-6.1-sol | ordinary_edit | 0/3 | n/a | 0 → 0 | 1 → 2 |
| gpt-6.1-sol | path_discovery | 3/3 | -22.4% [-38.9, 21.9] | 17 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -8.6% [-22.3, 26] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -0.2% [-0.4, 0] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -2.2% [-22.6, 23.1] | 10 → 10 | 1 → 0 |

</details>

## `e7ac981426` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 6766 | 21839 | 4.58 | 82% | 0 |
| claude-sonnet-5-5 | 27/27 | 6768 | 19862 | 4.42 | 84% | 0 |
| deepseek-flash | 26/27 | 6654 | 16587 | 5 | 78% | 1 |
| deepseek-v4-pro | 25/27 | 6655 | 14942 | 5.09 | 82% | 0 |
| gemini-3.8-flash-medium | 27/27 | 6691 | 17637 | 4.5 | 2% | 1 |
| gpt-6.1-sol | 20/27 | 6679 | 15611 | 5.59 | 63% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24 | 5 | 0 | 0 | 24/24 | 0 |
| claude-sonnet-5-5 | 24 | 2 | 0 | 0 | 24/24 | 0 |
| deepseek-flash | 24 | 27 | 19 | 0 | 23/23 | 0 |
| deepseek-v4-pro | 24 | 33 | 22 | 0 | 22/22 | 0 |
| gemini-3.8-flash-medium | 24 | 4 | 1 | 1 | 24/24 | 0 |
| gpt-6.1-sol | 24 | 31 | 17 | 0 | 17/17 | 7 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-flash | long_session | 1 | 39e787be35 | validation failed |
| deepseek-flash | long_session | 1 | e7ac981426 | empty final reply |
| deepseek-v4-pro | long_session | 0 | 39e787be35 | empty final reply |
| deepseek-v4-pro | long_session | 0 | e7ac981426 | empty final reply |
| deepseek-v4-pro | long_session | 1 | 39e787be35 | empty final reply |
| deepseek-v4-pro | long_session | 2 | 39e787be35 | empty final reply |
| deepseek-v4-pro | long_session | 2 | e7ac981426 | empty final reply |
| gpt-6.1-sol | long_session | 0 | e7ac981426 | runaway |
| gpt-6.1-sol | long_session | 1 | 39e787be35 | runaway |
| gpt-6.1-sol | long_session | 1 | e7ac981426 | runaway |
| gpt-6.1-sol | long_session | 2 | 39e787be35 | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | 39e787be35 | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | e7ac981426 | runaway |
| gpt-6.1-sol | multi_site_edit | 2 | e7ac981426 | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 39e787be35 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | e7ac981426 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | e7ac981426 | runaway |
| gpt-6.1-sol | stale_edit | 1 | e7ac981426 | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 39e787be35 | runaway |
