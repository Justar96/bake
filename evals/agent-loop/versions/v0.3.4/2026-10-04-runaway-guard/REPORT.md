# Abort tool calls that stream runaway whitespace

Candidate `runaway-guard` at `496d1b68ac`, compared with `pre-guard` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 5. Started: 2026-10-03T16:23:39.190Z.

The llm-pi-ai adapter now aborts a streamed tool call once its arguments end in more than 2,000 whitespace characters outside JSON strings, and raises a retryable TRANSPORT error. Base is a built snapshot of the branch just before this change (.agents/worktrees/eval-pre-guard). Every arm got one adapter retry (retryPolicy.maxRetries 1) instead of the eval default of none, as users have, so a guarded runaway can recover. GPT 6.1 Sol ran 5 trials and the other models 3. In the base, 10 of 40 GPT samples hit the 180 s cap while streaming whitespace inside an unclosed tool call (9 edit, 1 bash, 12k to 238k characters), and 1 failed on a gateway overload. With the guard, GPT failed once: the retry ran away again and the guard stopped it after 27 s. The guard never fired for Opus, Gemini, or DeepSeek, which had no failures in either arm. The strict arm, with strictTools on for the GPT route, is recorded under steps/strict-tools.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `pre-guard`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | 0% [-0.8, 0.8] | +2.1% [-17.7, 26.7] | 66 → 66 | 79 → 81 | 95% → 94% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +3.6% [-3.8, 13.2] | +0.4% [-13.2, 17.1] | 91 → 94 | 93 → 92 | 96% → 96% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | -4.8% [-13.1, 2.5] | -5% [-13.6, 2.7] | 75 → 73 | 102 → 99 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 20/35 | -2.9% [-8.7, 4.4] | -6.2% [-25.9, 17.8] | 91 → 91 | 72 → 74 | 51% → 53% | 0 → 0 | 11 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +1.6% [-0.1, 4.8] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +1.5% [-0.1, 4.7] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +0.2% [0, 0.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.7% [-1.2, 0.2] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -2.5% [-4.1, -0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -0.1% [-0.1, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | 0% [-0.3, 0.3] | 9 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -1.7% [-5.4, 0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -11.7% [-23.8, 0.3] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.1% [-0.8, 0.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -2.9% [-5.5, -0.5] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +40.9% [-0.1, 69.1] | 14 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | 0% [-0.7, 0.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +0.1% [-0.2, 0.4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | -14.7% [-43.9, 1.6] | 15 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +0.2% [-3.1, 3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +1.2% [-1.5, 3.6] | 11 → 11 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | -1.6% [-6.9, 1.7] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -9.7% [-35.1, 29.3] | 10 → 10 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | -1.7% [-2.2, -1.3] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +1% [-0.4, 1.7] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 5/5 | -9.1% [-15.4, -2.2] | 25 → 24 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 4/5 | -10.9% [-15.4, -3.9] | 16 → 16 | 1 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/5 | -0.3% [-0.3, -0.3] | 4 → 4 | 3 → 0 |
| gpt-6.1-sol | path_discovery | 3/5 | +10.5% [-9, 42.5] | 15 → 16 | 1 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/5 | -5.2% [-14.9, 17.4] | 15 → 15 | 2 → 0 |
| gpt-6.1-sol | stale_edit | 2/5 | +8.9% [0, 18] | 8 → 8 | 2 → 1 |
| gpt-6.1-sol | unprompted_edit | 2/5 | -0.2% [-0.5, 0.1] | 8 → 8 | 2 → 0 |

</details>

## `runaway-guard` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 12032 | 17624 | 3.14 | 94% | 0 |
| deepseek-v4-pro | 24/24 | 11892 | 17301 | 4.48 | 96% | 0 |
| gemini-3.8-flash-high | 24/24 | 11748 | 12715 | 3.48 | 0% | 0 |
| gpt-6.1-sol | 39/40 | 11730 | 11014 | 4.65 | 58% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_site_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 2 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 0 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 3 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 0 | runaway-guard | exit 1 |
| gpt-6.1-sol | stale_edit | 2 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 4 | pre-guard | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 1 | pre-guard | exit 1 |
| gpt-6.1-sol | unprompted_edit | 4 | pre-guard | wall_clock_limit |
