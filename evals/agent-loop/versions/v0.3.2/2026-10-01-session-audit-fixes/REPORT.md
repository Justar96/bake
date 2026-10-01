# Session-audit fixes: shell change reports, any-root workflow schemas, tolerated timeout names, recovery errors, and the extended model set

Candidate `v0.3.1+734dffa+worktree` at `734dffa0fb`, compared with `v0.3.1+734dffa` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6-astra, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 3. Started: 2026-10-01T08:43:23.888Z.

The first record on the extended model set: gpt-6-astra and claude-opus-5-5 through CLIProxyAPI, and deepseek-flash and deepseek-v4-pro through DeepSeek's API at effort high, which has no medium. The candidate is the uncommitted working tree on 734dffa, measured against a clean worktree of 734dffa; summary.json flags it dirty. Model-visible changes: the edit description drops 'Use this, not shell scripts, to change files.'; the workflow help says agent() takes a schema with any root type, such as an object or {type: "string"}, and agent() now accepts one; bash and pwsh accept timeout and timeout_ms; and the workflow parse error, the subagent-limit error, and the unknown-job error are reworded, on failure paths the suite does not reach. Display-only changes that do not reach the model: shell change reports in tool/result.meta, the confined status-line git poll, the /resume picker, and the bounded edit and write diffs, whose model-visible echo changes only past 1,000 changed lines. An earlier run of this branch, before the agent() change, recorded gpt-6.1-sol passing agent() the schema {type: 'string'} and getting a tool error; that run motivated the change. Here no workflow call errs. Six candidate workflow_script runs (claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, gpt-6-astra) used a {type: "string"} schema, which no base run did; all six succeeded, with 9.0 requests on average against the base's 9.2. The flagged regression is accepted. gpt-6.1-sol's three candidate failures are a provider 'servers are currently overloaded' error on the first request of shell_then_edit trial 1, and two 180-second wall-clock limits: ordinary_edit trial 0, a scenario the base also failed this way, and workflow_script trial 1, which used no schema, had no tool errors, and took the same path as the base's passing trial 1 before running out of time. The claude-sonnet-5-5 arm was rerun after its saved reasoning efforts were dropped from ~/.bake/settings.yaml by a Bake build older than this branch's CLIProxyAPI refresh fix; its first attempt stopped on UNSUPPORTED_REASONING_EFFORT in both arms. gemini-3.8-flash-medium returned empty final replies in both arms (2 base, 3 candidate).

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs v0.3.1+734dffa: failures 1 -> 3

