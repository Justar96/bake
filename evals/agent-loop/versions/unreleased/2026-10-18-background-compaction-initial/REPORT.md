# Initial background compaction with corrected usage eligibility

Candidate `0.4.0-dev+af35bc6faa-background` at `af35bc6faa`, compared with `0.4.0-dev+af35bc6faa-default` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: background_compaction. Trials: 3. Started: 2026-10-17T21:59:08.771Z.

This is a corrected accounting record of the original 36 background-compaction samples, not a new live evaluation. Both arms measured clean af35bc6faaafce414bb3c3e5d46aa7d963c1c21e with binary SHA-256 932c77b11dc95a14e6e712daab4751324851f10f531b5b44b90c2bd8c8fd4e04. The candidate enabled backgroundCompaction; the base left it disabled. Six available models ran three trials of the opt-in background_compaction scenario, with a 40000-token context window, 16384 reserved tokens, 2560 recent tokens, a 540000 ms cap, and a 46-request cap. These samples predate the print-finalization fix and do not measure the final integrated binary.

The base succeeds on 16/18 samples and candidate on 14/18. The candidate records 69 background starts, 49 commits, nine discards, and 58 attempt observations. Eleven starts have no terminal event in this historical binary, and four observed attempts lack usage. Some canceled Anthropic requests retain only initial partial counters. Missing terminal observations and unknown provider usage are not synthesized. Original raw samples, event logs, and previous committed records remain unchanged; the derived inputs only add provenance metadata and downgrade accounting eligibility where required.

The corrected Anthropic rule requires final message_delta output counters and message_stop for each request, using evaluator commit 033f047f3348b8a58bda953da55766ab7e61e605. It excludes Opus trial 0, which the old rule admitted despite partial usage from a canceled summary. Only three candidate samples have complete usage: Opus trial 1 and Gemini trials 1 and 2. Gemini trial 2's base failed, leaving two usable paired cost observations out of 18. Opus trial 1 uses 310070 → 333326 tokens (+7.50%) and 23 → 29 requests; Gemini trial 1 uses 289106 → 292182 (+1.06%) and 25 → 29 requests. The generated regression flags retain these increases. Each model has only one eligible pair, so its degenerate bootstrap interval is not evidence of repeatability. Neither this record nor the reported partial counters supports a general efficiency or dollar-cost claim. The candidate-only mean-token column also includes successful samples with incomplete usage and must be treated as diagnostic rather than complete cost.

The failed samples remain unchanged: DeepSeek Pro candidate trial 1 encounters a connection reset; Gemini candidate trial 0 reaches the request cap during unrelated exploration and base trial 2 reaches the wall-clock cap; GPT candidate trial 1 and base trial 2 stream repetitive write arguments until the wall-clock cap, and candidate trial 2 reaches the whitespace guard. Opus candidate trial 1's counted tool error is a read starting beyond the end of a file, followed by success. Sonnet candidate trial 2 calls a nonexistent destroy_bash tool and ultimately passes, but its incomplete usage excludes it from cost comparisons. These facts explain the observed failure and tool-error mechanisms without establishing that background compaction caused their frequency. All failures and repair overhead remain in the record; background compaction stays off by default.

provenance.json retains hashes for all original and derived samples, event logs, and designs, plus the derivation script and corrected-accounting verification. All 156 source/derived file hashes were checked; copied event logs and designs are byte-identical. All 36 derived accounting objects were directly checked against the corrected evaluator. No model was rerun. The later print fix preserves terminal publication through owned background cleanup but cannot recover counters a provider never reports; its evidence belongs to a separate run. The user-approved Gemini high-effort substitution leaves the unavailable required Gemini medium model unmeasured. Raw transcripts remain private.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs 0.4.0-dev+af35bc6faa-default: total tokens 7.5% [7.5, 7.5]
- claude-opus-5-5 vs 0.4.0-dev+af35bc6faa-default: requests 26.1% [26.1, 26.1]
- claude-opus-5-5 vs 0.4.0-dev+af35bc6faa-default: tool errors 0 -> 1
- gemini-3.8-flash-high vs 0.4.0-dev+af35bc6faa-default: total tokens 1.1% [1.1, 1.1]
- gemini-3.8-flash-high vs 0.4.0-dev+af35bc6faa-default: requests 16% [16, 16]

## Against `0.4.0-dev+af35bc6faa-default`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 1/3 | +7.5% [7.5, 7.5] | +62.9% [62.9, 62.9] | 23 → 29 (+26.1% [26.1, 26.1]) | 25 → 47 | 84% → 75% | 0 → 1 | 0 → 0 |
| claude-sonnet-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| deepseek-flash | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 1 |
| gemini-3.8-flash-high | 1/3 | +1.1% [1.1, 1.1] | +22% [22, 22] | 25 → 29 (+16% [16, 16]) | 27 → 26 | 22% → 3% | 0 → 0 | 1 → 1 |
| gpt-6.1-sol | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 1 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | background_compaction | 1/3 | +7.5% [7.5, 7.5] | 23 → 29 | 0 → 0 |
| claude-sonnet-5-5 | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 0 |
| deepseek-flash | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | background_compaction | 0/3 | n/a | 0 → 0 | 0 → 1 |
| gemini-3.8-flash-high | background_compaction | 1/3 | +1.1% [1.1, 1.1] | 25 → 29 | 1 → 1 |
| gpt-6.1-sol | background_compaction | 0/3 | n/a | 0 → 0 | 1 → 2 |

</details>

## `0.4.0-dev+af35bc6faa-background` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 328501 | 27.33 | 75% | 1 |
| claude-sonnet-5-5 | 3/3 | n/a | 330809 | 27.33 | 74% | 1 |
| deepseek-flash | 3/3 | n/a | 292544 | 28 | 78% | 0 |
| deepseek-v4-pro | 2/3 | n/a | 304128 | 28.5 | 82% | 0 |
| gemini-3.8-flash-high | 2/3 | n/a | 303482 | 30 | 1% | 2 |
| gpt-6.1-sol | 1/3 | n/a | 304804 | 28 | 80% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 16 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 16 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 18 | 2 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 22 | 2 | 0 | 2/2 | 0 |
| gemini-3.8-flash-high | 3 | 41 | 0 | 0 | 2/2 | 0 |
| gpt-6.1-sol | 3 | 10 | 1 | 0 | 1/1 | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-v4-pro | background_compaction | 1 | 0.4.0-dev+af35bc6faa-background | exit 1 |
| gemini-3.8-flash-high | background_compaction | 0 | 0.4.0-dev+af35bc6faa-background | request_limit |
| gemini-3.8-flash-high | background_compaction | 2 | 0.4.0-dev+af35bc6faa-default | wall_clock_limit |
| gpt-6.1-sol | background_compaction | 1 | 0.4.0-dev+af35bc6faa-background | wall_clock_limit |
| gpt-6.1-sol | background_compaction | 2 | 0.4.0-dev+af35bc6faa-default | wall_clock_limit |
| gpt-6.1-sol | background_compaction | 2 | 0.4.0-dev+af35bc6faa-background | runaway |
