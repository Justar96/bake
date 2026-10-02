# pi 1.0 migration (DeepSeek via pi-ai, MCP via pi-mcp, run_code via pi-codemode), loop halts and cut-off notice, compaction checkpoint state

Candidate `pi-migration-compaction` at `7428f19adb`, compared with `v0.3.2+7428f19adb` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6-astra, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-02T11:18:06.166Z.

Candidate is the uncommitted working tree on 7428f19adb. The first GPT runs were blocked at the gateway on both arms and were rerun. Flagged regression gpt-6.1-sol failures 1 -> 3 is accepted: all three candidate failures (ordinary_edit.0, multi_site_edit.0, workflow_script.0) hit wall_clock_limit while waiting on a model response with no tool running, in the same trial at the same time; the base arm and gpt-6-astra show the same stall (workflow_script.2.base, ordinary_edit.2.base). DeepSeek requests now carry the pi-ai __pi_deferred_placeholder__ tool with defer_loading; no sample called it.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs v0.3.2+7428f19adb: failures 1 -> 3

## Against `v0.3.2+7428f19adb`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/24 | +0.1% [-0.2, 0.6] | +2.7% [-4.5, 13.1] | 83 → 83 | 0 → 0 | 2 → 3 |
| claude-sonnet-5-5 | 24/24 | -1.4% [-4.1, 0.1] | -0.7% [-10.5, 10.8] | 94 → 93 | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | +0.7% [-5.7, 7.6] | +9.2% [1.4, 17.1] | 124 → 124 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | +1.8% [-4.2, 7.7] | +13.6% [2.6, 27.4] | 121 → 123 | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 17/24 | -3.8% [-11.3, 3.7] | -3.8% [-11.1, 3.8] | 95 → 92 | 0 → 0 | 5 → 5 |
| gpt-6-astra | 21/24 | +1.6% [-8.5, 12.3] | -0.3% [-22.6, 24.5] | 107 → 108 | 0 → 0 | 1 → 2 |
| gpt-6.1-sol | 20/24 | +3.2% [-1.6, 9.6] | +13.3% [-14.2, 46.6] | 101 → 104 | 0 → 0 | 1 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 2/3 | -0.3% [-0.5, -0.1] | 6 → 6 | 0 → 1 |
| claude-opus-5-5 | multi_site_edit | 3/3 | 0% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.1% [0, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +1.4% [0, 4.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 2/3 | 0% [0, 0] | 8 → 8 | 1 → 1 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [0, 0.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.6% [-1.4, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 2/3 | 0% [0, 0] | 18 → 18 | 1 → 1 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.1% [-1.7, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | 0% [0, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -1.4% [-3.5, 3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.2% [0, 0.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | 0% [0, 0] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -10% [-24.8, 0] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | 0% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | workflow_script | 3/3 | 0% [-0.1, 0.1] | 24 → 24 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | +1.1% [0.6, 1.9] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +8.6% [-1.6, 26.5] | 12 → 13 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +1.5% [0.3, 2.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +11.1% [2.4, 23.4] | 16 → 17 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +8.6% [-17.2, 44.4] | 17 → 18 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -15.4% [-34.5, -2.8] | 15 → 13 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -8.4% [-22.5, 1.5] | 13 → 12 | 0 → 0 |
| deepseek-flash | workflow_script | 3/3 | -2.8% [-5.1, 0.3] | 27 → 27 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +8.6% [-2.1, 27.5] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -0.9% [-23.9, 31.8] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.1% [-0.4, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +14.3% [-1, 22.8] | 15 → 17 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | 0% [-6.5, 4.1] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -7.6% [-20, 0.6] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.1% [-2.3, 2.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | -1.2% [-19.7, 18.2] | 28 → 28 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -0.6% [-1.1, -0.1] | 15 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 2/3 | +3% [2.9, 3.1] | 8 → 8 | 0 → 1 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +8.1% [-0.1, 24] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +6% [-4.1, 19.9] | 17 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 1/3 | -14.9% [-14.9, -14.9] | 7 → 6 | 2 → 1 |
| gemini-3.8-flash-medium | stale_edit | 1/3 | +0.1% [0.1, 0.1] | 4 → 4 | 2 → 1 |
| gemini-3.8-flash-medium | unprompted_edit | 2/3 | -19.2% [-35, -0.2] | 11 → 9 | 0 → 1 |
| gemini-3.8-flash-medium | workflow_script | 2/3 | -17.5% [-32.1, 0] | 21 → 19 | 1 → 1 |
| gpt-6-astra | multi_file_edit | 3/3 | -4.2% [-22.9, 10.4] | 15 → 14 | 0 → 0 |
| gpt-6-astra | multi_site_edit | 3/3 | -6.1% [-9.3, 0] | 12 → 12 | 0 → 0 |
| gpt-6-astra | ordinary_edit | 2/3 | +40.5% [40.3, 40.6] | 8 → 10 | 1 → 0 |
| gpt-6-astra | path_discovery | 3/3 | +11.6% [0.4, 35.7] | 17 → 18 | 0 → 0 |
| gpt-6-astra | shell_then_edit | 2/3 | +24.4% [0.1, 63.9] | 10 → 12 | 0 → 1 |
| gpt-6-astra | stale_edit | 3/3 | +0.2% [-8.3, 9.3] | 12 → 12 | 0 → 0 |
| gpt-6-astra | unprompted_edit | 3/3 | -6% [-11.4, 0.4] | 12 → 12 | 0 → 0 |
| gpt-6-astra | workflow_script | 2/3 | -25% [-44.9, -1] | 21 → 18 | 0 → 1 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +11.8% [1.5, 33.7] | 14 → 15 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 2/3 | +4.9% [0, 10.4] | 8 → 8 | 0 → 1 |
| gpt-6.1-sol | ordinary_edit | 2/3 | +16.7% [-0.1, 40.4] | 9 → 10 | 0 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | -2.9% [-5, -0.4] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +6.8% [-8.8, 27.8] | 18 → 19 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | -5.9% [-8.8, -0.3] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -2.7% [-9.3, 4.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 1/3 | +0.2% [0.2, 0.2] | 10 → 10 | 1 → 1 |

</details>

## `pi-migration-compaction` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/27 | 20891 | 28492 | 3.95 | 97% | 0 |
| claude-sonnet-5-5 | 27/27 | 20895 | 26738 | 3.88 | 97% | 0 |
| deepseek-flash | 27/27 | 21058 | 28052 | 5.17 | 95% | 0 |
| deepseek-v4-pro | 27/27 | 20865 | 27749 | 5.13 | 97% | 0 |
| gemini-3.8-flash-medium | 22/27 | 20561 | 26858 | 5.37 | 0% | 0 |
| gpt-6-astra | 25/27 | 20537 | 20358 | 5.14 | 74% | 0 |
| gpt-6.1-sol | 24/27 | 20537 | 21425 | 5.38 | 70% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 2 | pi-migration-compaction | exit 1 |
| claude-opus-5-5 | shell_then_edit | 2 | v0.3.2+7428f19adb | exit 1 |
| claude-opus-5-5 | shell_then_edit | 2 | pi-migration-compaction | exit 1 |
| claude-opus-5-5 | workflow_script | 2 | v0.3.2+7428f19adb | exit 1 |
| claude-opus-5-5 | workflow_script | 2 | pi-migration-compaction | exit 1 |
| gemini-3.8-flash-medium | multi_site_edit | 0 | pi-migration-compaction | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 0 | v0.3.2+7428f19adb | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 0 | pi-migration-compaction | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 2 | v0.3.2+7428f19adb | empty final reply |
| gemini-3.8-flash-medium | stale_edit | 1 | v0.3.2+7428f19adb | empty final reply |
| gemini-3.8-flash-medium | stale_edit | 1 | pi-migration-compaction | wall_clock_limit |
| gemini-3.8-flash-medium | stale_edit | 2 | v0.3.2+7428f19adb | empty final reply |
| gemini-3.8-flash-medium | unprompted_edit | 1 | pi-migration-compaction | exit 1 |
| gemini-3.8-flash-medium | workflow_script | 0 | v0.3.2+7428f19adb | empty final reply |
| gemini-3.8-flash-medium | workflow_script | 0 | pi-migration-compaction | empty final reply |
| gpt-6-astra | ordinary_edit | 2 | v0.3.2+7428f19adb | wall_clock_limit |
| gpt-6-astra | shell_then_edit | 1 | pi-migration-compaction | wall_clock_limit |
| gpt-6-astra | workflow_script | 0 | pi-migration-compaction | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 0 | pi-migration-compaction | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | pi-migration-compaction | wall_clock_limit |
| gpt-6.1-sol | workflow_script | 0 | pi-migration-compaction | wall_clock_limit |
| gpt-6.1-sol | workflow_script | 2 | v0.3.2+7428f19adb | wall_clock_limit |
