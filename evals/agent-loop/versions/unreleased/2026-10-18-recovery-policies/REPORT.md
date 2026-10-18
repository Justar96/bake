# Recoverable execution and opt-in policy comparison

Candidate `0.4.0-dev+737ce81265-recoverable` at `737ce81265`, compared with `0.4.0-dev+af35bc6faa` and `0.4.0-dev+737ce81265-default` and `0.4.0-dev+737ce81265-masking-read4` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T22:11:35.931Z.

This four-arm native run retains all 576 unique samples: eight standard scenarios, three trials, six available models, and four arms. The clean base is af35bc6faaafce414bb3c3e5d46aa7d963c1c21e, binary SHA-256 932c77b11dc95a14e6e712daab4751324851f10f531b5b44b90c2bd8c8fd4e04. Default, recovery, and optimized use the same clean 737ce81265c941fd17402a305952270097f94007 binary, SHA-256 67671d9e1fdc7cbe8320f34ba77988989c86383a04300b5382e350769b2df7c9. These are historical measurements of those binaries, not the later integrated source. The default and base arms use persistent sessions. Recovery starts a recoverable run with the recorded 14-model-call and 180000 ms limits. Optimized uses persistent sessions with contextMasking enabled and maxConcurrentReadTools set to 4; it does not enable recoverable execution. Background compaction and task documents remain disabled.

Five models pass all 24 samples in every arm. GPT passes 22/24 base, 21/24 default, 19/24 recovery, and 18/24 optimized. All 16 failed samples stop on the existing tool-argument whitespace guard while GPT streams malformed edit arguments; each exits one and has incomplete usage. No sample is reclassified as evaluator-invalid. Recovery's higher failure counts against base and default remain regression flags. The secondary optimized comparisons also flag its higher failures against both arms. These outcomes prevent claims of improved success or general efficiency; the observed stream failure mechanism does not establish why the opt-in arms failed more often in this small run.

Every recovery sample persisted one run, totaling 2638 journal entries. This measures ordinary recoverable-run execution and durable recording overhead; it does not measure crash restart, selected-run reconciliation, approval renewal, or effect replay. No context masking, foreground summary, or background summary occurred. Optimized produced 47 multi-call read batches, with at most three calls in a batch, so the configured limit of four never constrained a batch. This suite therefore provides no evidence for masking effectiveness or throughput gains from the concurrency limit.

Recovery has no aggregate token regression against base or same-binary default. Against optimized, Sonnet uses 5.1% more total tokens [0.6, 10.3] and 69 rather than 66 requests across successful task pairs. That measured variation is retained without attributing it to journal writes or masking. Gemini's recovery tool errors are a failing assertion before repair and a git command in a fixture that is not a Git repository; both occur in an ultimately successful task. Secondary default-versus-base flags are Flash's nonexistent-path probe, Pro's assertion before a correct rounding repair, and Gemini's git inspection. Optimized's Gemini flags are two git inspections in non-Git fixtures. Repair and orientation overhead remain counted. These explained tool-error flags are accepted as observed model behavior on successful tasks, while the GPT failure regressions remain a limitation. The complete secondary default-versus-base and optimized-versus-base/default comparisons are retained in comparisons.json from the same samples, not additional runs.

The user approved gemini-3.8-flash-high at high effort because required gemini-3.8-flash-medium was unavailable; required standard-model coverage remains incomplete. DeepSeek uses high effort and the other models medium. All 576 accounting objects were reconciled with the corrected terminal-usage implementation from evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605 and matched exactly. The 16 failed samples remain excluded from paired token comparisons. Pricing was not established, so token changes are not dollar-cost estimates. Base/new executable paths and time-varying providers also limit causal attribution; the three new arms share one binary. provenance.json preserves measured revisions, binary hashes, effective modes, route effort, and hashes of raw samples, event logs, and designs; raw transcripts remain private.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5-5 vs 0.4.0-dev+737ce81265-masking-read4: total tokens 5.1% [0.6, 10.3]
- gemini-3.8-flash-high vs 0.4.0-dev+af35bc6faa: tool errors 0 -> 2
- gemini-3.8-flash-high vs 0.4.0-dev+737ce81265-default: tool errors 1 -> 2
- gpt-6.1-sol vs 0.4.0-dev+af35bc6faa: failures 2 -> 5
- gpt-6.1-sol vs 0.4.0-dev+737ce81265-default: failures 3 -> 5

