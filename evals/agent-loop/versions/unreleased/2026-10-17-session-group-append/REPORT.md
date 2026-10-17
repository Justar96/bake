# Session group append: native paired eval

Candidate `41e5e867a3-native` at `41e5e867a3`, compared with `3807a95127-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-16T22:06:18.532Z.

Native bake-rs 41e5e867a3 against clean native base 3807a95127: all eight standard scenarios, three trials, six routes, 288 samples. With owner approval for this record, gemini-3.8-flash-high at medium requested effort replaces the unavailable gemini-3.8-flash-medium route. Both arms use the same substitution; these numbers do not represent the unavailable route.

The sole flagged regression is Gemini tool errors, 0 to 2, in candidate path_discovery trials 0 and 1. Trial 0 ran node test.cjs before fixing the seeded defect, then fixed it and passed; trial 1 fixed and passed before an unguarded git status failed because the fixture is not a Git repository. Base trials used guarded git status commands. Every Gemini task succeeded. The flag is accepted for this storage change: these are observed model command choices, and the affected persistence and recovery paths are not reached here. Sampling variation is an inference, not a proof of no effect. The flag and all samples remain in the record.

GPT failed four base and three candidate samples. Six failures were unfinished edit-argument whitespace streams stopped by the unchanged bake-ai guard (base four, candidate two). The seventh was a candidate provider overload reported as an SSE error; the recorder labels it empty final reply. One otherwise successful base GPT sample and one base DeepSeek v4-pro sample lost a stream to a closed/reset connection and have incomplete usage. Paired token/request metrics cover only 14 of 21 GPT task cells (no ordinary_edit pair) and 20 of 21 DeepSeek v4-pro cells; the failure counts cannot exclude a small effect. Sonnet total tokens are +3.5% [0, 8.3], not flagged: two shell_then_edit trials added one read. No sample was discarded, reclassified, or selectively rerun.

The eval uses --no-session and disables agent retry. It exercises append_group only for the initial model/thinking entries in memory, in their previous order. File writes, fsync, mid-run model changes and retry/overflow omission are not exercised; crash and fault tests cover them separately. Normalized system prompts and first requests match apart from each binary path (six bytes), temporary workspace paths, session dates and random cache ids. Thus this is routine-task regression evidence, not durability qualification. Raw transcripts stay in ignored .preflight/.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-high vs 3807a95127-native: tool errors 0 -> 2

## Against `3807a95127-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -1.9% [-4.9, -0.1] | +0.3% [-4.1, 5.2] | 67 → 66 (-1.5% [-4.5, 0]) | 85 → 84 | 84% → 84% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | +3.5% [0, 8.3] | +2.5% [-3, 8.4] | 66 → 68 (+3% [0, 7.7]) | 80 → 82 | 85% → 85% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +0.4% [-8.2, 10] | +2.8% [-2.1, 7.7] | 85 → 87 (+2.4% [-4.7, 9.8]) | 104 → 99 | 77% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 20/21 | -3% [-12.4, 6.9] | +1.6% [-2.6, 6.6] | 91 → 88 (-3.3% [-11.2, 4.3]) | 96 → 93 | 82% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -3.8% [-11.4, 4.4] | -3.8% [-11.2, 4.3] | 74 → 72 (-2.7% [-7.6, 2.8]) | 102 → 100 | 0% → 0% | 0 → 2 | 0 → 0 |
| gpt-6.1-sol | 14/21 | +1.2% [-4.9, 7.9] | +22.5% [-2.7, 49.5] | 70 → 71 (+1.4% [-4.4, 7.6]) | 59 → 60 | 69% → 62% | 0 → 0 | 4 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.4% [-0.2, 1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | 0% [-0.2, 0.4] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.1% [-0.4, 0.5] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.5% [-1.5, 0.3] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -11.5% [-25.2, -2] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.5% [-1, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.6% [-1.9, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.2% [-0.4, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.1% [-0.3, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.2, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +2.6% [2.1, 2.9] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +23% [0.2, 34.8] | 9 → 11 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -0.3% [-0.9, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -0.1% [-6, 6.4] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -1.7% [-3, -1] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +5.6% [-4.1, 30.2] | 11 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -5.8% [-22.9, 4.9] | 12 → 11 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -7.2% [-31.8, 11.5] | 13 → 12 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +3.5% [-10.7, 41.1] | 14 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +10.2% [-30.4, 39.6] | 12 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -0.4% [-27.7, 46] | 11 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +0.9% [-20.8, 29.7] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -6.1% [-23.4, 6.9] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.7% [-0.1, 1.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 2/3 | -9.9% [-18.8, 1] | 11 → 10 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -2.9% [-38.3, 51.6] | 18 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.6% [-1.5, -0.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -2.5% [-4.9, 1.4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +13.8% [-2.2, 45.7] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | -0.3% [-1.1, 1] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | -1% [-4.7, 1.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -3.1% [-15, 17.2] | 14 → 13 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -19.8% [-38.9, 0.4] | 15 → 13 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -3.6% [-10.2, 1.8] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +1.2% [-0.7, 4.8] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -6% [-11.7, 0.4] | 19 → 18 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | -0.3% [-0.4, -0.1] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 0/3 | n/a | 0 → 0 | 2 → 2 |
| gpt-6.1-sol | path_discovery | 3/3 | +11% [-0.2, 23.6] | 15 → 17 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -0.1% [-0.7, 0.5] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 1/3 | 0% [0, 0] | 4 → 4 | 2 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +2.8% [-21.8, 34.2] | 9 → 9 | 0 → 0 |

</details>

## `41e5e867a3-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 6796 | 10490 | 3.14 | 84% | 0 |
| claude-sonnet-5-5 | 24/24 | 6798 | 10422 | 3.24 | 85% | 0 |
| deepseek-flash | 24/24 | 6684 | 10026 | 4.14 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 6685 | 10900 | 4.43 | 81% | 0 |
| gemini-3.8-flash-high | 24/24 | 6719 | 7837 | 3.43 | 0% | 2 |
| gpt-6.1-sol | 21/24 | 6709 | 9194 | 4.94 | 61% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 0 | 0 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 2 | 0 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 21 | 13 | 2 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 27 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 6 | 0 | 1 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 32 | 18 | 1 | 18/18 | 2 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 1 | 41e5e867a3-native | empty final reply |
| gpt-6.1-sol | ordinary_edit | 0 | 3807a95127-native | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | 41e5e867a3-native | runaway |
| gpt-6.1-sol | ordinary_edit | 1 | 41e5e867a3-native | runaway |
| gpt-6.1-sol | ordinary_edit | 2 | 3807a95127-native | runaway |
| gpt-6.1-sol | stale_edit | 1 | 3807a95127-native | runaway |
| gpt-6.1-sol | stale_edit | 2 | 3807a95127-native | runaway |
