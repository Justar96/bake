# Pi v1.4.0 port (standard suite and long_session)

Candidate `pi-1.4+60f7d3a9c4` at `60f7d3a9c4`, compared with `develop+f78800abc5` in the same run.
Models: claude-opus-5, claude-sonnet-5, deepseek-flash, deepseek-v4-pro, gemini-3-flash, gemini-3.5-flash-lite, gpt-6-sol, kimi-k2.7-code. Scenarios: long_session, multi_file_edit, multi_site_edit, no_tools, ordinary_edit, path_discovery, shell_then_edit, stale_edit, unprompted_edit. Trials: 3. Started: 2026-10-22T10:54:11.855Z.

Paired clean native release builds: develop@f78800abc5 (base) and the Pi v1.4.0 port at 60f7d3a9c4 (candidate), on the standard suite plus long_session, which exercises compaction, over all eight standard and extended model routes, three trials per arm, through the opn24 gateway (EVAL_GATEWAY): 432 samples, 430 successful. The candidate changes what the model sees in compaction summaries (no output cap; thinking blocks no longer serialized into the summary prompt; a split turn's prefix summary takes the custom instructions) and in requests this suite does not build: whitespace-only tool results, a recovered overflow or length-stop attempt, retry classification, and the codemode slot queue. No sample retried, recovered, or stopped on length, and no codemode script waited for a slot: 27 samples per arm used codemode, one script at a time. The first requests are identical on all eight models once each arm's paths and prompt-cache keys are normalized. Over long_session, the mean summary request input fell from 3,704 tokens (28 summaries) to 3,375 (26), unpaired; no summary failed in either arm. Regression flagged: gemini-3.5-flash-lite tool errors 18 to 26. Accepted: every error in both arms is the model's own bash or codemode call: a test run before the fix (10 in each arm), git commands in fixtures that are not repositories (8 to 14), and in the candidate one grep with no match and one codemode script that called require. None comes from a changed code path, both arms succeeded on all 24 of that model's tasks, and the requests that led to them start from the same first request. The two failures are the base's: deepseek-v4-pro long_session trials 1 and 2 wrote notes/codes.txt instead of codes.txt after summaries that restated the target as 'codes.txt in the same directory'. The candidate's three summaries said the current or fixture directory, too few samples to attribute to the thinking omission.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.5-flash-lite vs develop+f78800abc5: tool errors 18 -> 26

## Against `develop+f78800abc5`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5 | 24/24 | -2.6% [-15.1, 11.6] | -0.2% [-41.7, 73.9] | 99 → 97 (-2% [-12.5, 9.5]) | 93 → 93 | 83% → 82% | 0 → 0 | 0 → 0 |
| claude-sonnet-5 | 24/24 | -6.5% [-19.7, 4.6] | -17.7% [-39, 3.9] | 136 → 130 (-4.4% [-13.5, 3.1]) | 126 → 119 | 83% → 85% | 0 → 0 | 0 → 0 |
| deepseek-flash | 24/24 | -0.2% [-8.7, 9.3] | +0.9% [-3.1, 5.7] | 137 → 135 (-1.5% [-6.2, 2.9]) | 133 → 127 | 79% → 79% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 22/24 | +1.6% [-4.2, 9.1] | -1% [-3.8, 3.3] | 111 → 114 (+2.7% [-3.6, 8.9]) | 120 → 120 | 80% → 80% | 0 → 0 | 2 → 0 |
| gemini-3-flash | 24/24 | -1.9% [-16.8, 15.5] | -2% [-17.2, 17] | 193 → 191 (-1% [-13.9, 14.2]) | 191 → 197 | 20% → 20% | 25 → 25 | 0 → 0 |
| gemini-3.5-flash-lite | 24/24 | -1.1% [-13.3, 12.3] | -8.1% [-19.8, 4.9] | 198 → 190 (-4% [-14.4, 6.4]) | 188 → 174 | 22% → 28% | 18 → 26 | 0 → 0 |
| gpt-6-sol | 24/24 | +1.9% [-37.9, 74] | -0.5% [-31.5, 45.4] | 107 → 109 (+1.9% [-16, 27]) | 89 → 92 | 60% → 61% | 1 → 0 | 0 → 0 |
| kimi-k2.7-code | 24/24 | -2.6% [-8.9, 1.3] | -2.1% [-8.1, 1.3] | 141 → 136 (-3.5% [-8.1, 0]) | 145 → 138 | 84% → 84% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5 | long_session | 3/3 | -1.9% [-37.2, 84.8] | 18 → 18 | 0 → 0 |
| claude-opus-5 | multi_file_edit | 3/3 | -8.7% [-25, 5.5] | 10 → 9 | 0 → 0 |
| claude-opus-5 | multi_site_edit | 3/3 | +12.2% [-22.8, 63.1] | 10 → 11 | 0 → 0 |
| claude-opus-5 | ordinary_edit | 3/3 | -0.4% [-6, 5.1] | 9 → 9 | 0 → 0 |
| claude-opus-5 | path_discovery | 3/3 | +0.1% [-0.7, 1] | 15 → 15 | 0 → 0 |
| claude-opus-5 | shell_then_edit | 3/3 | -22.1% [-40.3, -0.1] | 15 → 12 | 0 → 0 |
| claude-opus-5 | stale_edit | 3/3 | +13.1% [-0.2, 21.9] | 12 → 14 | 0 → 0 |
| claude-opus-5 | unprompted_edit | 3/3 | -8.1% [-25.9, 6.9] | 10 → 9 | 0 → 0 |
| claude-sonnet-5 | long_session | 3/3 | -13.3% [-43.1, 8.3] | 49 → 43 | 0 → 0 |
| claude-sonnet-5 | multi_file_edit | 3/3 | +0.1% [0.1, 0.2] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | multi_site_edit | 3/3 | +0.2% [-0.2, 0.4] | 12 → 12 | 0 → 0 |
| claude-sonnet-5 | ordinary_edit | 3/3 | +9.2% [-1.2, 32.6] | 10 → 11 | 0 → 0 |
| claude-sonnet-5 | path_discovery | 3/3 | +5.6% [1, 9.6] | 13 → 13 | 0 → 0 |
| claude-sonnet-5 | shell_then_edit | 3/3 | +7.2% [0.7, 19.8] | 15 → 16 | 0 → 0 |
| claude-sonnet-5 | stale_edit | 3/3 | -7.8% [-20.6, 0.5] | 13 → 12 | 0 → 0 |
| claude-sonnet-5 | unprompted_edit | 3/3 | -8.2% [-24.7, 0.2] | 12 → 11 | 0 → 0 |
| deepseek-flash | long_session | 3/3 | +3.2% [-10.4, 24.5] | 45 → 47 | 0 → 0 |
| deepseek-flash | multi_file_edit | 3/3 | -10.9% [-28, -2.1] | 12 → 11 | 0 → 0 |
| deepseek-flash | multi_site_edit | 3/3 | -1.6% [-5.2, 0.9] | 12 → 12 | 0 → 0 |
| deepseek-flash | ordinary_edit | 3/3 | -1.4% [-3.3, -0.3] | 12 → 12 | 0 → 0 |
| deepseek-flash | path_discovery | 3/3 | +23.8% [11.4, 34.2] | 15 → 16 | 0 → 0 |
| deepseek-flash | shell_then_edit | 3/3 | -19.6% [-23.7, -12.7] | 16 → 14 | 0 → 0 |
| deepseek-flash | stale_edit | 3/3 | -18.8% [-30.7, -1] | 13 → 11 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | +0.3% [-2.7, 3.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | long_session | 1/3 | -1.9% [-1.9, -1.9] | 13 → 14 | 2 → 0 |
| deepseek-v4-pro | multi_file_edit | 3/3 | +9.6% [-22.3, 32.1] | 13 → 14 | 0 → 0 |
| deepseek-v4-pro | multi_site_edit | 3/3 | +9% [-0.5, 32.1] | 14 → 15 | 0 → 0 |
| deepseek-v4-pro | ordinary_edit | 3/3 | +11.5% [1.6, 27.9] | 12 → 13 | 0 → 0 |
| deepseek-v4-pro | path_discovery | 3/3 | +8.2% [0.2, 23.1] | 15 → 16 | 0 → 0 |
| deepseek-v4-pro | shell_then_edit | 3/3 | -13.2% [-21.6, -3.2] | 18 → 16 | 0 → 0 |
| deepseek-v4-pro | stale_edit | 3/3 | +0.6% [-22.3, 30.7] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | 0% [-23.9, 33.8] | 13 → 13 | 0 → 0 |
| gemini-3-flash | long_session | 3/3 | -17.4% [-53.1, 14.7] | 43 → 33 | 0 → 0 |
| gemini-3-flash | multi_file_edit | 3/3 | +1.3% [-13.5, 52.6] | 35 → 37 | 0 → 0 |
| gemini-3-flash | multi_site_edit | 3/3 | +4.6% [-14.4, 37.2] | 16 → 16 | 0 → 0 |
| gemini-3-flash | ordinary_edit | 3/3 | +28.9% [12.5, 68.7] | 19 → 24 | 0 → 0 |
| gemini-3-flash | path_discovery | 3/3 | +14.8% [-10.3, 40.7] | 23 → 24 | 0 → 0 |
| gemini-3-flash | shell_then_edit | 3/3 | -8.3% [-40.1, 14] | 21 → 21 | 0 → 0 |
| gemini-3-flash | stale_edit | 3/3 | +42.6% [-41, 164.2] | 13 → 18 | 0 → 0 |
| gemini-3-flash | unprompted_edit | 3/3 | -24.9% [-56.5, 50.1] | 23 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | long_session | 3/3 | +3.7% [-13.7, 35.7] | 47 → 49 | 0 → 0 |
| gemini-3.5-flash-lite | multi_file_edit | 3/3 | -6.3% [-33.4, 31.8] | 32 → 27 | 0 → 0 |
| gemini-3.5-flash-lite | multi_site_edit | 3/3 | -1% [-20.1, 16] | 18 → 18 | 0 → 0 |
| gemini-3.5-flash-lite | ordinary_edit | 3/3 | +42.4% [28.6, 52.2] | 15 → 20 | 0 → 0 |
| gemini-3.5-flash-lite | path_discovery | 3/3 | +28.8% [1.9, 89.2] | 21 → 26 | 0 → 0 |
| gemini-3.5-flash-lite | shell_then_edit | 3/3 | -28.8% [-40.6, -15.5] | 25 → 19 | 0 → 0 |
| gemini-3.5-flash-lite | stale_edit | 3/3 | -53.4% [-59.1, -43.7] | 21 → 10 | 0 → 0 |
| gemini-3.5-flash-lite | unprompted_edit | 3/3 | +20.9% [6.8, 50] | 19 → 21 | 0 → 0 |
| gpt-6-sol | long_session | 3/3 | +3.2% [-69.6, 367.6] | 26 → 27 | 0 → 0 |
| gpt-6-sol | multi_file_edit | 3/3 | +0.9% [-0.6, 3] | 9 → 9 | 0 → 0 |
| gpt-6-sol | multi_site_edit | 3/3 | -11.6% [-28, 0.1] | 10 → 9 | 0 → 0 |
| gpt-6-sol | ordinary_edit | 3/3 | 0% [-0.2, 0.1] | 12 → 12 | 0 → 0 |
| gpt-6-sol | path_discovery | 3/3 | +7.6% [-1.6, 29.2] | 13 → 14 | 0 → 0 |
| gpt-6-sol | shell_then_edit | 3/3 | +6.8% [-0.2, 22.1] | 16 → 17 | 0 → 0 |
| gpt-6-sol | stale_edit | 3/3 | +0.1% [0, 0.4] | 12 → 12 | 0 → 0 |
| gpt-6-sol | unprompted_edit | 3/3 | -2.1% [-4.1, -0.1] | 9 → 9 | 0 → 0 |
| kimi-k2.7-code | long_session | 3/3 | +2.3% [-2.3, 4.9] | 44 → 45 | 0 → 0 |
| kimi-k2.7-code | multi_file_edit | 3/3 | -23.9% [-38.7, -1.7] | 15 → 12 | 0 → 0 |
| kimi-k2.7-code | multi_site_edit | 3/3 | -8.1% [-24.8, 3.1] | 14 → 13 | 0 → 0 |
| kimi-k2.7-code | ordinary_edit | 3/3 | -1.4% [-6.1, 1.5] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | path_discovery | 3/3 | -4.5% [-15.6, 4.6] | 16 → 15 | 0 → 0 |
| kimi-k2.7-code | shell_then_edit | 3/3 | -11.4% [-26.2, -0.7] | 16 → 15 | 0 → 0 |
| kimi-k2.7-code | stale_edit | 3/3 | -0.4% [-1.1, -0.1] | 12 → 12 | 0 → 0 |
| kimi-k2.7-code | unprompted_edit | 3/3 | +0.2% [-5.1, 3.3] | 12 → 12 | 0 → 0 |

</details>

## `pi-1.4+60f7d3a9c4` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5 | 27/27 | 9861 | 20652 | 4.04 | 82% | 0 |
| claude-sonnet-5 | 27/27 | 9863 | 32136 | 5.42 | 85% | 0 |
| deepseek-flash | 27/27 | 9737 | 25531 | 5.63 | 79% | 0 |
| deepseek-v4-pro | 27/27 | 9738 | 26880 | 6 | 80% | 0 |
| gemini-3-flash | 27/27 | 9687 | 36165 | 7.96 | 20% | 25 |
| gemini-3.5-flash-lite | 27/27 | 9694 | 38001 | 7.92 | 28% | 26 |
| gpt-6-sol | 27/27 | 9682 | 14774 | 4.54 | 61% | 0 |
| kimi-k2.7-code | 27/27 | 9732 | 24172 | 5.67 | 84% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5 | 24 | 13 | 10 | 0 | 18/18 | 0 |
| claude-sonnet-5 | 24 | 28 | 19 | 0 | 24/24 | 0 |
| deepseek-flash | 24 | 30 | 22 | 0 | 24/24 | 0 |
| deepseek-v4-pro | 24 | 39 | 24 | 0 | 24/24 | 0 |
| gemini-3-flash | 24 | 92 | 20 | 3 | 24/24 | 0 |
| gemini-3.5-flash-lite | 24 | 85 | 18 | 4 | 24/24 | 0 |
| gpt-6-sol | 24 | 17 | 12 | 0 | 12/12 | 0 |
| kimi-k2.7-code | 24 | 31 | 24 | 2 | 24/24 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-v4-pro | long_session | 1 | develop+f78800abc5 | validation failed |
| deepseek-v4-pro | long_session | 2 | develop+f78800abc5 | validation failed |
