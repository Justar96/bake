# Run control and opt-in task document comparison

Candidate `0.4.0-dev+e60273b96b-task-documents` at `e60273b96b`, compared with `0.4.0-dev+ad9c5a48c2` and `0.4.0-dev+e60273b96b-default` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T21:50:46.216Z.

This native three-arm run contains all 432 unique samples: eight standard cases, three trials, six available models, and base/default/task-document arms. The base is clean ad9c5a48c2; both new arms use the same clean e60273b96b binary, with only taskDocuments enabled in the candidate. Background compaction remains off. All samples are retained, including nine GPT provider-stream failures; none is classified as evaluator-invalid. Five models succeed on 24/24 tasks in every arm. GPT succeeds on 20/24 base, 20/24 default, and 23/24 task-document tasks. Its failures are malformed edit calls that trigger the whitespace guard, with incomplete usage retained; the base exits zero on these model failures, while the new typed-outcome arms correctly exit one. Three trials do not establish that the document tool improves task success.

No model called task_document in this standard suite. The same-binary candidate comparison therefore measures schema/context overhead and model variation, not document usefulness or artifact verification. Candidate total tokens rise 9.8–20.9% against the default arm across the six models, with the reported intervals excluding zero; this is an accepted cost only for explicit opt-in use, and the tool stays disabled by default. Artifact-bound verification and crash safety are tested separately with native fixtures. A document-focused live benchmark is still needed before claiming a task-completion benefit. The tool cannot claim verified completion.

Candidate Gemini's three tool-error flags are failed git inspection commands in fixture directories that are intentionally not Git repositories; every corresponding task passes the independent artifact check. A secondary comparison of default against base from these same samples flags one DeepSeek Flash path probe of the obsolete src/money.js and one Gemini pre-repair failing test, both followed by successful repair. It flags no aggregate token regression. These errors are accepted as model exploration/reproduction on successful tasks, not hidden or reclassified.

The user approved gemini-3.8-flash-high at high effort because required gemini-3.8-flash-medium is absent from the gateway; standard-model coverage remains incomplete. Distinct base/new binary paths and time-varying providers remain confounders for that comparison; the candidate/default comparison uses one binary. Runs use --no-session, so they do not test journal durability or selected-run recovery. Sample and design records preserve effective settings and model effort; raw transcripts stay ignored under .preflight/evals/agent-loop/run-control.

Full preflight on the subsequent cancellation-fix commit af35bc6faa passed 34 gates with no failures and one unchanged-TypeScript-runtime skip, including both PTY suites, packaging, and native eval smoke. The earlier e602 preflight exposed a summary cancellation dispatch race and a provider-start assumption in an abort test. The correction checks the original cancellation signal after the mandatory debit completes; its deterministic regression fails on the old implementation. Those later changes are not relabeled as measured e602 samples. macOS qualification failed at SSH authentication (exit255, publickey); no Mac execution or architecture was observed. Background-summary live comparisons on the corrected commit and selected-run recovery qualification are separate records.


Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs 0.4.0-dev+ad9c5a48c2: total tokens 17% [13.9, 21.5]
- claude-opus-5-5 vs 0.4.0-dev+e60273b96b-default: total tokens 16.7% [14.5, 20.7]
- claude-sonnet-5-5 vs 0.4.0-dev+ad9c5a48c2: total tokens 12.8% [9.4, 14.9]
- claude-sonnet-5-5 vs 0.4.0-dev+e60273b96b-default: total tokens 11.4% [6.5, 15.1]
- deepseek-flash vs 0.4.0-dev+ad9c5a48c2: total tokens 19% [12.7, 26.2]
- deepseek-flash vs 0.4.0-dev+e60273b96b-default: total tokens 20.9% [10.4, 34.9]
- deepseek-v4-pro vs 0.4.0-dev+ad9c5a48c2: total tokens 20.2% [13.4, 27]
- deepseek-v4-pro vs 0.4.0-dev+e60273b96b-default: total tokens 14.1% [7.4, 21.5]
- gemini-3.8-flash-high vs 0.4.0-dev+ad9c5a48c2: tool errors 0 -> 3
- gemini-3.8-flash-high vs 0.4.0-dev+e60273b96b-default: total tokens 17.1% [4.1, 31.3]
- gemini-3.8-flash-high vs 0.4.0-dev+e60273b96b-default: tool errors 1 -> 3
- gpt-6.1-sol vs 0.4.0-dev+ad9c5a48c2: total tokens 8.2% [1.2, 15.6]
- gpt-6.1-sol vs 0.4.0-dev+e60273b96b-default: total tokens 9.8% [4, 16.6]

