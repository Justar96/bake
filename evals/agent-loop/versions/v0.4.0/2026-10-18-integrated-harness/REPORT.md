# Integrated harness defaults

Candidate `0.4.0-dev+e6e4469230-default` at `e6e4469230`, compared with `0.4.0-dev+41eef5020f` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: background_test, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-17T22:35:34.282Z.

This final default-policy comparison retains all 324 unique samples: the eight standard scenarios plus background_test, six available models, three trials, and two arms. The clean base is 41eef5020ff1b8bfa8ffb07b7576e2f3b8600f3e, binary SHA-256 f0fef4d7353c5016c37430d8344390a2b54ba00ab62ff7ef7ab4f1cd9c32bd97. The clean integrated candidate is e6e4469230dd73caf97354640e1f214dbf287461, binary SHA-256 36b0bb74eb3b57de7c59e1eaac97b3bcc5a6c673be7858f59f57484be043432a. Both checkouts remained clean and both binary hashes matched after the run. Evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605 supplied the corrected usage accounting. Later test-only and documentation commits are not relabeled as measured source.

Each arm passes 158/162 samples. Opus, DeepSeek Flash, DeepSeek Pro, and Gemini pass 27/27 in each arm; Sonnet passes 26/27 and GPT 24/27 in each. No sample is evaluator-invalid. All six GPT failures are streamed-edit whitespace-guard stops, three per arm, and their independent code checks fail. The base exits zero on those model failures; the candidate returns a failed outcome and exits one. Sonnet candidate background_test trial 0 and base trial 2 both pass the independent code check with protected files unchanged, but a model-generated shell background command is followed by a 60-second process-name wait timeout, then a provider connection reset reported as a 502. Both end without a final reply or the required slow-check report. They remain failed tasks despite the passing code check; the base exits zero and the candidate exits one. The eight failed samples retain their partial counters and are excluded from paired cost comparisons. analysis.json preserves process exits, independent test exits, final-reply presence, and typed outcomes separately.

The Opus total-token increase remains flagged: 307825 → 309666 tokens, +0.6% [0.1, 1.4], across 24 successful task pairs. Requests are unchanged at 80, with tool calls 104 → 105. Of the 1841 additional tokens, 1060 occur in path_discovery trial 2, where the candidate emits a full-path find listing and the base a shorter ls listing; the candidate also reads one extra file in shell_then_edit trial 2. Executable-path differences and model variation remain confounders. These observations explain where the measured increase occurs, but its causal attribution to harness changes is unresolved. This record makes no efficiency claim and does not dismiss the positive interval as equivalence.

The other aggregate flag is Sonnet tool errors 0 → 2 within eligible pairs. Both candidate errors occur in background_test trial 1: a 60-second process-name wait times out, then a command prints the slow-check output but its process probe exits nonzero. The model ultimately supplies the required report and the independent checks pass. The timeout errors in the two failed Sonnet samples remain in per-sample data but are outside eligible paired cost totals. The successful-task repair overhead is retained and explained as observed shell orchestration, without attributing its frequency to a harness change. Unflagged tool errors also remain: Flash probes an absent package.json once per arm; Gemini has four errors per arm from pre-repair assertions and git inspection in non-Git fixtures. GPT's eligible uncached input rises 18.5% [2.2, 36.5] while its cache-read share falls from 70% to 63%; that metric is visible even though the repository's regression rule does not flag it. Pricing was not established, so token differences are not dollar-cost estimates.

The twelve retained first-request pairs, covering no_tools trial 0 and ordinary_edit trial 0 for all six models, contain exactly the same seven tools in each arm: read, bash, edit, write, job_output, job_list, and job_kill. Their full schemas match, and their system prompts match after replacing only the measured executable paths and temporary working directories. First-request schema character counts also match across all 162 pairs. Effective settings contain only the evaluator's disabled retry policy. Task documents, context masking, background compaction, and bounded-read scheduling remain at their defaults; no task_document call, masking commit, foreground summary, or background summary occurs. Native read concurrency is unbounded in both arms. These observations establish the measured default configuration, not the usefulness of the disabled features.

Every sample runs with --no-session: none persists a session or records a recoverable run. The evaluator caps standard cases at 14 requests and 180000 ms and background_test at 20 requests and 240000 ms; no explicit native RunControl limits are supplied. Background_test passes 17/18 per arm. Managed background starts occur in 17 base samples and 16 candidate samples; the remaining cases use shell background commands. These ordinary task runs do not test crash recovery, durable job adoption, selected-run reconciliation, approval renewal, bounded shared-run budgets, or artifact-bound task-document verification. Passing the independent artifact predicate is distinct from successful agent completion and does not establish a general task-completion guarantee.

All 324 accounting objects were independently reconciled with the corrected evaluator and matched exactly; all 316 successful samples have complete usage. Paired token comparisons require both runs to succeed and both usage records to be complete. No raw sample was changed and no model was rerun. The user-approved gemini-3.8-flash-high substitution runs at high effort because required gemini-3.8-flash-medium is unavailable, so required standard-model coverage remains incomplete. DeepSeek also runs at high effort and the other models at medium. provenance.json preserves exact measured revisions, binary hashes, settings, design hashes, sample/event hashes, and retained first-request hashes. Raw transcripts stay private.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs 0.4.0-dev+41eef5020f: total tokens 0.6% [0.1, 1.4]
- claude-sonnet-5-5 vs 0.4.0-dev+41eef5020f: tool errors 0 -> 2