## Against `v0.3.1+734dffa`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | +0.5% [-3.2, 4.5] | -4.5% [-8.9, -1.6] | 98 → 99 | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 24/24 | -1.6% [-4.1, -0.2] | -1.1% [-4.7, 4.8] | 94 → 93 | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | -3.4% [-10.1, 3] | -1.8% [-11.3, 8.3] | 129 → 126 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 24/24 | -3.4% [-9.2, 2.7] | -14.4% [-23.8, -3.9] | 124 → 119 | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 20/24 | -5.7% [-12.6, 1.3] | -5.6% [-12.6, 1.4] | 112 → 106 | 0 → 0 | 2 → 3 |
| gpt-6-astra | 22/24 | -5.1% [-10.2, -0.5] | +10.5% [-13.8, 43.6] | 115 → 114 | 0 → 0 | 1 → 1 |
| gpt-6.1-sol | 20/24 | -2.5% [-10.4, 7.6] | +3.8% [-26.3, 51.4] | 108 → 107 | 0 → 0 | 1 → 3 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -0.3% [-0.7, 0] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -0.2% [-0.3, -0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | 0% [-0.1, 0.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -2.8% [-4.1, -0.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -8.2% [-20.7, -0.3] | 13 → 12 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +8.7% [-0.4, 32.7] | 11 → 12 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -0.7% [-0.9, -0.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | workflow_script | 3/3 | +6.4% [-0.5, 23.1] | 26 → 27 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -0.2% [-1.9, 1.6] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -0.3% [-0.4, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -1.3% [-3.1, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | 0% [-0.1, 0.1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -0.2% [-0.2, -0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -10% [-25, 0.1] | 10 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -0.7% [-1.6, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | workflow_script | 3/3 | -0.4% [-0.7, 0] | 24 → 24 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -15% [-21.4, 0.8] | 14 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | 0% [-20.8, 28.1] | 13 → 13 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +1.3% [0.5, 2.2] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +17.1% [0.6, 25.2] | 16 → 18 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -11.2% [-27.6, -2.1] | 18 → 17 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -7.8% [-21.2, 0.5] | 15 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -0.5% [-21.8, 24] | 13 → 13 | 0 → 0 |
| deepseek-flash | workflow_script | 3/3 | -8.6% [-22.4, 2.8] | 28 → 27 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -7.9% [-21.3, 2.3] | 13 → 12 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +0.4% [-21.3, 29.7] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -0.5% [-0.8, -0.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -2.2% [-4.6, 2.2] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -17.9% [-29.1, 1.8] | 19 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -0.1% [-0.8, 0.8] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -1.1% [-2.6, -0.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | workflow_script | 3/3 | +6.2% [-6.8, 31.5] | 28 → 27 | 0 → 0 |
| gemini-3.8-flash-medium | multi_file_edit | 3/3 | -19.7% [-27.3, 0.7] | 19 → 15 | 0 → 0 |
| gemini-3.8-flash-medium | multi_site_edit | 3/3 | -0.2% [-3.1, 2.6] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-medium | ordinary_edit | 3/3 | +7.3% [-0.2, 23.7] | 13 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | path_discovery | 3/3 | +7.2% [-0.5, 22.9] | 18 → 19 | 0 → 0 |
| gemini-3.8-flash-medium | shell_then_edit | 1/3 | -0.3% [-0.3, -0.3] | 6 → 6 | 2 → 1 |
| gemini-3.8-flash-medium | stale_edit | 2/3 | -12.1% [-21.2, -0.6] | 9 → 8 | 0 → 1 |
| gemini-3.8-flash-medium | unprompted_edit | 3/3 | -6% [-19.2, 1.1] | 15 → 14 | 0 → 0 |
| gemini-3.8-flash-medium | workflow_script | 2/3 | -19.1% [-31.4, -1.3] | 20 → 18 | 0 → 1 |
| gpt-6-astra | multi_file_edit | 2/3 | +4.3% [2.4, 6.1] | 10 → 11 | 0 → 1 |
| gpt-6-astra | multi_site_edit | 3/3 | -2.1% [-5.9, -0.1] | 12 → 12 | 0 → 0 |
| gpt-6-astra | ordinary_edit | 3/3 | -19.5% [-29.2, -0.4] | 15 → 13 | 0 → 0 |
| gpt-6-astra | path_discovery | 3/3 | -8.6% [-25.6, 0.1] | 18 → 17 | 0 → 0 |
| gpt-6-astra | shell_then_edit | 3/3 | -3.2% [-8.8, -0.4] | 18 → 18 | 0 → 0 |
| gpt-6-astra | stale_edit | 3/3 | -5.9% [-8.9, -0.2] | 12 → 12 | 0 → 0 |
| gpt-6-astra | unprompted_edit | 3/3 | -7% [-10, -1.7] | 12 → 12 | 0 → 0 |
| gpt-6-astra | workflow_script | 2/3 | +9.2% [0.2, 18.2] | 18 → 19 | 1 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | -9.7% [-27.2, 6.9] | 17 → 16 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +0.8% [-9.2, 12.7] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -9.4% [-9.4, -9.4] | 5 → 5 | 1 → 1 |
| gpt-6.1-sol | path_discovery | 3/3 | -15.5% [-24.1, -0.7] | 18 → 16 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 2/3 | +4.6% [-0.2, 9.8] | 12 → 12 | 0 → 1 |
| gpt-6.1-sol | stale_edit | 3/3 | -3.2% [-9, -0.1] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +0.1% [-10, 12.4] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 2/3 | +16.7% [-14.1, 81.5] | 20 → 22 | 0 → 1 |

</details>

## `v0.3.1+734dffa+worktree` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 27/27 | 20891 | 28938 | 4.13 | 95% | 0 |
| claude-sonnet-5-5 | 27/27 | 20895 | 26789 | 3.88 | 96% | 0 |
| deepseek-flash | 27/27 | 25563 | 28227 | 5.25 | 96% | 0 |
| deepseek-v4-pro | 27/27 | 25565 | 27030 | 4.96 | 98% | 0 |
| gemini-3.8-flash-medium | 24/27 | 20561 | 26872 | 5.33 | 0% | 0 |
| gpt-6-astra | 26/27 | 20537 | 21169 | 5.52 | 75% | 0 |
| gpt-6.1-sol | 24/27 | 20537 | 21132 | 5.33 | 69% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | shell_then_edit | 1 | v0.3.1+734dffa | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 2 | v0.3.1+734dffa | empty final reply |
| gemini-3.8-flash-medium | shell_then_edit | 2 | v0.3.1+734dffa+worktree | empty final reply |
| gemini-3.8-flash-medium | stale_edit | 0 | v0.3.1+734dffa+worktree | exit 1 |
| gemini-3.8-flash-medium | workflow_script | 0 | v0.3.1+734dffa+worktree | empty final reply |
| gpt-6-astra | multi_file_edit | 2 | v0.3.1+734dffa+worktree | exit 1 |
| gpt-6-astra | workflow_script | 0 | v0.3.1+734dffa | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 0 | v0.3.1+734dffa+worktree | wall_clock_limit |
| gpt-6.1-sol | ordinary_edit | 2 | v0.3.1+734dffa | wall_clock_limit |
| gpt-6.1-sol | shell_then_edit | 1 | v0.3.1+734dffa+worktree | exit 1 |
| gpt-6.1-sol | workflow_script | 1 | v0.3.1+734dffa+worktree | wall_clock_limit |
