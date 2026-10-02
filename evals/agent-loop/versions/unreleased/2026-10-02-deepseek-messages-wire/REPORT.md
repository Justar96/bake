# DeepSeek Messages wire aligned with the removed llm-deepseek adapter

Candidate `deepseek-messages-wire` at `7f2b96ee5d`, compared with `v0.3.2+7428f19adb` in the same run.
Models: deepseek-flash, deepseek-v4-pro. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-02T12:34:25.706Z.

Candidate is PR #16 (7f2b96ee5d) plus the llm-pi-ai messagesWire option on deepseek-official: no __pi_deferred_placeholder__ tool, no cache_control, adjacent same-role messages merged. Supersedes the DeepSeek rows of 2026-10-02-pi-migration-compaction for the wire question.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.3.2+7428f19adb`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| deepseek-flash | 24/24 | -2.3% [-10.9, 6] | -2.6% [-11.1, 6.5] | 125 → 123 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +1.9% [-4.3, 7.8] | -8.3% [-19, 2.3] | 122 → 125 | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| deepseek-flash | multi_file_edit | 3/3 | +1.1% [-0.9, 3.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +8.2% [0.1, 22] | 14 → 15 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -0.3% [-0.9, 0.1] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -3.1% [-21.5, 21.2] | 16 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -17.7% [-23.8, -13.3] | 17 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -9.7% [-47.9, 53.9] | 16 → 15 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +0.8% [-1, 2.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | workflow_script | 3/3 | +6.9% [-2.2, 26.9] | 26 → 27 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +16.5% [-1.5, 27.2] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -7.1% [-20.4, 0.7] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.2% [0, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +3.6% [-7.7, 19.5] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -9.5% [-29.7, 4.7] | 17 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +8.8% [-0.2, 26.3] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -1% [-1.9, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | +7.2% [-2.3, 26.7] | 28 → 29 | 0 → 0 |

</details>

## `deepseek-messages-wire` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| deepseek-flash | 27/27 | 20749 | 27217 | 5.13 | 96% | 0 |
| deepseek-v4-pro | 27/27 | 20751 | 28311 | 5.21 | 97% | 0 |
