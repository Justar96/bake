# Rename core workspace packages to Bake names

Candidate `277a05fc73` at `277a05fc73`, compared with `a1ec50245e` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-05T08:55:02.888Z.

Renames the eight core workspace packages to bake-<name> and updates their consumers, aliases, Loader rows, and generated catalogs. All six required models ran the standard eight scenarios for three trials against clean base a1ec50245e and clean candidate 277a05fc73. Recorded message provenance and the shared scheduler symbol retain their released identities; all 548 persistence fingerprints are unchanged.

This eval measures the rename commit 277a05fc73. The subsequent e3a537e9db fix adds bake-* to the standalone migration worker's bundling rule; it changes packaging only and is validated separately by the isolated built-worker test and the full preflight (28 gates passed). Later edits update contributor documentation only.

The original Opus cohort is excluded: two candidate processes started while a preflight rebuild temporarily removed the launcher. The entire Opus cohort was repeated from a separate built worktree at the same 277a05fc73 revision; only that isolated cohort is included here. Other cohorts have no launcher-start failures. Captured first requests for no_tools and ordinary_edit, trial 0, match between arms after removing the per-session prompt_cache_key; this observation does not establish equality of every later request.

GPT has 5 baseline and 3 candidate failures from the provider streaming more than 2,000 trailing whitespace characters after tool arguments. These failures remain recorded; incomplete pairs are excluded from token comparisons by the recorder. One successful GPT baseline sample first tried to read a filename with an extra character, then recovered; its tool error remains recorded. These provider/model failures are not treated as package-rename regressions.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `a1ec50245e`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -0.1% [-0.3, 0.1] | +1.8% [-2.6, 9] | 66 → 66 | 71 → 72 | 96% → 96% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -0.5% [-1, 0] | +1.1% [-7.6, 13.5] | 66 → 66 | 66 → 66 | 96% → 96% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +1.4% [-4.7, 8.4] | +8.8% [-2.1, 22.2] | 88 → 89 | 100 → 94 | 94% → 94% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +0.3% [-3.3, 3.3] | +0.1% [-9.1, 10.8] | 91 → 91 | 96 → 93 | 96% → 96% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 21/21 | +3% [-3.4, 11] | +2.9% [-3.5, 11.1] | 71 → 73 | 98 → 97 | 0% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 13/21 | -1.8% [-10.8, 8.7] | +19.5% [-1.4, 44.3] | 59 → 59 | 48 → 48 | 59% → 50% | 1 → 0 | 5 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +0.1% [-0.1, 0.3] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -0.5% [-1.2, 0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -0.4% [-1.3, 0.1] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +0.5% [-0.2, 1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | 0% [-0.1, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.1% [-0.3, 0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -1.9% [-2.8, -0.1] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | 0% [-0.1, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -1.4% [-4.3, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -0.1% [-0.2, 0.1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | 0% [0, 0] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | 0% [-0.5, 0.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | 0% [0, 0.1] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -0.1% [-0.9, 0.5] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -8.3% [-26.3, 1.8] | 12 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +8.9% [-3, 35.6] | 11 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +6.8% [-20.7, 46.3] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | +4.1% [-9.5, 25.5] | 14 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -3.1% [-8.2, 0.1] | 12 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -0.3% [-1.7, 1.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +1.8% [0.6, 4.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -2.4% [-4.4, 1.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +0.1% [-0.1, 0.4] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +5.4% [2.1, 12.3] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -3.9% [-19.2, 11.3] | 16 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +0.6% [-0.1, 1.3] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | 0% [-0.4, 0.4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | +18.8% [-0.6, 68.2] | 11 → 13 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -1% [-4.1, 0.8] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +11.2% [-0.9, 34.1] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +0.2% [-0.2, 0.7] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 3/3 | -5.8% [-25.9, 6.9] | 12 → 11 | 0 → 0 |
| gemini-3.8-flash-medium | stale_edit | 3/3 | -1.4% [-4.4, 1.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -0.5% [-0.9, -0.1] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 2/3 | -14.1% [-16.6, -11.5] | 10 → 10 | 0 → 1 |
| gpt-6.1-sol | multi_site_edit | 1/3 | +17.6% [17.6, 17.6] | 4 → 4 | 2 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | -10.2% [-30.2, 3.9] | 13 → 13 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 2/3 | +9.4% [-13.4, 34.6] | 10 → 11 | 0 → 1 |
| gpt-6.1-sol | shell_then_edit | 1/3 | -14.4% [-14.4, -14.4] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | stale_edit | 2/3 | +4.5% [-6.2, 17.8] | 9 → 8 | 1 → 0 |
| gpt-6.1-sol | unprompted_edit | 2/3 | +7.8% [-0.1, 17] | 8 → 8 | 1 → 0 |

</details>

## `277a05fc73` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 12140 | 16457 | 3.14 | 96% | 0 |
| claude-sonnet-5-5 | 24/24 | 12144 | 16193 | 3.14 | 96% | 0 |
| deepseek-flash | 24/24 | 11998 | 16377 | 4.24 | 94% | 0 |
| deepseek-v4-pro | 24/24 | 12000 | 16727 | 4.33 | 96% | 0 |
| gemini-3.8-flash-medium | 24/24 | 11862 | 12228 | 3.48 | 0% | 0 |
| gpt-6.1-sol | 21/24 | 11838 | 11651 | 4.44 | 51% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 2 | 277a05fc73 | exit 1 |
| gpt-6.1-sol | multi_site_edit | 0 | a1ec50245e | exit 1 |
| gpt-6.1-sol | multi_site_edit | 1 | a1ec50245e | exit 1 |
| gpt-6.1-sol | path_discovery | 1 | 277a05fc73 | exit 1 |
| gpt-6.1-sol | shell_then_edit | 0 | a1ec50245e | exit 1 |
| gpt-6.1-sol | shell_then_edit | 1 | 277a05fc73 | exit 1 |
| gpt-6.1-sol | stale_edit | 2 | a1ec50245e | exit 1 |
| gpt-6.1-sol | unprompted_edit | 1 | a1ec50245e | exit 1 |
