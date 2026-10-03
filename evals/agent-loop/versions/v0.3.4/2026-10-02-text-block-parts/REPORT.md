# Keep several text blocks in a user message or tool result apart as pi-ai parts

Candidate `v0.3.3+text-block-parts` at `040235851b`, compared with `v0.3.3` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6-astra, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-02T14:40:11.039Z.

The candidate is the uncommitted working tree on 040235851b (v0.3.3): llm-pi-ai sends a user message or tool result that holds several text blocks as separate text parts instead of concatenating them with no separator. One block still goes as plain text, so the standard suite rarely exercises the change: every first request is byte-identical between the arms for all seven models, and the dumped ordinary_edit requests match wherever the model made the same choices. Two token regressions are flagged and accepted as run variance, not an input change. gemini-3.8-flash-medium (+8.6%) took more steps (98 to 105 requests; multi_file_edit 15 to 18, unprompted_edit 13 to 15, workflow_script 18 to 20) with identical first-request token counts in both arms. gpt-6-astra (+12.3%) also took more steps (121 to 132), and the gateway billed byte-identical OpenAI requests at two levels about 410 tokens apart (3572-3606 or 3980-4015 input tokens for the same first request, in both arms), which also accounts for gpt-6.1-sol multi_site_edit (+10.6% with fewer request bytes). Claude and DeepSeek totals moved within noise, and deepseek-v4-pro tool errors fell from 2 to 0. Failures: gemini 3 to 4 (validation, both arms), gpt-6-astra 0 to 1 (wall-clock limit), gpt-6.1-sol 2 to 1.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-medium vs v0.3.3: total tokens 8.6% [1.1, 17.6]
- gpt-6-astra vs v0.3.3: total tokens 12.3% [3.4, 23.4]

