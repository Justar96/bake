# The skills section against no skills listing

Candidate `v0.3.5+5892e87953-native` at `5892e87953`, compared with `v0.3.5+b74d1b8c99-native` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: instructions_file, skill_check, skill_distractors, unprompted_edit. Trials: 3. Started: 2026-10-13T15:45:39.639Z.

Measures the skills section that Pi's skills (#206) added to bake-rs's system prompt, with the new skill_check and skill_distractors scenarios and instructions_file and unprompted_edit as controls. The base is bake-rs at b74d1b8c99, before skills, which ignores .agents/skills; the candidate's bake-rs is rust/0.4.0 at 7cc4366dc7 (the binary the pi-skills record measured), which lists the fixture's skills. In the control scenarios the two arms' prompts differ only in the executable's path and the temporary workspace path. Run on the gateway Pi's CLIProxyAPI provider uses (EVAL_GATEWAY), with gemini-3.8-flash-high at medium effort in place of gemini-3.8-flash-medium, which the evaluator's model catalog lacks. After the run, skillLoads was recomputed from each sample's raw events once the metric counted a glob or find that prints every skill as *; no other field changed. Result: with the listing, every model loaded money-check and passed skill_check (18 of 18 samples, 2 GPT runaways aside) and no model loaded a distractor. Without it, Gemini never found the skill and failed all 3 by running only node test.cjs, DeepSeek V4 Pro failed 2 of 3 the same way, and Claude and DeepSeek Flash found it by listing .agents, sometimes printing every skill. The listing also saves round trips in skill_check: Claude 12 and 13 requests fall to 9, GPT 16 to 10 over its pairs. skill_distractors measures its cost: three skills add about 1,200 characters to every request, 10 to 20 percent of this small task's tokens, with no change in requests. Regressions: Gemini's +6.8% total tokens is that listing cost (+19.6% in skill_distractors) on a model without cache reads; its controls are within 1%, and its skill_check pairs drop out because the base failed all three. Accepted: the cost buys skill_check and grows only with the skills a user installs. gpt-6.1-sol's failures 0 -> 2 are both the known whitespace runaway in streamed tool arguments (75 KB and 454 KB of blanks before the 180 s limit), one in instructions_file, whose prompts are the same in both arms; not caused by the change.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gemini-3.8-flash-high vs v0.3.5+b74d1b8c99-native: total tokens 6.8% [1.8, 13.3]
- gpt-6.1-sol vs v0.3.5+b74d1b8c99-native: failures 0 -> 2

## Against `v0.3.5+b74d1b8c99-native`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 12/12 | -5.2% [-13.7, 4.9] | -6.2% [-23.3, 12.6] | 39 → 36 (-7.7% [-14.3, 0]) | 35 → 41 | 81% → 81% | 1 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 12/12 | -4.8% [-14.5, 5.2] | +8% [0, 18.1] | 40 → 36 (-10% [-20, 0]) | 38 → 37 | 84% → 82% | 1 → 0 | 0 → 0 |
| deepseek-flash | 12/12 | +14.3% [-0.8, 35.9] | +8.8% [2.1, 16] | 53 → 55 (+3.8% [-7, 19.6]) | 63 → 66 | 78% → 79% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 10/12 | -2% [-18.1, 17.1] | +1.6% [-10.9, 14.8] | 43 → 41 (-4.7% [-14.9, 5]) | 42 → 40 | 80% → 80% | 1 → 1 | 2 → 0 |
| gemini-3.8-flash-high | 9/12 | +6.8% [1.8, 13.3] | +5.9% [1.6, 12.1] | 27 → 27 (0% [0, 0]) | 33 → 33 | 0% → 0% | 0 → 0 | 3 → 0 |
| gpt-6.1-sol | 10/12 | -12.3% [-26.8, 4.3] | -11% [-32.8, 25.5] | 52 → 47 (-9.6% [-23, 6.7]) | 46 → 42 | 39% → 38% | 0 → 0 | 0 → 2 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | instructions_file | 3/3 | -1% [-4, 1.2] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | skill_check | 3/3 | -24.5% [-26.4, -23.3] | 12 → 9 | 0 → 0 |
| claude-opus-5-5 | skill_distractors | 3/3 | +15.9% [14.4, 18.1] | 9 → 9 | 0 → 0 |
| claude-opus-5-5 | unprompted_edit | 3/3 | +0.9% [-0.1, 2.5] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | instructions_file | 3/3 | -0.2% [-0.3, -0.2] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | skill_check | 3/3 | -23% [-34.2, -12.2] | 13 → 9 | 0 → 0 |
| claude-sonnet-5-5 | skill_distractors | 3/3 | +15.9% [15.9, 16] | 9 → 9 | 0 → 0 |
| claude-sonnet-5-5 | unprompted_edit | 3/3 | +0.1% [-0.2, 0.7] | 9 → 9 | 0 → 0 |
| deepseek-flash | instructions_file | 3/3 | +31.7% [-0.7, 141.1] | 13 → 16 | 0 → 0 |
| deepseek-flash | skill_check | 3/3 | +17.4% [-8.8, 43.1] | 16 → 16 | 0 → 0 |
| deepseek-flash | skill_distractors | 3/3 | +3.5% [-18.4, 17.6] | 12 → 11 | 0 → 0 |
| deepseek-flash | unprompted_edit | 3/3 | -2.1% [-2.9, -1.6] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | instructions_file | 3/3 | -0.1% [-36.9, 58.8] | 13 → 13 | 0 → 0 |
| deepseek-v4-pro | skill_check | 1/3 | -34.9% [-34.9, -34.9] | 6 → 4 | 2 → 0 |
| deepseek-v4-pro | skill_distractors | 3/3 | +14.6% [11.4, 20.1] | 12 → 12 | 0 → 0 |
| deepseek-v4-pro | unprompted_edit | 3/3 | -0.4% [-5.4, 4] | 12 → 12 | 0 → 0 |
| gemini-3.8-flash-high | instructions_file | 3/3 | +0.4% [-0.8, 1.6] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | skill_check | 0/3 | n/a | 0 → 0 | 3 → 0 |
| gemini-3.8-flash-high | skill_distractors | 3/3 | +19.6% [17.3, 23.2] | 9 → 9 | 0 → 0 |
| gemini-3.8-flash-high | unprompted_edit | 3/3 | +0.8% [-0.9, 2.6] | 9 → 9 | 0 → 0 |
| gpt-6.1-sol | instructions_file | 2/3 | -11% [-19.7, 0.1] | 9 → 9 | 0 → 1 |
| gpt-6.1-sol | skill_check | 2/3 | -32.5% [-46.4, -16] | 16 → 10 | 0 → 1 |
| gpt-6.1-sol | skill_distractors | 3/3 | +10.8% [-3.6, 22.9] | 14 → 14 | 0 → 0 |
| gpt-6.1-sol | unprompted_edit | 3/3 | -6.5% [-22.2, 27.9] | 13 → 14 | 0 → 0 |

