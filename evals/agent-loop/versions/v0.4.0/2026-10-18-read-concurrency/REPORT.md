# Bounded concurrent native reads

Candidate `0.4.0-dev+737ce81265-read4` at `737ce81265`, compared with `0.4.0-dev+737ce81265-read1` and `0.4.0-dev+737ce81265-read-unbounded` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: independent_reads. Trials: 3. Started: 2026-10-17T22:18:21.906Z.

This focused three-arm run retains all 54 samples: six available models, three trials, and the independent_reads scenario. Every arm uses clean 737ce81265c941fd17402a305952270097f94007, binary SHA-256 67671d9e1fdc7cbe8320f34ba77988989c86383a04300b5382e350769b2df7c9. The base sets maxConcurrentReadTools to 1, the candidate sets it to 4, and the third arm leaves the default unbounded. Background compaction is disabled in all arms. This measures those recorded settings on the frozen binary, not the later integrated source.

Opus, Sonnet, DeepSeek Flash, DeepSeek Pro, and Gemini pass all nine samples each, preserving the required facts, protected fixtures, and final check. GPT passes 1/3 serial, 1/3 bounded, and 2/3 unbounded. Three GPT failures stop on the existing streamed-write whitespace guard; serial and bounded trial 2 reach the 180000 ms cap while streaming repetitive write arguments. No failure is reclassified as evaluator-invalid. These failures remain in the record; their mechanism does not implicate read scheduling. No regression is flagged under the repository rule, which does not establish behavioral equivalence from three trials.

All 45 non-GPT samples submit at least one multi-read batch, with recorded maximum widths from four to six. GPT submits only single-read batches, so its trials do not exercise concurrent reads. The batch metric measures the number of submitted calls, not the number of simultaneously active workers. Of 288 logged read results, 273 have a duration of zero milliseconds, 13 one millisecond, and two two milliseconds. This tiny-file scenario cannot establish a filesystem throughput improvement or measure the active-worker ceiling. Model round trips, generation, and cache variation dominate total elapsed time; the deterministic scheduler tests supply separate evidence for the configured ceiling. No sample repeats an identical read argument set. The observations support compatibility and fact preservation on this fixture, with no general speedup claim.

All 54 accounting objects were reconciled against the corrected terminal-usage implementation from evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605 and matched exactly. Original raw files were not changed and no eligibility downgrade was required. Paired token comparisons require both tasks to succeed and both usage records to be complete; failed samples retain partial counters but do not enter those comparisons. Pricing was not established, so token differences are not dollar-cost estimates. The user-approved Gemini high-effort substitution leaves the required but unavailable Gemini medium model unmeasured. DeepSeek runs at high effort and the other models at medium. provenance.json records all 114 raw sample, event-log, and design hashes, measured revisions and binaries, settings, and accounting verification; observations.json preserves aggregate activation and repeat-read counters without transcripts. No sample was rerun.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `0.4.0-dev+737ce81265-read1`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | -0.1% [-0.2, 0.1] | -6.2% [-16.7, 0.1] | 9 → 9 (0% [0, 0]) | 24 → 24 | 81% → 82% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 3/3 | 0% [-0.4, 0.3] | -6.3% [-16.7, 0.1] | 9 → 9 (0% [0, 0]) | 24 → 24 | 81% → 82% | 0 → 0 | 0 → 0 |
| deepseek-flash | 3/3 | -6% [-20.2, 6.4] | +0.3% [-5.2, 6.5] | 13 → 12 (-7.7% [-20, 0]) | 27 → 27 | 78% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | +4% [-21, 22.4] | -4.4% [-9.5, 5] | 13 → 14 (+7.7% [-20, 25]) | 27 → 25 | 82% → 84% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 3/3 | -3.8% [-10.9, 9.5] | -3% [-10.1, 8.9] | 12 → 12 (0% [0, 0]) | 26 → 26 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 2 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | independent_reads | 3/3 | -0.1% [-0.2, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | independent_reads | 3/3 | 0% [-0.4, 0.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | independent_reads | 3/3 | -6% [-20.2, 6.4] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | independent_reads | 3/3 | +4% [-21, 22.4] | 13 → 14 | 0 → 0 |
| gemini-3.8-flash-high | independent_reads | 3/3 | -3.8% [-10.9, 9.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | independent_reads | 0/3 | n/a | 0 → 0 | 2 → 2 |

</details>

## Against `0.4.0-dev+737ce81265-read-unbounded`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | -0.2% [-0.6, 0] | -0.5% [-0.8, 0.1] | 9 → 9 (0% [0, 0]) | 24 → 24 | 82% → 82% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 3/3 | 0% [-0.2, 0.4] | +0.1% [0, 0.1] | 9 → 9 (0% [0, 0]) | 24 → 24 | 82% → 82% | 0 → 0 | 0 → 0 |
| deepseek-flash | 3/3 | -7.6% [-22, 1.8] | -0.3% [-5.2, 5] | 13 → 12 (-7.7% [-20, 0]) | 27 → 27 | 79% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | -3.8% [-16.4, 4.5] | +1.1% [-1.6, 4.5] | 15 → 14 (-6.7% [-20, 0]) | 24 → 25 | 84% → 84% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 3/3 | +2.8% [-5.2, 14.5] | +2.1% [-4.4, 13] | 12 → 12 (0% [0, 0]) | 25 → 26 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 1 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | independent_reads | 3/3 | -0.2% [-0.6, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | independent_reads | 3/3 | 0% [-0.2, 0.4] | 9 → 9 | 0 → 0 |
| deepseek-flash | independent_reads | 3/3 | -7.6% [-22, 1.8] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | independent_reads | 3/3 | -3.8% [-16.4, 4.5] | 15 → 14 | 0 → 0 |
| gemini-3.8-flash-high | independent_reads | 3/3 | +2.8% [-5.2, 14.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | independent_reads | 0/3 | n/a | 0 → 0 | 1 → 2 |

</details>

## `0.4.0-dev+737ce81265-read4` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 10854 | 3 | 82% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 10865 | 3 | 82% | 0 |
| deepseek-flash | 3/3 | n/a | 9867 | 4 | 77% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 11608 | 4.67 | 84% | 0 |
| gemini-3.8-flash-high | 3/3 | n/a | 12938 | 4 | 0% | 0 |
| gpt-6.1-sol | 1/3 | n/a | 21610 | 11 | 83% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 0 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 0 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 3 | 3 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 5 | 3 | 0 | 3/3 | 0 |
| gemini-3.8-flash-high | 3 | 3 | 0 | 0 | 3/3 | 0 |
| gpt-6.1-sol | 3 | 15 | 1 | 0 | 1/2 | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | independent_reads | 0 | 0.4.0-dev+737ce81265-read1 | runaway |
| gpt-6.1-sol | independent_reads | 0 | 0.4.0-dev+737ce81265-read-unbounded | runaway |
| gpt-6.1-sol | independent_reads | 1 | 0.4.0-dev+737ce81265-read4 | runaway |
| gpt-6.1-sol | independent_reads | 2 | 0.4.0-dev+737ce81265-read1 | wall_clock_limit |
| gpt-6.1-sol | independent_reads | 2 | 0.4.0-dev+737ce81265-read4 | wall_clock_limit |
