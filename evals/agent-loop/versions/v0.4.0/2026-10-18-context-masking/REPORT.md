# Context masking with foreground compaction fallback

Candidate `0.4.0-dev+737ce81265-masking` at `737ce81265`, compared with `0.4.0-dev+737ce81265-foreground` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: context_masking. Trials: 3. Started: 2026-10-17T22:19:00.955Z.

This focused paired run retains all 36 samples: six available models, three trials, and the context_masking scenario. Both arms use clean 737ce81265c941fd17402a305952270097f94007, binary SHA-256 67671d9e1fdc7cbe8320f34ba77988989c86383a04300b5382e350769b2df7c9. The base leaves contextMasking disabled; the candidate enables it with foreground compaction fallback. Background compaction is disabled in both arms. The context window is 40000 tokens, the reserve 16384, the retained tail 2560, and both arms have the same 46-request and 540000 ms evaluator caps. These are measurements of the frozen binary and recorded settings, not the later integrated source.

The candidate passes all 18 samples; the base passes 17/18. Every successful sample preserves the three required facts, protected fixtures, and the final code check. GPT base trial 2 fails on the existing streamed-write whitespace guard; it remains a failed task with incomplete usage. Flash base trial 1 makes an exact-text edit that fails, then repairs it and passes. No sample is evaluator-invalid. One extra successful candidate sample does not establish a reliability improvement from three trials per model.

Masking activates in all 12 DeepSeek, Gemini, and GPT candidate samples, producing 184 commits that replace 184 results and remove 1584652 bytes across those samples. Flash, Pro, Gemini, and GPT contribute 46, 45, 43, and 50 commits. Their candidate samples issue no foreground summaries, versus three, three, four, and three respectively in their baseline arms. Opus and Sonnet never activate masking: the protected latest eight complete assistant/tool batches leave no eligible observation before foreground compaction occurs. Both arms make nine foreground summaries per Anthropic model. These six configured-but-inactive candidate trials remain recorded; their token differences cannot be credited to masking. No fixture or policy was retuned to force activation.

Across successful, usage-complete pairs, total tokens change by +8.6% for Flash, +7.7% for Pro, +3.9% for Gemini, and -4.7% for GPT; GPT has only two eligible pairs. The Pro increase is the sole repository-rule regression, +7.7% [6.2, 9.1], and remains flagged. Cache-read shares fall from 86% to 34%, 85% to 35%, 44% to 5%, and 85% to 27% respectively. Uncached input rises by 401.8%, 383.0%, 78.5%, and 366.3%. Fewer summary calls therefore do not establish lower token cost: the measured loss of cache reuse is substantial. The Pro regression is retained as a measured cost of this default-off experiment, not treated as an efficiency improvement. Pricing was not established, so these counters do not estimate dollar cost. Gateway and model variation limit latency attribution.

No candidate repeats an identical read or grep argument set. Gemini baseline re-reads the protected requirements once per trial, while its candidate reads them once per trial. Flash baseline repeats one source read after its failed edit; its candidate adds one distinct DONE grep across the three trials. These observations support retaining the protected facts in this fixture. They do not establish that arbitrary older masked facts remain recoverable or that masking alone can replace compaction. The result supports keeping masking as an opt-in experiment with foreground fallback; it does not support enabling it by default.

All 36 accounting objects were reconciled against the corrected terminal-usage implementation from evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605 and matched exactly. The raw files were not changed and no accounting downgrade was required. Paired token comparisons require both tasks to succeed and both usage records to be complete. The user-approved Gemini high-effort substitution leaves the unavailable required Gemini medium model unmeasured. DeepSeek runs at high effort and the other models at medium. provenance.json retains all 78 raw sample/event/design hashes, measured source and binary identities, effective settings, and accounting verification. observations.json preserves activation and repeat-read counters without transcripts. No sample was rerun.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-v4-pro vs 0.4.0-dev+737ce81265-foreground: total tokens 7.7% [6.2, 9.1]

## Against `0.4.0-dev+737ce81265-foreground`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | -0.6% [-2.5, 1.9] | -1.4% [-3.7, 1.7] | 75 → 75 (0% [0, 0]) | 76 → 75 | 72% → 72% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 3/3 | -1.8% [-2.9, -0.1] | -0.7% [-2, 0.1] | 77 → 75 (-2.6% [-3.8, 0]) | 75 → 75 | 73% → 73% | 0 → 0 | 0 → 0 |
| deepseek-flash | 3/3 | +8.6% [-0.9, 19.8] | +401.8% [397.2, 404.3] | 76 → 75 (-1.3% [-7.7, 4.2]) | 78 → 79 | 86% → 34% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | +7.7% [6.2, 9.1] | +383% [375.9, 389] | 76 → 74 (-2.6% [-4, 0]) | 75 → 78 | 85% → 35% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 3/3 | +3.9% [-6.9, 10.9] | +78.5% [66.3, 89.2] | 79 → 73 (-7.6% [-14.3, 0]) | 80 → 77 | 44% → 5% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 2/3 | -4.7% [-10.2, 1.4] | +366.3% [358.2, 375.1] | 58 → 53 (-8.6% [-13.3, -3.6]) | 54 → 51 | 85% → 27% | 0 → 0 | 1 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | context_masking | 3/3 | -0.6% [-2.5, 1.9] | 75 → 75 | 0 → 0 |
| claude-sonnet-5-5 | context_masking | 3/3 | -1.8% [-2.9, -0.1] | 77 → 75 | 0 → 0 |
| deepseek-flash | context_masking | 3/3 | +8.6% [-0.9, 19.8] | 76 → 75 | 0 → 0 |
| deepseek-v4-pro | context_masking | 3/3 | +7.7% [6.2, 9.1] | 76 → 74 | 0 → 0 |
| gemini-3.8-flash-high | context_masking | 3/3 | +3.9% [-6.9, 10.9] | 79 → 73 | 0 → 0 |
| gpt-6.1-sol | context_masking | 2/3 | -4.7% [-10.2, 1.4] | 58 → 53 | 1 → 0 |

</details>

## `0.4.0-dev+737ce81265-masking` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 341104 | 25 | 72% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 337850 | 25 | 73% | 0 |
| deepseek-flash | 3/3 | n/a | 331166 | 25 | 34% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 332254 | 24.67 | 35% | 0 |
| gemini-3.8-flash-high | 3/3 | n/a | 367342 | 24.33 | 5% | 0 |
| gpt-6.1-sol | 3/3 | n/a | 325336 | 26.67 | 27% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 9 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 9 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 9 | 3 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 8 | 3 | 0 | 3/3 | 0 |
| gemini-3.8-flash-high | 3 | 7 | 0 | 0 | 3/3 | 0 |
| gpt-6.1-sol | 3 | 14 | 3 | 0 | 3/3 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | context_masking | 2 | 0.4.0-dev+737ce81265-foreground | runaway |
