# Opt-in codemode (D42): the tool off and on

Candidate `codemode-on+4cc1d29414` at `4cc1d29414`, compared with `codemode-off+4cc1d29414` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gemini-4-argon-high, gpt-6.1-sol. Scenarios: multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-19T11:52:39.406Z.

Both arms are bake-rs at 4cc1d29414; the on arm adds codemode to defaultTools through EVAL_SETTINGS_ON, which declares the codemode tool and gives every declared callable tool Pi's 'Codemode: tools.x(args) resolves to ...' line. The default model surface is unchanged from develop: the first-request fixtures are identical, so the standard suite needs no pair against develop. During the run the CLIProxyAPI gateway refused claude-opus-5-5, claude-sonnet-5-5, gemini-4-argon-high, and gpt-6.1-sol with 400 'unknown provider for model', so both control samples failed for each and the evaluator skipped their remaining cells; a direct bake-rs -p call to each got the same 400 afterwards while claude-haiku-4-5-20251001 answered. This record therefore covers deepseek-flash, deepseek-v4-pro, and gemini-3.8-flash-high only, and the four models need a superseding run once the gateway serves them. Flagged regressions, accepted because the tool is opt-in: total tokens rose 18.3% (deepseek-flash), 28.6% (deepseek-v4-pro), and 21.6% (gemini-3.8-flash-high) with no change in failures, because the tool definitions grow from about 4,800 to 7,000 characters, about 2.3 KB more in every request (first request 7,815 to 10,097 bytes on deepseek-flash); the standard tasks are single edits and short shell steps, so models rarely chose codemode (gemini-3.8-flash-high made 3 codemode calls across its on samples, the DeepSeek models none) and the extra definitions buy nothing on them. That cost is why codemode stays off by default, as Pi registers it inactive. gemini-3.8-flash-high's tool errors 1 to 2 are bash commands that failed (git outside a repository and a node assertion), one in each arm plus one more on, not codemode calls. The gemini-3.8-flash-high failures on trial 2 in both arms (one request, no output) are gateway errors that hit both arms alike.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-flash vs codemode-off+4cc1d29414: total tokens 18.3% [10.4, 26.3]
- deepseek-v4-pro vs codemode-off+4cc1d29414: total tokens 28.6% [19.2, 40.7]
- gemini-3.8-flash-high vs codemode-off+4cc1d29414: total tokens 21.6% [9, 38.5]
- gemini-3.8-flash-high vs codemode-off+4cc1d29414: tool errors 1 -> 2

## Against `codemode-off+4cc1d29414`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/0 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 0/0 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| deepseek-flash | 21/21 | +18.3% [10.4, 26.3] | +17.6% [12.8, 22.9] | 93 → 88 (-5.4% [-9.5, -2.1]) | 91 → 87 | 76% → 77% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 21/21 | +28.6% [19.2, 40.7] | +30.2% [25.3, 35.6] | 95 → 98 (+3.2% [-3.1, 10.5]) | 100 → 102 | 81% → 81% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 16/21 | +21.6% [9, 38.5] | +24.6% [10.2, 44.1] | 55 → 54 (-1.8% [-11.9, 10.4]) | 78 → 72 | 0% → 0% | 1 → 2 | 5 → 4 |
| gemini-4-argon-high | 0/0 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 0/0 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| deepseek-flash | multi_file_edit | 3/3 | +25.5% [19.6, 28.9] | 12 → 12 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | +13.4% [-10.2, 27.8] | 12 → 11 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | +26.2% [23.5, 29.6] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +36.1% [16.1, 57] | 15 → 15 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -0.2% [-9.6, 13.3] | 17 → 15 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | +19% [3, 31.4] | 13 → 12 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +11.9% [-9.4, 26.4] | 12 → 11 | 0 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +5.2% [-4.5, 22.6] | 15 → 13 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +15.2% [1.4, 25.5] | 14 → 13 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +24.3% [23.9, 25] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +21% [20.2, 22.4] | 15 → 15 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | +50% [20, 130.1] | 15 → 18 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +54.2% [31.4, 68.4] | 12 → 14 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | +38.8% [28.9, 56.8] | 12 → 13 | 0 → 0 |
| gemini-3.8-flash-high | multi_file_edit | 2/3 | +51% [-3.4, 122.2] | 7 → 8 | 1 → 1 |
| gemini-3.8-flash-high | multi_site_edit | 2/3 | +27.2% [26.9, 27.6] | 6 → 6 | 1 → 1 |
| gemini-3.8-flash-high | ordinary_edit | 3/3 | +37.2% [15.8, 70.3] | 9 → 10 | 0 → 0 |
| gemini-3.8-flash-high | path_discovery | 3/3 | +1.7% [-6.2, 20.8] | 15 → 12 | 0 → 0 |
| gemini-3.8-flash-high | shell_then_edit | 2/3 | +11% [-3.5, 25.1] | 6 → 6 | 1 → 1 |
| gemini-3.8-flash-high | stale_edit | 2/3 | +26.2% [18.3, 35.2] | 6 → 6 | 1 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 2/3 | +25.4% [18.6, 32.4] | 6 → 6 | 1 → 1 |

