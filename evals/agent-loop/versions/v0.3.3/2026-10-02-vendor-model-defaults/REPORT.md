# Vendor catalog defaults for hand-declared route models: claude-sonnet-5-5 over CLIProxyAPI

Candidate `7428f19+vendor-defaults` at `7428f19adb`, compared with `v0.3.2+7428f19` in the same run.
Models: claude-sonnet-5-5, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit, workflow_script. Trials: 1. Started: 2026-10-01T19:12:36.665Z.

Candidate: a hand-declared pi-ai route entry whose id its family's vendor publishes takes that vendor's context window, output capability, inputs, and, over the vendor's protocol, effort levels and adaptive thinking from the installed catalog, at route resolution; nothing is written to settings. The base is a clean worktree of 7428f19. Both arms read a copy of the settings saved before this work (HOME=/tmp/bake-eval-home), in which CLIProxyAPI lists claude-sonnet-5-5 bare. One trial of the standard suite per model. claude-sonnet-5-5: every base run exits at startup with UNSUPPORTED_REASONING_EFFORT medium before any request, so its 9 failures (8 task cells plus no_tools) and the absent token comparison are the defect this fixes; every candidate run succeeds, and its no_tools request carries thinking adaptive with output_config effort medium. gpt-6.1-sol is the control, whose saved entry already states its efforts: 18 of 18 succeed, total tokens 162205 against 161877, and the no_tools request bodies are identical apart from the cache key.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `v0.3.2+7428f19`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 0/8 | n/a | n/a | 0 → 0 | 0 → 0 | 8 → 0 |
| gpt-6.1-sol | 8/8 | 0% [-9.3, 8.9] | +8.5% [-23.5, 46.2] | 41 → 41 | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | path_discovery | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | stale_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 0/1 | n/a | 0 → 0 | 1 → 0 |
| claude-sonnet-5-5 | workflow_script | 0/1 | n/a | 0 → 0 | 1 → 0 |
| gpt-6.1-sol | multi_file_edit | 1/1 | -24.5% [-24.5, -24.5] | 5 → 4 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 1/1 | -2.1% [-2.1, -2.1] | 4 → 4 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 1/1 | 0% [0, 0] | 5 → 5 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 1/1 | +22.2% [22.2, 22.2] | 5 → 6 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 1/1 | -0.4% [-0.4, -0.4] | 6 → 6 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 1/1 | +9.3% [9.3, 9.3] | 4 → 4 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 1/1 | +0.2% [0.2, 0.2] | 4 → 4 | 0 → 0 |
| gpt-6.1-sol | workflow_script | 1/1 | -1.6% [-1.6, -1.6] | 8 → 8 | 0 → 0 |

</details>

## `7428f19+vendor-defaults` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 9/9 | 20895 | 27749 | 4 | 93% | 0 |
| gpt-6.1-sol | 9/9 | 20537 | 19793 | 5.13 | 64% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-sonnet-5-5 | multi_file_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | multi_site_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | no_tools | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | ordinary_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | path_discovery | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | shell_then_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | stale_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | unprompted_edit | 0 | v0.3.2+7428f19 | exit 1 |
| claude-sonnet-5-5 | workflow_script | 0 | v0.3.2+7428f19 | exit 1 |
