# Leaner tool roster and core cleanup

Candidate `trim-and-removals` at `496d1b68ac`, compared with `v0.3.3+496d1b68ac-dirty` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-03T13:31:58.198Z.

Candidate is the whole branch change against its pre-change working tree (same base as 2026-10-03-trim-and-batch): shorter tool descriptions and round-trip persona guidance, plus removal of the todo_write, workflow (with tool_help prose and ralph), present and subagent_fork tools, unmounted packages, and a behaviour-preserving core cleanup (helper dedupe, ToolRuntime split with byte-identical schemas). The exact pre-removal tree could not be rebuilt, so the removals alone are not isolated; compare with 2026-10-03-trim-and-batch for an approximate split. workflow_script left the standard suite with the workflow tool. Models: GPT 6.1 Sol medium, Gemini 3.8 Flash High at high, DeepSeek V4 Pro, Opus 5.5 medium via EVAL_GATEWAY. The flagged GPT failures (1 -> 4) are wall-clock aborts in which the last request returned 200 and delivered no usage event before the 180 s cap, with no tool errors; the base arm hit it once in this run. Follow-up probes with stream timing and tool-argument capture (raw output in .preflight, not part of this record) found the cause in both arms: GPT 6.1 Sol completes a valid edit call, then keeps streaming whitespace inside the unclosed tool-call JSON until the cap (4 of 48 samples on the default gateway, 1 of 48 via EVAL_GATEWAY, with both arms on the pre-change tree). It is a model degeneration that predates this change, not a transport stall and not a confirmed effect of the change; a guard is a separate fix.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs v0.3.3+496d1b68ac-dirty: failures 1 -> 4

## Against `v0.3.3+496d1b68ac-dirty`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -41.7% [-46.1, -37.2] | +11% [0.9, 22.1] | 76 → 66 | 73 → 75 | 97% → 94% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -39.4% [-44.2, -34.5] | -7.8% [-25.3, 14.2] | 96 → 91 | 96 → 92 | 97% → 95% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -67.1% [-70.4, -63.2] | -69.2% [-72.2, -65.4] | 133 → 66 | 120 → 99 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 16/21 | -41.6% [-45.1, -37.7] | -30% [-39.7, -18.6] | 78 → 73 | 62 → 59 | 53% → 43% | 0 → 0 | 1 → 4 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -31.6% [-31.8, -31.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -51.4% [-51.5, -51.4] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -33.3% [-33.5, -32.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -30.1% [-30.3, -30.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -53.5% [-59.9, -49.3] | 13 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -50.4% [-50.4, -50.3] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -34% [-34.1, -33.8] | 9 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -39.7% [-47.1, -34.2] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -40.7% [-51.9, -31.7] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -35.2% [-35.3, -35.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -48.5% [-59.7, -33.9] | 18 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -35.4% [-50.2, -17.9] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -35.4% [-36, -35.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -34.9% [-35.6, -33.9] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -62.6% [-66.7, -59.4] | 16 → 9 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -58.7% [-62.2, -50.9] | 14 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -67.8% [-78.5, -51.7] | 18 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -71.1% [-77.8, -51.9] | 27 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -65.8% [-67.4, -64.3] | 21 → 9 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -73% [-73.3, -72.5] | 21 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -64.6% [-70.1, -61] | 16 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -38.5% [-43.5, -30.7] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -34.3% [-37.5, -30.8] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -43.1% [-43.1, -43.1] | 5 → 4 | 0 → 2 |
| gpt-6.1-sol | path_discovery | 2/3 | -40.3% [-49, -30.6] | 12 → 11 | 0 → 1 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -46.9% [-55.3, -42.2] | 18 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -41.8% [-47, -30.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -43.4% [-45.6, -41.1] | 8 → 8 | 1 → 0 |

</details>

## `trim-and-removals` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 12032 | 17492 | 3.14 | 94% | 0 |
| deepseek-v4-pro | 24/24 | 11892 | 16554 | 4.33 | 95% | 0 |
| gemini-3.8-flash-high | 24/24 | 11748 | 11487 | 3.14 | 0% | 0 |
| gpt-6.1-sol | 20/24 | 11730 | 11619 | 4.53 | 43% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 2 | trim-and-removals | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | trim-and-removals | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 2 | trim-and-removals | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 2 | trim-and-removals | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 2 | v0.3.3+496d1b68ac-dirty | wall_clock_limit |
