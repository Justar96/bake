# Native bake-rs arm against the TypeScript agent

Candidate `v0.3.5+d17d04bb6c-native` at `d17d04bb6c`, compared with `v0.3.5+d17d04bb6c-ts` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-13T08:35:31.389Z.

First live record of the native arm (roadmap scope 09): the same commit runs the TypeScript agent (base) and bake-rs -p (candidate). bake-rs carries Pi's prompt and tools (read, bash, edit, write), reaches cliproxyapi models through the read-only D25 import and DeepSeek over Chat Completions, and has no composition, roster, or delegation, so this measures two agents and its regressions are evidence, not a gate. gemini-3.8-flash-medium was not configured on the gateway on 2026-10-13, so the Gemini rows run gemini-3.8-flash-high at medium effort; they are not comparable with earlier records. Tokens: bake-rs spends about 30% fewer total tokens on Claude and DeepSeek, mostly from the smaller prompt and tool surface. Claude: requests +10 to 12% (extra check and edit rounds in shell_then_edit, stale_edit, and multi_file_edit); uncached input +47 to 58% is cache writes on each sample's first request, since the shorter prompt shares less prefix across samples, while every step still reads the cache. Gemini: requests +116%, tokens +43%, tool errors 0 -> 18: under Pi's prompt Gemini runs git commands in the fixture workspace, which is not a repository, and all 18 errors are "fatal: not a git repository"; accepted for this record and left to the prompt work in scope 10. deepseek-flash tool errors 0 -> 1: one failing check run. gpt-6.1-sol: 10/21 pairs. All 7 base failures are whitespace runaways after edit arguments that the llm-pi-ai guard stops; of the 7 candidate failures, 4 are the same runaway reaching the wall-clock limit because bake-ai has no runaway guard, and 3 are gateway errors (two "overloaded", one 5xx) with agent retry off. Porting the runaway guard to bake-ai is the follow-up these rows call for.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- claude-opus-5-5 vs v0.3.5+d17d04bb6c-ts: requests 11.8% [4.5, 20]
- claude-sonnet-5-5 vs v0.3.5+d17d04bb6c-ts: requests 10.1% [4.3, 16.7]
- deepseek-flash vs v0.3.5+d17d04bb6c-ts: tool errors 0 -> 1
- gemini-3.8-flash-high vs v0.3.5+d17d04bb6c-ts: total tokens 42.6% [20.1, 69.8]
- gemini-3.8-flash-high vs v0.3.5+d17d04bb6c-ts: requests 115.6% [89.8, 147.1]
- gemini-3.8-flash-high vs v0.3.5+d17d04bb6c-ts: tool errors 0 -> 18