</details>

## `codemode-on+4cc1d29414` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/1 | 9829 | n/a | n/a | n/a | 0 |
| claude-sonnet-5-5 | 0/1 | 9831 | n/a | n/a | n/a | 0 |
| deepseek-flash | 24/24 | 9703 | 13359 | 4.19 | 77% | 0 |
| deepseek-v4-pro | 24/24 | 9704 | 15374 | 4.67 | 81% | 0 |
| gemini-3.8-flash-high | 20/24 | 9658 | 11823 | 3.35 | 0% | 2 |
| gemini-4-argon-high | 0/1 | 9658 | n/a | n/a | n/a | 0 |
| gpt-6.1-sol | 0/1 | 9650 | n/a | n/a | n/a | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0 | 0 | 0 | 0 | 0/0 | 0 |
| claude-sonnet-5-5 | 0 | 0 | 0 | 0 | 0/0 | 0 |
| deepseek-flash | 21 | 22 | 18 | 0 | 21/21 | 0 |
| deepseek-v4-pro | 21 | 32 | 21 | 1 | 21/21 | 0 |
| gemini-3.8-flash-high | 21 | 3 | 0 | 1 | 16/16 | 0 |
| gemini-4-argon-high | 0 | 0 | 0 | 0 | 0/0 | 0 |
| gpt-6.1-sol | 0 | 0 | 0 | 0 | 0/0 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | no_tools | 0 | codemode-off+4cc1d29414 | exit 1 |
| claude-opus-5-5 | no_tools | 0 | codemode-on+4cc1d29414 | exit 1 |
| claude-sonnet-5-5 | no_tools | 0 | codemode-off+4cc1d29414 | exit 1 |
| claude-sonnet-5-5 | no_tools | 0 | codemode-on+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | multi_file_edit | 2 | codemode-off+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | multi_file_edit | 2 | codemode-on+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | multi_site_edit | 2 | codemode-off+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | multi_site_edit | 2 | codemode-on+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | shell_then_edit | 2 | codemode-off+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | shell_then_edit | 2 | codemode-on+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | stale_edit | 2 | codemode-off+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | unprompted_edit | 2 | codemode-off+4cc1d29414 | exit 1 |
| gemini-3.8-flash-high | unprompted_edit | 2 | codemode-on+4cc1d29414 | exit 1 |
| gemini-4-argon-high | no_tools | 0 | codemode-off+4cc1d29414 | exit 1 |
| gemini-4-argon-high | no_tools | 0 | codemode-on+4cc1d29414 | exit 1 |
| gpt-6.1-sol | no_tools | 0 | codemode-off+4cc1d29414 | exit 1 |
| gpt-6.1-sol | no_tools | 0 | codemode-on+4cc1d29414 | exit 1 |
