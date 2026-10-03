# Leaner tool descriptions and round-trip guidance

Candidate `trim-and-batch` at `496d1b68ac`, compared with `v0.3.3+496d1b68ac-dirty` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-02T23:49:07.885Z.

Tool and parameter descriptions shortened (tool JSON about 19.7k to 13.1k chars, schema shape unchanged); persona asks for independent calls in one response, an edit sent with its check, one edit call per file, and no confirming re-read; todo_write says to skip short tasks. Base is the pre-change working tree of chore/post-0.3.3-debt (uncommitted, hence dirty). Models: GPT 6.1 Sol medium, Gemini 3.8 Flash High at high, DeepSeek V4 Pro high, Claude Opus 5.5 medium; Opus runs through the second CLIProxyAPI gateway (EVAL_GATEWAY) for both arms. Sonnet 5.5 and GPT-6 Astra were not run. Supersedes an earlier unmerged run of this change whose workflow description had lost its tool_help pointer (GPT then called skill("workflow")) and whose three GPT failures were gateway streams that returned 200 and then stalled to the wall-clock cap.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.3.3+496d1b68ac-dirty`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -30.5% [-34.4, -26.4] | +97.8% [40.5, 183.6] | 101 → 90 | 83 → 90 | 98% → 94% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -24% [-26.1, -22.2] | +18.9% [7.2, 31.8] | 119 → 117 | 110 → 108 | 97% → 96% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 24/24 | -53.3% [-59.8, -45.9] | -55.8% [-62.1, -48.9] | 164 → 101 | 131 → 109 | 1% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 23/24 | -30.1% [-36.3, -23.4] | -8.2% [-33.5, 27.7] | 115 → 111 | 90 → 87 | 73% → 64% | 1 → 0 | 1 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -19.4% [-19.6, -19.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -42.6% [-42.7, -42.5] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -21.2% [-21.6, -20.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -19.8% [-22.4, -18.3] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -39.9% [-41.2, -38.1] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -41.3% [-41.4, -41.3] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -21.8% [-22.2, -21.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 3/3 | -31% [-35.6, -19.9] | 26 → 24 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -23.2% [-26.4, -20.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -29.1% [-39.7, -20.8] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -23.4% [-23.7, -23] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -19.8% [-23.9, -14.7] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -27.1% [-31.6, -23.1] | 16 → 15 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -23.5% [-24.3, -22.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -23.3% [-24.5, -22.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | -22.9% [-24.3, -22] | 27 → 27 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -37.1% [-42.3, -23.1] | 19 → 15 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -51.4% [-55.2, -43] | 14 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -56.5% [-67.4, -40.8] | 16 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -62.3% [-68.1, -50.4] | 23 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -68.3% [-78.7, -47] | 31 → 11 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -50.5% [-62.9, -40.8] | 14 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | -54.1% [-54.3, -53.8] | 15 → 9 | 0 → 0 |
| gemini-3.8-flash-high | workflow_script | 3/3 | -35.8% [-51.2, -19] | 32 → 27 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -28% [-42.3, -2.6] | 14 → 14 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | -31.5% [-33.7, -28.3] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -15.9% [-40.9, 1.8] | 13 → 14 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | -26.8% [-36.1, -20.5] | 16 → 15 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -24.6% [-40, -2.5] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -28.8% [-36.4, -21.5] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -33.4% [-38.4, -26.6] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | -50.9% [-61.3, -39.3] | 21 → 17 | 1 → 0 |

</details>

## `trim-and-batch` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 14624 | 21558 | 3.75 | 94% | 0 |
| deepseek-v4-pro | 27/27 | 14484 | 20195 | 4.88 | 96% | 0 |
| gemini-3.8-flash-high | 27/27 | 14288 | 16457 | 4.21 | 0% | 0 |
| gpt-6.1-sol | 27/27 | 14270 | 14048 | 5 | 65% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | workflow_script | 0 | v0.3.3+496d1b68ac-dirty | wall_clock_limit |