## Against `v0.3.3`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | -1.5% [-7, 4.5] | -4.1% [-16.9, 10.4] | 98 → 97 | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 24/24 | +2.3% [-0.2, 6] | +1.6% [-3.8, 9.9] | 93 → 95 | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | -1.2% [-9.6, 8.8] | +5.4% [-5.5, 17.9] | 128 → 125 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -2.1% [-8.6, 5.2] | -2.4% [-10.5, 7.2] | 125 → 122 | 2 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 19/24 | +8.6% [1.1, 17.6] | +8.6% [1.1, 17.3] | 98 → 105 | 0 → 0 | 3 → 4 |
| gpt-6-astra | 23/24 | +12.3% [3.4, 23.4] | +12.4% [-20, 60.5] | 121 → 132 | 0 → 0 | 0 → 1 |
| gpt-6.1-sol | 21/24 | +3.4% [-3, 10.5] | +13.8% [-14.7, 53.7] | 107 → 110 | 0 → 0 | 2 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.2% [0, 0.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.1% [-0.4, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -10.5% [-26.1, 0.4] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -1.2% [-3.7, 0.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -7.7% [-19.9, 0] | 13 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-24.8, 32.9] | 11 → 11 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.3% [-1.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 3/3 | +6.9% [-18.3, 22.5] | 25 → 26 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.2% [-1.7, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | 0% [0, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | 0% [-0.6, 0.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | +0.1% [0.1, 0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | +8.4% [0, 25.2] | 12 → 13 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | +11.1% [-0.1, 33.1] | 9 → 10 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | 0% [0, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | workflow_script | 3/3 | -0.2% [-0.7, 0.1] | 24 → 24 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -17.4% [-26.6, 0.6] | 12 → 10 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -14.1% [-34.4, 0.1] | 15 → 13 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -9% [-23.4, 0.5] | 13 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +4.2% [-19.1, 35.6] | 17 → 17 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +1.9% [-12, 29.7] | 18 → 17 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +15.4% [-25.5, 86.8] | 14 → 16 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +10% [-1.3, 26.2] | 12 → 13 | 0 → 0 |
| deepseek-flash | workflow_script | 3/3 | -2.6% [-3.2, -1.6] | 27 → 27 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -9.1% [-20.9, -1.2] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -1.1% [-2.7, 1.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.3% [-0.3, 0.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -17.9% [-30.7, 2.7] | 18 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +7.1% [3.2, 13.4] | 16 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | 0% [-0.4, 0.4] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.2% [-0.1, 0.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | +6% [-24.7, 36.5] | 29 → 29 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | +19% [0.1, 38] | 15 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | 0% [-0.1, 0] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 1/3 | 0% [0, 0] | 4 → 4 | 1 → 1 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | -0.1% [-0.3, 0] | 18 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 1/3 | 0% [0, 0] | 6 → 6 | 1 → 2 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | +0.1% [-0.2, 0.5] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | +20% [0, 65.9] | 13 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | workflow_script | 2/3 | +22.9% [-1, 47] | 18 → 20 | 1 → 1 |
| gpt-6-astra | multi_file_edit | 3/3 | +8.4% [-4, 32.4] | 14 → 15 | 0 → 0 |
| gpt-6-astra | multi_site_edit | 3/3 | +33.7% [-5.5, 106.3] | 12 → 16 | 0 → 0 |
| gpt-6-astra | ordinary_edit | 3/3 | +14.3% [-0.2, 40.5] | 14 → 15 | 0 → 0 |
| gpt-6-astra | path_discovery | 3/3 | +10.8% [0, 36] | 17 → 18 | 0 → 0 |
| gpt-6-astra | shell_then_edit | 3/3 | +18.7% [0.1, 60.4] | 16 → 18 | 0 → 0 |
| gpt-6-astra | stale_edit | 2/3 | -0.2% [-8.5, 8.8] | 8 → 8 | 0 → 1 |
| gpt-6-astra | unprompted_edit | 3/3 | -6.1% [-10.8, 0.1] | 12 → 12 | 0 → 0 |
| gpt-6-astra | workflow_script | 3/3 | +13.4% [-1, 44] | 28 → 30 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | +12.3% [-1, 29.9] | 9 → 10 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +10.6% [10.2, 10.9] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -0.1% [-29, 40.2] | 14 → 14 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +14.7% [-10.4, 32.9] | 16 → 18 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -6.1% [-9.2, 0] | 18 → 18 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -0.1% [-0.2, 0.1] | 8 → 8 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -1.3% [-6.9, 3.6] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | +0.2% [-0.1, 0.4] | 18 → 18 | 1 → 0 |

</details>

## `v0.3.3+text-block-parts` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 20891 | 28285 | 4.04 | 97% | 0 |
| claude-sonnet-5-5 | 27/27 | 20895 | 27374 | 3.96 | 98% | 0 |
| deepseek-flash | 27/27 | 20749 | 28311 | 5.21 | 96% | 0 |
| deepseek-v4-pro | 27/27 | 20751 | 27574 | 5.08 | 97% | 0 |
| gemini-3.8-flash-medium | 23/27 | 20561 | 27882 | 5.5 | 0% | 0 |
| gpt-6-astra | 26/27 | 20537 | 22619 | 5.74 | 72% | 0 |
| gpt-6.1-sol | 26/27 | 20537 | 20809 | 5.39 | 60% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | ordinary_edit | 1 | v0.3.3+text-block-parts | empty final reply |
| gemini-3.8-flash-medium | ordinary_edit | 2 | v0.3.3 | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 1 | v0.3.3 | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 1 | v0.3.3+text-block-parts | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 2 | v0.3.3+text-block-parts | empty final reply |
| gemini-3.8-flash-medium | workflow_script | 0 | v0.3.3 | empty final reply |
| gemini-3.8-flash-medium | workflow_script | 0 | v0.3.3+text-block-parts | empty final reply |
| gpt-6-astra | stale_edit | 1 | v0.3.3+text-block-parts | wall_clock_limit |
| gpt-6.1-sol | multi_file_edit | 2 | v0.3.3+text-block-parts | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | v0.3.3 | exit 1 |
| gpt-6.1-sol | workflow_script | 1 | v0.3.3 | wall_clock_limit |