## Against `v0.3.5+d17d04bb6c-ts`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21/21 | -29.4% [-33.5, -24.7] | +47.2% [37.8, 56.9] | 68 → 76 (+11.8% [4.5, 20]) | 78 → 80 | 93% → 85% | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 21/21 | -30.5% [-34.4, -26.1] | +58.3% [49.4, 69.3] | 69 → 76 (+10.1% [4.3, 16.7]) | 81 → 81 | 94% → 86% | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | -28.9% [-33.2, -23.8] | -41.8% [-43.5, -40] | 90 → 94 (+4.4% [-2.1, 11.8]) | 91 → 99 | 75% → 79% | 0 → 1 | 0 → 0 |
| deepseek-v4-pro | 21/21 | -33.3% [-41, -26.5] | -48% [-50.2, -45.5] | 94 → 95 (+1.1% [-7, 7.8]) | 93 → 93 | 77% → 82% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 21/21 | +42.6% [20.1, 69.8] | +43.8% [21.1, 71.7] | 77 → 166 (+115.6% [89.8, 147.1]) | 108 → 149 | 0% → 0% | 0 → 18 | 0 → 0 |
| gpt-6.1-sol | 10/21 | -17.8% [-22.5, -13.4] | +1.8% [-25.3, 39.8] | 45 → 49 (+8.9% [2.3, 15.2]) | 37 → 42 | 51% → 39% | 0 → 0 | 7 → 7 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | multi_file_edit | 3/3 | -28.1% [-40.1, -8.6] | 9 → 10 | 0 → 0 |
| claude-opus-5-5 | multi_site_edit | 3/3 | -37.1% [-37.6, -36.6] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | ordinary_edit | 3/3 | -36.6% [-37, -36] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | path_discovery | 3/3 | -33.5% [-35.8, -30.5] | 12 → 12 | 0 → 0 |
| claude-opus-5-5 | shell_then_edit | 3/3 | -18.2% [-24.8, -2.4] | 11 → 15 | 0 → 0 |
| claude-opus-5-5 | stale_edit | 3/3 | -17.7% [-17.8, -17.7] | 9 → 12 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | -35.9% [-36.7, -35] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | multi_file_edit | 3/3 | -15.2% [-15.3, -15.1] | 9 → 12 | 0 → 0 |
| claude-sonnet-5-5 | multi_site_edit | 3/3 | -37.3% [-37.3, -37.3] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | ordinary_edit | 3/3 | -23% [-36.7, -16.1] | 9 → 11 | 0 → 0 |
| claude-sonnet-5-5 | path_discovery | 3/3 | -30.9% [-36.4, -20.8] | 12 → 13 | 0 → 0 |
| claude-sonnet-5-5 | shell_then_edit | 3/3 | -39.2% [-39.2, -39.1] | 12 → 12 | 0 → 0 |
| claude-sonnet-5-5 | stale_edit | 3/3 | -31.1% [-38.1, -17.3] | 9 → 10 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | -34.4% [-34.4, -34.3] | 9 → 9 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -32% [-33.6, -29.4] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -28.3% [-36.6, -10.6] | 11 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -35% [-36.4, -33.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | -33.3% [-37.5, -27.9] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -27.2% [-34.5, -19.8] | 16 → 16 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -19.2% [-47, 5.5] | 12 → 14 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -25.9% [-32.7, -14.8] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | -21.2% [-32.3, -13.7] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | -33.9% [-34.5, -33.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | -33.5% [-34, -32.7] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | -30.9% [-42.4, -15.4] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -41.5% [-64.5, -5] | 19 → 17 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | -35.7% [-36.4, -34.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -32.3% [-34.4, -30.6] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 3/3 | +98.6% [35.8, 136.5] | 9 → 26 | 0 → 0 |
| gemini-3.8-flash-high | multi_site_edit | 3/3 | +14.2% [5.7, 30.6] | 9 → 16 | 0 → 0 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +40% [19.5, 61] | 10 → 21 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +21.3% [1.8, 34.3] | 15 → 25 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 3/3 | -0.9% [-34.5, 78] | 16 → 29 | 0 → 0 |
| gemini-3.8-flash-high | stale_edit | 3/3 | +97.3% [72.9, 122.6] | 9 → 24 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +100.7% [55.2, 161.5] | 9 → 25 | 0 → 0 |
| gpt-6.1-sol | multi_file_edit | 1/3 | -24.3% [-24.3, -24.3] | 5 → 5 | 1 → 2 |
| gpt-6.1-sol | multi_site_edit | 0/3 | n/a | 0 → 0 | 3 → 1 |
| gpt-6.1-sol | ordinary_edit | 1/3 | -8.1% [-8.1, -8.1] | 4 → 5 | 0 → 2 |
| gpt-6.1-sol | path_discovery | 1/3 | -19.5% [-19.5, -19.5] | 5 → 5 | 2 → 0 |
| gpt-6.1-sol | shell_then_edit | 3/3 | -10% [-10.4, -9.7] | 15 → 18 | 0 → 0 |
| gpt-6.1-sol | stale_edit | 2/3 | -25.6% [-26, -25.2] | 8 → 8 | 1 → 1 |
| gpt-6.1-sol | unprompted_edit | 2/3 | -24.9% [-25, -24.7] | 8 → 8 | 0 → 1 |

</details>

## `v0.3.5+d17d04bb6c-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 24/24 | 7497 | 12842 | 3.62 | 85% | 0 |
| claude-sonnet-5-5 | 24/24 | 7499 | 12636 | 3.62 | 86% | 0 |
| deepseek-flash | 24/24 | 7385 | 11604 | 4.48 | 79% | 1 |
| deepseek-v4-pro | 24/24 | 7386 | 11793 | 4.52 | 82% | 0 |
| gemini-3.8-flash-high | 24/24 | 7420 | 19949 | 7.9 | 0% | 18 |
| gpt-6.1-sol | 17/24 | 7410 | 10070 | 4.93 | 48% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 21 | 10 | 4 | 0 | 21/21 | 0 |
| claude-sonnet-5-5 | 21 | 10 | 7 | 0 | 21/21 | 0 |
| deepseek-flash | 21 | 28 | 18 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 29 | 21 | 0 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 100 | 15 | 3 | 21/21 | 0 |
| gpt-6.1-sol | 21 | 24 | 14 | 0 | 14/14 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6.1-sol | multi_file_edit | 0 | v0.3.5+d17d04bb6c-native | empty final reply |
| gpt-6.1-sol | multi_file_edit | 2 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | multi_file_edit | 2 | v0.3.5+d17d04bb6c-native | empty final reply |
| gpt-6.1-sol | multi_site_edit | 0 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | multi_site_edit | 1 | v0.3.5+d17d04bb6c-native | wall_clock_limit |
| gpt-6.1-sol | multi_site_edit | 2 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | ordinary_edit | 0 | v0.3.5+d17d04bb6c-native | empty final reply |
| gpt-6.1-sol | ordinary_edit | 2 | v0.3.5+d17d04bb6c-native | wall_clock_limit |
| gpt-6.1-sol | path_discovery | 1 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | path_discovery | 2 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | stale_edit | 1 | v0.3.5+d17d04bb6c-ts | runaway |
| gpt-6.1-sol | stale_edit | 1 | v0.3.5+d17d04bb6c-native | wall_clock_limit |
| gpt-6.1-sol | unprompted_edit | 2 | v0.3.5+d17d04bb6c-native | wall_clock_limit |