## Against `0.4.0-dev+ad9c5a48c2`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +17% [13.9, 21.5] | +4.9% [1, 9.1] | 66 → 68 (+3% [0, 7.7]) | 86 → 86 | 84% → 86% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +12.8% [9.4, 14.9] | +5.1% [0.5, 10] | 67 → 66 (-1.5% [-4.5, 0]) | 80 → 78 | 86% → 87% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +19% [12.7, 26.2] | +19.3% [14.8, 24.3] | 88 → 92 (+4.5% [0, 9.9]) | 89 → 95 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +20.2% [13.4, 27] | +15.9% [13, 18.8] | 92 → 96 (+4.3% [0, 9.5]) | 97 → 104 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +12.2% [-2.8, 29.1] | +14.7% [-2, 33.6] | 72 → 73 (+1.4% [-10, 13.9]) | 104 → 105 | 0% → 0% | 0 → 3 | 0 → 0 |
| gpt-6.1-sol | 17/21 | +8.2% [1.2, 15.6] | +0.2% [-17.8, 23] | 83 → 82 (-1.2% [-6.1, 3.9]) | 69 → 68 | 63% → 66% | 0 → 0 | 4 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +13.2% [12.8, 13.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +15.2% [14.6, 16.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +14.1% [13.7, 14.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +12.9% [12.4, 13.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +35.1% [13, 47] | 9 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +15.7% [15.1, 16.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +14.4% [14, 14.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +14.3% [14.2, 14.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +15.1% [15, 15.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +14.5% [14.1, 14.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +15% [13.6, 17.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +4.4% [-11.7, 15] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +15.3% [15, 15.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +11% [8.5, 15.8] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +14.5% [11, 16.8] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +28.1% [13.3, 57.7] | 11 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +18% [12.4, 26.1] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +23.3% [14.4, 39.6] | 14 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +10.9% [3.3, 19.1] | 16 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +24.1% [-0.6, 63.3] | 12 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +17% [-12.8, 55.8] | 11 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +24.5% [15.3, 37] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +32.9% [15.7, 44.6] | 13 → 15 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +14.3% [13.4, 15] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +16.2% [13.1, 18.5] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +20% [-15.1, 44.2] | 16 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +19.1% [16.5, 23.9] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +14.5% [12.8, 15.7] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +77.4% [54.7, 106.1] | 9 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +17.3% [16.3, 18.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +38.3% [18.5, 52.3] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +8% [-22.7, 37.2] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +2.7% [-5.3, 16.4] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -19.3% [-45.1, 10.7] | 11 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -14.6% [-37, 12.2] | 11 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +9.8% [9.6, 10.1] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +0.8% [-14.3, 10.9] | 13 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 0/3 | n/a | 0 → 0 | 3 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | +8.9% [-14.4, 37.5] | 16 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +10.1% [8.6, 11.3] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +10.5% [10.1, 10.7] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +8.1% [-17.3, 42.1] | 9 → 9 | 1 → 0 |

</details>

## Against `0.4.0-dev+e60273b96b-default`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +16.7% [14.5, 20.7] | +7.8% [3.7, 12.3] | 67 → 68 (+1.5% [0, 4.7]) | 85 → 86 | 85% → 86% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +11.4% [6.5, 15.1] | +5.5% [1.2, 10] | 68 → 66 (-2.9% [-7.1, 0]) | 82 → 78 | 86% → 87% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +20.9% [10.4, 34.9] | +16.4% [10.1, 22.6] | 86 → 92 (+7% [0, 15.7]) | 89 → 95 | 76% → 77% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +14.1% [7.4, 21.5] | +17% [13, 21.3] | 96 → 96 (0% [-4, 4.3]) | 100 → 104 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +17.1% [4.1, 31.3] | +20.4% [5.9, 36.4] | 69 → 73 (+5.8% [-3.9, 15.9]) | 101 → 105 | 0% → 0% | 1 → 3 | 0 → 0 |
| gpt-6.1-sol | 16/21 | +9.8% [4, 16.6] | +15.7% [-5.2, 42.9] | 79 → 78 (-1.3% [-5.2, 2.6]) | 65 → 64 | 66% → 65% | 0 → 0 | 4 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +13.8% [13.2, 14.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +15.8% [15.5, 16.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +14.5% [14, 15.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +15.7% [12.8, 20.8] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +26.2% [14.7, 52.6] | 10 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +15.2% [14.9, 15.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +15.1% [14.5, 15.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +14.6% [14.5, 14.7] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +15.4% [15.2, 15.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +14.1% [14, 14.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +16.6% [14.4, 18] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -6.2% [-14.1, 15] | 11 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +15.8% [15.6, 16.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +11.4% [9.2, 15.9] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +24% [8.1, 62.9] | 11 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +18.2% [15.8, 21.2] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +12.1% [6, 19.2] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +3.8% [-7.1, 12.3] | 16 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +44.2% [3, 135.5] | 13 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +27% [-16.7, 115.1] | 12 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +27% [16.1, 52.2] | 10 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +7.1% [-7.7, 19.2] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +39.7% [14, 59.4] | 13 → 15 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +14.5% [13.7, 15.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +12% [9, 16.6] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +2.9% [-19.5, 17.7] | 18 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +18.7% [16.8, 22.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +14.2% [12.9, 15] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +73.4% [53.2, 109.3] | 9 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +18.8% [17.5, 19.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +21.8% [7.9, 43.8] | 10 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -0.8% [-34, 24.6] | 14 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +8.6% [-12, 31.9] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +5.6% [-2.5, 12.7] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +10% [4, 18.6] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | +9.3% [8.9, 9.7] | 12 → 12 | 1 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | +9.7% [9.2, 10.2] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -9.1% [-9.1, -9.1] | 6 → 5 | 1 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | +24.3% [9.6, 48.5] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +10.5% [10, 10.7] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | +11.2% [10.9, 11.5] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +1.2% [-14.2, 10.1] | 15 → 14 | 0 → 0 |

</details>

## `0.4.0-dev+e60273b96b-task-documents` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 7904 | 12325 | 3.24 | 86% | 0 |
| claude-sonnet-5-5 | 24/24 | 7906 | 11539 | 3.14 | 87% | 0 |
| deepseek-flash | 24/24 | 7821 | 12123 | 4.38 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 7822 | 12816 | 4.57 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 7841 | 10157 | 3.48 | 0% | 3 |
| gpt-6.1-sol | 23/24 | 7833 | 9944 | 4.85 | 66% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 26 | 20 | 1 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 30 | 21 | 5 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 7 | 0 | 1 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 34 | 20 | 0 | 20/20 | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 0 | 0.4.0-dev+e60273b96b-default | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | 0.4.0-dev+e60273b96b-default | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 0.4.0-dev+ad9c5a48c2 | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 0.4.0-dev+e60273b96b-task-documents | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+ad9c5a48c2 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 0.4.0-dev+ad9c5a48c2 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 0.4.0-dev+e60273b96b-default | runaway |
| gpt-6.1-sol | stale_edit | 2 | 0.4.0-dev+e60273b96b-default | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 0.4.0-dev+ad9c5a48c2 | runaway |