## Against `0.4.0-dev+41eef5020f`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | +0.6% [0.1, 1.4] | +1.6% [0.4, 3.7] | 80 → 80 (0% [0, 0]) | 104 → 105 | 87% → 87% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 22/24 | +1.6% [-0.6, 5.1] | +0.4% [-2.9, 3.4] | 72 → 73 (+1.4% [0, 4.2]) | 87 → 87 | 88% → 88% | 0 → 2 | 1 → 1 |
| deepseek-flash | 24/24 | -2.1% [-8.6, 4.9] | -0.8% [-4.2, 2.9] | 104 → 103 (-1% [-5.6, 4.2]) | 103 → 99 | 77% → 77% | 1 → 1 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -2.4% [-8.9, 4.6] | +1.1% [-2.7, 5.1] | 113 → 111 (-1.8% [-7.2, 4.6]) | 125 → 124 | 82% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 24/24 | +4.3% [-6.7, 17.1] | +1.9% [-10.2, 17] | 82 → 82 (0% [-11.8, 12.8]) | 124 → 122 | 0% → 0% | 4 → 4 | 0 → 0 |
| gpt-6.1-sol | 19/24 | -2.9% [-7.1, 0.6] | +18.5% [2.2, 36.5] | 98 → 95 (-3.1% [-6.5, 0]) | 82 → 79 | 70% → 63% | 0 → 0 | 3 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | background_test | 3/3 | +0.4% [0, 0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.2% [0.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +0.3% [0.1, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.2% [-0.5, 0.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +2.1% [-0.5, 6.7] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +0.6% [0.1, 1.5] | 11 → 11 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +0.3% [-0.3, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.1% [0, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | background_test | 1/3 | +27.7% [27.7, 27.7] | 4 → 5 | 1 → 1 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | +0.1% [0, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | +0.1% [0, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | +0.2% [0.1, 0.4] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.9% [-0.1, 2.5] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +0.2% [0, 0.5] | 11 → 11 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | 0% [-0.3, 0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -1.6% [-5.1, 5.8] | 9 → 9 | 0 → 0 |
| deepseek-flash | background_test | 3/3 | -4.7% [-21.1, 9.8] | 16 → 15 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +12.9% [-29.8, 49.2] | 10 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +3.1% [-0.3, 6.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +0.3% [-0.4, 1.8] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +5.1% [-11.1, 24.7] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -12.3% [-15.8, -7.3] | 16 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -8.4% [-11.4, -1.7] | 13 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -7.5% [-29, 18.8] | 10 → 10 | 0 → 0 |
| deepseek-v4-pro | background_test | 3/3 | -12.4% [-23.3, 7.4] | 18 → 16 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | 0% [-2.2, 4.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +10.1% [-18.9, 29.2] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -1.1% [-3.9, 2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -8.3% [-19.4, -1.3] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +7.8% [-20.7, 33.4] | 17 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.1% [-0.8, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -9.8% [-24.8, 0.1] | 13 → 12 | 0 → 0 |
| gemini-3.8-flash-high | background_test | 3/3 | -11.9% [-31.9, 5] | 13 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -0.7% [-39.9, 98.6] | 12 → 11 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +3.4% [2.9, 4.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +22.4% [11.8, 38.9] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +5.7% [-0.7, 15.2] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +3.8% [-17.7, 25.7] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +19.2% [-15.6, 72.9] | 9 → 11 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +16.2% [3.3, 37.3] | 9 → 10 | 0 → 0 |
| gpt-6.1-sol | background_test | 3/3 | 0% [-0.2, 0.3] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | 0% [-0.2, 0.3] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -12.8% [-22.7, -0.1] | 9 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | +5% [5, 5] | 5 → 5 | 2 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | -5.8% [-19.1, 5.5] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +0.6% [0.2, 0.8] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +0.4% [0.3, 0.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 1/3 | -22% [-22, -22] | 5 → 4 | 1 → 1 |

</details>

## `0.4.0-dev+e6e4469230-default` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 8059 | 12903 | 3.33 | 87% | 0 |
| claude-sonnet-5-5 | 26/27 | 8061 | 12509 | 3.35 | 88% | 3 |
| deepseek-flash | 27/27 | 7933 | 11716 | 4.29 | 77% | 1 |
| deepseek-v4-pro | 27/27 | 7934 | 13033 | 4.63 | 82% | 0 |
| gemini-3.8-flash-high | 27/27 | 7927 | 11191 | 3.42 | 0% | 4 |
| gpt-6.1-sol | 24/27 | 7919 | 10344 | 5 | 64% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24 | 2 | 0 | 0 | 24/24 | 0 |
| claude-sonnet-5-5 | 24 | 3 | 0 | 0 | 24/24 | 0 |
| deepseek-flash | 24 | 25 | 19 | 0 | 24/24 | 0 |
| deepseek-v4-pro | 24 | 33 | 24 | 2 | 24/24 | 0 |
| gemini-3.8-flash-high | 24 | 7 | 0 | 0 | 24/24 | 0 |
| gpt-6.1-sol | 24 | 36 | 21 | 0 | 21/21 | 3 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-sonnet-5-5 | background_test | 0 | 0.4.0-dev+e6e4469230-default | exit 1 |
| claude-sonnet-5-5 | background_test | 2 | 0.4.0-dev+41eef5020f | empty final reply |
| gpt-6.1-sol | multi_site_edit | 1 | 0.4.0-dev+e6e4469230-default | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+41eef5020f | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 0.4.0-dev+e6e4469230-default | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 0.4.0-dev+41eef5020f | runaway |
| gpt-6.1-sol | unprompted_edit | 0 | 0.4.0-dev+41eef5020f | runaway |
| gpt-6.1-sol | unprompted_edit | 2 | 0.4.0-dev+e6e4469230-default | runaway |
