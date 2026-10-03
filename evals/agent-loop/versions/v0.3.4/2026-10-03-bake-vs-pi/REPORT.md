# Bake against pi 1.0.0

Candidate `v0.3.3+496d1b68ac-dirty` at `496d1b68ac`, compared with `pi-1.0.0` in the same run.
Models: claude-opus-5-5, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-02T22:17:10.633Z.

A comparison of two agents, not a change: Bake (this branch, uncommitted) against pi 1.0.0 with its own system prompt and default tools, through the same capturing proxy and fixtures. workflow_script is left out because pi ships no workflow tool. GPT 6.1 Sol at medium, Gemini 3.8 Flash High at high, DeepSeek V4 Pro at high, Claude Opus 5.5 at medium. On DeepSeek pi uses its own Chat Completions route while Bake uses the Anthropic-format endpoint. Opus runs through a second CLIProxyAPI gateway (EVAL_GATEWAY) for both arms, because the default gateway refused pi requests as out of extra usage. Regressions here mark where Bake costs more than pi, not a gate.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs pi-1.0.0: total tokens 166.1% [148.8, 187]
- deepseek-v4-pro vs pi-1.0.0: total tokens 153% [136.5, 168.1]
- gemini-3.8-flash-high vs pi-1.0.0: total tokens 189.7% [136.8, 256.8]
- gpt-6.1-sol vs pi-1.0.0: total tokens 129.9% [110.8, 152]

## Against `pi-1.0.0`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | +166.1% [148.8, 187] | -51.3% [-57.1, -45.9] | 76 → 75 | 78 → 72 | 83% → 97% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +153% [136.5, 168.1] | -62.7% [-67.6, -57.5] | 95 → 97 | 89 → 95 | 83% → 98% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +189.7% [136.8, 256.8] | +198.8% [143.5, 268.8] | 138 → 136 | 119 → 124 | 0% → 0% | 4 → 0 | 0 → 0 |
| gpt-6.1-sol | 21/21 | +129.9% [110.8, 152] | +31.1% [-1.2, 67.1] | 100 → 97 | 88 → 81 | 45% → 69% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | +159.3% [150.8, 167.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | +278.6% [277.9, 279.1] | 9 → 12 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | +144.1% [105.2, 170.6] | 10 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | +145.2% [136.2, 164.4] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | +141.6% [121.9, 179.9] | 15 → 13 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | +157.7% [110.5, 183.8] | 12 → 11 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +170.5% [166.5, 176.6] | 9 → 9 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +167% [143.6, 202.9] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +159.2% [142.8, 170.5] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +154.1% [150.5, 159] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +138.5% [73.5, 180.5] | 16 → 17 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +145.9% [120.1, 202.4] | 19 → 19 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +169.9% [168, 171.2] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +150.7% [136.3, 161.1] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +87.3% [47.4, 118.4] | 23 → 15 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +253.3% [207.7, 368.7] | 14 → 16 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +247.5% [136.8, 406.9] | 15 → 17 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +111.8% [50.8, 164.2] | 24 → 19 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | +439.5% [407, 458] | 21 → 35 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +104.3% [63.6, 144.7] | 26 → 19 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +216.9% [152.7, 284.1] | 15 → 15 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 3/3 | +134% [71.5, 222.4] | 14 → 14 | 0 → 0 |
| gpt-6.1-sol | multi_site_edit | 3/3 | +168.7% [139.8, 185.9] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | ordinary_edit | 3/3 | +91.8% [68.9, 137.4] | 15 → 13 | 0 → 0 |
| gpt-6.1-sol | path_discovery | 3/3 | +107.1% [60.4, 176.7] | 17 → 17 | 0 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | +114.7% [82.1, 140.5] | 18 → 17 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 3/3 | +154.4% [123.5, 208.9] | 12 → 12 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | +186.2% [144.1, 215] | 12 → 12 | 0 → 0 |

</details>

## `v0.3.3+496d1b68ac-dirty` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 20891 | 29636 | 3.57 | 97% | 0 |
| deepseek-v4-pro | 24/24 | 20751 | 26863 | 4.62 | 98% | 0 |
| gemini-3.8-flash-high | 24/24 | 20555 | 35823 | 6.48 | 0% | 0 |
| gpt-6.1-sol | 24/24 | 20537 | 19410 | 4.62 | 69% | 0 |