## Against `0.4.0-dev+af35bc6faa`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.9% [-4.2, 1.5] | +1.1% [-0.7, 3.9] | 68 → 67 (-1.5% [-4.4, 0]) | 86 → 86 | 85% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +4.4% [-0.2, 9.6] | +0.2% [-0.9, 1.4] | 66 → 69 (+4.5% [0, 9.4]) | 80 → 83 | 85% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -5.9% [-13, 2.1] | -2.7% [-5.5, -0.1] | 92 → 87 (-5.4% [-11, 1.1]) | 91 → 86 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +2.3% [-4.4, 9.3] | +1% [-1.9, 4] | 93 → 96 (+3.2% [-2.1, 8.7]) | 103 → 105 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -0.9% [-11.4, 10.6] | +0.7% [-9.6, 13] | 68 → 69 (+1.5% [-5.8, 10]) | 99 → 99 | 0% → 0% | 0 → 2 | 0 → 0 |
| gpt-6.1-sol | 16/21 | -0.5% [-6.7, 5.5] | -0.7% [-15.7, 18.8] | 81 → 81 (0% [-3.6, 3.8]) | 67 → 67 | 65% → 65% | 0 → 0 | 2 → 5 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -0.2% [-1, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.2% [-0.7, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.4% [-0.4, 1.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +2.6% [-0.3, 7.9] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -8.7% [-24.8, 0.8] | 11 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.5, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.3% [-1.6, 0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | 0% [-0.1, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.2% [-0.3, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.1% [-0.1, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.9% [-2.4, -0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +34% [33.6, 34.3] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.1% [-0.3, 0.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -0.1% [-0.1, 0] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +1.3% [-28.2, 41.2] | 11 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +0.8% [-3.2, 3] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -9.3% [-25.8, -1.1] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -3.1% [-7.9, 1.5] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -1.8% [-27.3, 30.9] | 15 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -24.5% [-31.8, -17.4] | 15 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -1.4% [-6.3, 1.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +8.4% [0.2, 23.4] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +0.1% [-23.2, 29.6] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.6% [-1.2, 2.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +3.4% [-8.2, 18.2] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -2.5% [-27.7, 25.1] | 17 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +8.9% [-7.4, 34.3] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -0.1% [-4.4, 3.5] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -0.2% [-8.5, 8.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +0.1% [-0.9, 1.4] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +11.9% [-29.7, 78.2] | 10 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +10.4% [-11.1, 36.3] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -17.3% [-31.9, 11.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -14.2% [-32.9, 12.7] | 10 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +11.9% [5, 21.7] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -0.3% [-4.2, 3.8] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 1/3 | +0.9% [0.9, 0.9] | 4 → 4 | 1 → 2 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +9.3% [0.2, 27.4] | 15 → 16 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -9.2% [-29.4, 10.6] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -0.2% [-0.6, 0.6] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 1/3 | +0.3% [0.3, 0.3] | 4 → 4 | 1 → 2 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -1.5% [-3.9, 0.8] | 15 → 15 | 0 → 0 |

</details>

## Against `0.4.0-dev+737ce81265-default`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.1% [-0.3, 0.2] | -0.1% [-0.8, 0.6] | 67 → 67 (0% [0, 0]) | 85 → 86 | 85% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +4.5% [-0.1, 10] | +0.6% [-2.2, 3.3] | 66 → 69 (+4.5% [0, 9.4]) | 80 → 83 | 85% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -6.1% [-11.9, 1.1] | -2.9% [-6, 0.1] | 90 → 87 (-3.3% [-8.8, 2.3]) | 96 → 86 | 77% → 77% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -1.8% [-9.8, 6.3] | -0.1% [-4.5, 4.6] | 97 → 96 (-1% [-6.9, 5.2]) | 103 → 105 | 82% → 81% | 1 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +3.2% [-2.3, 11.1] | +4.1% [-1.3, 12.9] | 67 → 69 (+3% [0, 9.2]) | 98 → 99 | 0% → 0% | 1 → 2 | 0 → 0 |
| gpt-6.1-sol | 13/21 | -1% [-3.4, 1.5] | -4.8% [-20.2, 15.4] | 65 → 65 (0% [0, 0]) | 53 → 54 | 65% → 66% | 0 → 0 | 3 → 5 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.2% [-0.1, 0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.3% [-0.1, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.5% [-0.1, 1.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.2% [-0.6, 0.3] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -0.8% [-1.2, -0.5] | 10 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +0.3% [0.1, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.8% [-1, -0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +0.1% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.2% [0, 0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.8% [-2.6, 0.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +34% [33.7, 34.2] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.1% [-0.1, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | 0% [-6.1, 6.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -9.1% [-24.9, -1.3] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +0.9% [-1.1, 4.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.2% [-27.3, 27.9] | 11 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -10.9% [-17.7, -5.7] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -6.9% [-30, 28.1] | 15 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -11.6% [-18.7, -3.9] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +1.3% [-0.4, 3.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -9.8% [-26.3, 25.7] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +3.3% [-23, 30.5] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.4% [-0.8, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +5% [-2.2, 17.9] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -13% [-33.9, 16.5] | 19 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +9.8% [-5.2, 35.6] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -0.4% [-4.6, 5.1] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +0.7% [-5.4, 6.8] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -1.3% [-3.1, 0.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +19.1% [-16.8, 74.2] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -1.4% [-8.1, 3.9] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -1.2% [-7.8, 6.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +2.7% [-7.5, 18] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +9.1% [3.4, 12.4] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | +1.3% [-4.9, 8.1] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 1/3 | +0.7% [0.7, 0.7] | 4 → 4 | 0 → 2 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -1.6% [-1.6, -1.6] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | -5.2% [-9.8, -0.5] | 10 → 10 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -0.1% [-0.8, 0.6] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 1/3 | -0.4% [-0.4, -0.4] | 4 → 4 | 0 → 2 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -1% [-4, 0.9] | 15 → 15 | 0 → 0 |

</details>

## Against `0.4.0-dev+737ce81265-masking-read4`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +1.8% [-0.3, 5.2] | +1.3% [-0.4, 3.6] | 66 → 67 (+1.5% [0, 4.7]) | 84 → 86 | 85% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +5.1% [0.6, 10.3] | +2.1% [0.5, 4.2] | 66 → 69 (+4.5% [0, 9.4]) | 79 → 83 | 86% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -2.9% [-9.7, 4.6] | -1.4% [-4.3, 1.9] | 90 → 87 (-3.3% [-8.8, 2.3]) | 89 → 86 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +3% [-3, 9.2] | +1.5% [-1.2, 4.5] | 94 → 96 (+2.1% [-3, 7.3]) | 98 → 105 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +3.1% [-6, 13.3] | +2.9% [-7, 14.5] | 68 → 69 (+1.5% [-5.6, 9.5]) | 100 → 99 | 0% → 0% | 2 → 2 | 0 → 0 |
| gpt-6.1-sol | 11/21 | -1% [-9.5, 6.4] | +4% [-9.9, 19.6] | 58 → 58 (0% [-4.9, 5.5]) | 48 → 49 | 66% → 65% | 0 → 0 | 6 → 5 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.1% [-0.5, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.4% [0, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | 0% [-0.5, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +1.8% [-0.3, 6.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +10.1% [-3.1, 32.7] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.2% [-1.3, 0.8] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.3, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.4, 0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.9% [-0.1, 2.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +33.9% [33.5, 34.2] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +0.2% [-0.1, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +2.1% [0, 6.4] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +2.3% [-26.5, 40.3] | 11 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +0.7% [-0.9, 3] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -8.2% [-26.8, 1.5] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -1.3% [-8.4, 11.2] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -3.6% [-32.5, 28.1] | 15 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -11.1% [-18.2, 0.6] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +1.5% [-0.4, 4.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +9.6% [-0.1, 25.6] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +4.8% [0.7, 12.9] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.1% [-1, 0.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -2.9% [-22.4, 18.3] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +0.3% [-18.4, 21] | 17 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +11.8% [-0.3, 35.9] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +2.9% [2.1, 4.4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +0.8% [-9.8, 14.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -2.8% [-4.1, -2.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +26.4% [-9.1, 91.9] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -2.6% [-28.9, 26.2] | 13 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +9.3% [4.4, 12.5] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -4.5% [-11.4, 6.1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -2.6% [-20.9, 20.9] | 10 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | +3.9% [3.8, 3.9] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 0/3 | n/a | 0 → 0 | 1 → 2 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -0.4% [-0.4, -0.4] | 6 → 6 | 2 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -11.9% [-29.4, 7] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -0.1% [-1.1, 0.7] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 0/3 | n/a | 0 → 0 | 2 → 2 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +12.8% [0.9, 28] | 9 → 10 | 1 → 0 |

</details>

## `0.4.0-dev+737ce81265-recoverable` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6806 | 10641 | 3.19 | 85% | 0 |
| claude-sonnet-5-5 | 24/24 | 6808 | 10537 | 3.29 | 86% | 0 |
| deepseek-flash | 24/24 | 6694 | 9843 | 4.14 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6695 | 11240 | 4.57 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6727 | 8628 | 3.29 | 0% | 2 |
| gpt-6.1-sol | 19/24 | 6719 | 9389 | 5.06 | 65% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 1 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 3 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 21 | 18 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 30 | 21 | 2 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 3 | 0 | 2 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 30 | 16 | 1 | 16/16 | 5 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 1 | 0.4.0-dev+737ce81265-recoverable | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | 0.4.0-dev+af35bc6faa | runaway |
| gpt-6.1-sol | multi_site_edit | 0 | 0.4.0-dev+737ce81265-recoverable | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
| gpt-6.1-sol | multi_site_edit | 2 | 0.4.0-dev+737ce81265-recoverable | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+737ce81265-default | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 0.4.0-dev+737ce81265-default | runaway |
| gpt-6.1-sol | path_discovery | 1 | 0.4.0-dev+737ce81265-default | runaway |
| gpt-6.1-sol | stale_edit | 0 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
| gpt-6.1-sol | stale_edit | 0 | 0.4.0-dev+737ce81265-recoverable | runaway |
| gpt-6.1-sol | stale_edit | 1 | 0.4.0-dev+af35bc6faa | runaway |
| gpt-6.1-sol | stale_edit | 1 | 0.4.0-dev+737ce81265-recoverable | runaway |
| gpt-6.1-sol | stale_edit | 2 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
| gpt-6.1-sol | unprompted_edit | 1 | 0.4.0-dev+737ce81265-masking-read4 | runaway |