</details>

## `v0.3.5+5892e87953-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 12/12 | n/a | 9857 | 3 | 81% | 0 |
| claude-sonnet-5-5 | 12/12 | n/a | 9643 | 3 | 82% | 0 |
| deepseek-flash | 12/12 | n/a | 13708 | 4.58 | 79% | 0 |
| deepseek-v4-pro | 12/12 | n/a | 12429 | 4.5 | 82% | 2 |
| gemini-3.8-flash-high | 12/12 | n/a | 6549 | 3 | 0% | 0 |
| gpt-6.1-sol | 10/12 | n/a | 9056 | 4.7 | 38% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 12 | 0 | 0 | 0 | 12/12 | 0 |
| claude-sonnet-5-5 | 12 | 0 | 0 | 0 | 12/12 | 0 |
| deepseek-flash | 12 | 19 | 9 | 1 | 12/12 | 0 |
| deepseek-v4-pro | 12 | 18 | 12 | 0 | 12/12 | 0 |
| gemini-3.8-flash-high | 12 | 0 | 0 | 0 | 12/12 | 0 |
| gpt-6.1-sol | 12 | 18 | 10 | 0 | 10/10 | 0 |

### Skills

| Model | Version | Samples | Needed skill loaded | Loaded a skill not needed |
|---|---|---|---|---|
| claude-opus-5-5 | v0.3.5+b74d1b8c99-native | 6 | 3/3 | 1/6 |
| claude-opus-5-5 | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |
| claude-sonnet-5-5 | v0.3.5+b74d1b8c99-native | 6 | 3/3 | 1/6 |
| claude-sonnet-5-5 | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |
| deepseek-flash | v0.3.5+b74d1b8c99-native | 6 | 3/3 | 2/6 |
| deepseek-flash | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |
| deepseek-v4-pro | v0.3.5+b74d1b8c99-native | 6 | 1/3 | 0/6 |
| deepseek-v4-pro | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |
| gemini-3.8-flash-high | v0.3.5+b74d1b8c99-native | 6 | 0/3 | 0/6 |
| gemini-3.8-flash-high | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |
| gpt-6.1-sol | v0.3.5+b74d1b8c99-native | 6 | 3/3 | 0/6 |
| gpt-6.1-sol | v0.3.5+5892e87953-native | 6 | 3/3 | 0/6 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| deepseek-v4-pro | skill_check | 0 | v0.3.5+b74d1b8c99-native | validation failed |
| deepseek-v4-pro | skill_check | 1 | v0.3.5+b74d1b8c99-native | validation failed |
| gemini-3.8-flash-high | skill_check | 0 | v0.3.5+b74d1b8c99-native | validation failed |
| gemini-3.8-flash-high | skill_check | 1 | v0.3.5+b74d1b8c99-native | validation failed |
| gemini-3.8-flash-high | skill_check | 2 | v0.3.5+b74d1b8c99-native | validation failed |
| gpt-6.1-sol | instructions_file | 0 | v0.3.5+5892e87953-native | wall_clock_limit |
| gpt-6.1-sol | skill_check | 2 | v0.3.5+5892e87953-native | wall_clock_limit |
