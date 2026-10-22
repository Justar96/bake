# Background compaction with terminal accounting

Candidate `0.4.0-dev+0454597aee-background` at `0454597aee`, compared with `0.4.0-dev+0454597aee-default` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: background_compaction. Trials: 3. Started: 2026-10-17T22:26:40.094Z.

This paired run retains all 36 samples from six available models and three trials of background_compaction. Both arms use clean 0454597aee9462f40e33528e7ea53645da59b95e, binary SHA-256 35cb7de15da5434f4fb4cf3683ef30ea24506f74e5aafbd60fb309cb8e2a0d0a. The candidate enables backgroundCompaction and the base disables it. Both use the same 40000-token window, 16384-token reserve, 2560-token recent tail, 46-request cap, and 540000 ms cap. Evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605 requires final Anthropic message_delta counters and message_stop. These are measurements of the recorded binary and settings, not the later integrated source. The earlier initial-background record is a separate paired run; comparing their absolute counts would not isolate the accounting fix.

All 18 baseline samples pass the artifact predicate; the candidate passes 14/18. Flash candidate trial 2 fixes the code and passes its check but omits required codes.txt, printing the codes in its answer instead. Gemini candidate trial 0 reaches the wall-clock cap during recursive search outside the fixture. GPT candidate trials 1 and 2 stop on the existing streamed write/edit whitespace guard. The GPT failure increase from zero to two remains a regression flag. These failures are retained without claiming that background compaction caused their frequency. Two artifact-successful controls are excluded as evaluator-invalid in separate derived inputs, as described below; every failed sample is retained.

The candidate records 57 background starts, 44 commits, 12 discards, and 56 dispatched attempt observations. Six discards are attributed to foreground compaction and six to print completion. Every sample that reaches normal or model-error completion has terminal observations for its starts. Gemini's externally interrupted trial has two starts but only one terminal observation: signal handling cancels stdout delivery, so it cannot provide a complete terminal log. Eight observed attempts lack usage; four cancelled Anthropic attempts retain only partial initial counters. Unknown usage is not replaced with zero, and missing observations are not synthesized from proxy traffic. The measured policy can start speculative work that is later cancelled at print completion; its overhead remains in the record.

Only two valid pairs have successful artifacts and complete usage: Sonnet trials 0 and 2. Sonnet trial 0 extracts checkpoint lines with shell commands and has no background start in either arm. Sonnet trial 2 traverses notes in both arms and has four committed candidate summaries: 307467 to 330398 tokens (+7.46%), 23 to 26 requests, and 47708 to 58173 ms. Together these two pairs produce the reported Sonnet token flag of +6.9% [0.5, 7.5], but only one exercises the feature. That is insufficient evidence of repeatable performance.

Gemini baseline trial 1 inspects evaluator source and earlier raw logs outside the fixture; trial 2 inspects evaluator source. Both use the discovered checkpoint generator to construct answers. Their artifact predicates pass, but those classifications do not establish instruction compliance or independent fact retention. Separate hashed derived inputs mark only those two controls invalid while preserving their original artifact success, usage completeness, counters, and every failure. This mechanically excludes both paired cells from policy and cost comparisons. Before that validity exclusion, Gemini trial 2 would have shown 22 to 31 requests (+40.9%) and 276517 to 267380 tokens; its control is contaminated, so these are diagnostic counts rather than an eligible regression or efficiency estimate. The invalidated control makes three read calls and 19 bash calls; its candidate makes 24 reads and three bash calls. The fixture runs with danger-full-access and its instruction to remain inside the workspace does not enforce isolation. No fixture was retuned or sample rerun for this record.

All 36 accounting objects were reconciled with the corrected evaluator and matched exactly. The derived inputs change only the invalid field for the two demonstrated oracle-exposed controls; accounting, usage eligibility, outcomes, and all other fields remain unchanged. This is evaluator contamination, not a production defect or evidence of a model benefit. Paired token comparisons require both tasks to succeed and both usage records to be complete. Candidate-only means include successful samples with incomplete usage and are diagnostic, not complete costs. Pricing was not established; this record supplies no dollar-cost conclusion or basis for enabling background compaction by default. The user-approved Gemini high-effort substitution leaves the unavailable required Gemini medium model unmeasured. DeepSeek runs at high effort and the other models at medium. provenance.json preserves measured settings, revisions and binaries, all 156 original/derived sample, event, and design hashes, exact non-sensitive evidence for the two invalid controls, and accounting verification. observations.json retains lifecycle and activation counters without transcripts. Raw evidence and earlier committed records remain unchanged.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-sonnet-5-5 vs 0.4.0-dev+0454597aee-default: total tokens 6.9% [0.5, 7.5]
- gpt-6.1-sol vs 0.4.0-dev+0454597aee-default: failures 0 -> 2

## Against `0.4.0-dev+0454597aee-default`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 2/3 | +6.9% [0.5, 7.5] | +63.2% [2.5, 69.2] | 28 → 31 (+10.7% [0, 13]) | 30 → 30 | 84% → 75% | 0 → 0 | 0 → 0 |
| deepseek-flash | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 1 |
| deepseek-v4-pro | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 0/1 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 1 |
| gpt-6.1-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | background_compaction | 2/3 | +6.9% [0.5, 7.5] | 28 → 31 | 0 → 0 |
| deepseek-flash | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 1 |
| deepseek-v4-pro | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | background_compaction | 0/1 | n/a | 0 → 0 | 0 → 1 |
| gpt-6.1-sol | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 2 |

</details>

## `0.4.0-dev+0454597aee-background` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 318322 | 25.33 | 71% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 236328 | 19 | 75% | 0 |
| deepseek-flash | 2/3 | n/a | 292027 | 27.5 | 81% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 313973 | 27 | 82% | 0 |
| gemini-3.8-flash-high | 2/3 | n/a | 263355 | 30.5 | 1% | 1 |
| gpt-6.1-sol | 1/3 | n/a | 299752 | 28 | 78% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 11 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 8 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 21 | 2 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 15 | 3 | 0 | 3/3 | 0 |
| gemini-3.8-flash-high | 3 | 17 | 0 | 0 | 2/2 | 0 |
| gpt-6.1-sol | 3 | 13 | 1 | 0 | 1/2 | 2 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-flash | background_compaction | 2 | 0.4.0-dev+0454597aee-background | validation failed |
| gemini-3.8-flash-high | background_compaction | 0 | 0.4.0-dev+0454597aee-background | wall_clock_limit |
| gpt-6.1-sol | background_compaction | 1 | 0.4.0-dev+0454597aee-background | runaway |
| gpt-6.1-sol | background_compaction | 2 | 0.4.0-dev+0454597aee-background | runaway |
