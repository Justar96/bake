# Long-session compaction with pi's reserve on the forced window

Candidate `03941ee9e3-native` at `03941ee9e3`, compared with `pi-1.3.0` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: long_session. Trials: 3. Started: 2026-10-16T04:08:03.688Z.

Evaluator fix 03941ee9e3: pi and bake-rs arms on long_session now get pi's default 16,384-token compaction reserve on a 29,184-token window (Bake's 12,800-token threshold plus that reserve) with keepRecentTokens 2,560, in place of a 3,200-token reserve on the 16,000-token window. The old reserve sat below the 4,096 tokens pi's request clamp keeps back, so requests just under the threshold were capped at one output token, every DeepSeek compaction ran as overflow recovery mid-turn, and the turn-prefix summary got 1,600 tokens. Control (old evaluator at e84a1693c9, same bake-rs binary, DeepSeek only, 12 samples): all ten compactions ran as overflow recovery, and six samples failed with the turn-prefix summary at its token cap. This run (bake-rs built at 03941ee9e3 against pi 1.3.0, three trials, EVAL_GATEWAY): bake-rs passes 17/18, with every compaction triggered by the threshold and none failing; DeepSeek Flash and V4 Pro pass 6/6 in each arm. All six Claude pi samples failed on their first request because the gateway refused pi's requests as out of extra usage, so Claude has no pi measurement. gemini-3.8-flash-high at medium effort substitutes for the standard gemini-3.8-flash-medium alias, which the local catalog lacks. Compaction runs only where the provider's reported context crosses 12,800 tokens: every Claude sample, two of six DeepSeek bake-rs samples, and no Gemini or GPT sample. The GPT flags (tokens +24.6%, requests 15 to 17) and its one failure come from the write of codes.txt. In both measured bake-rs pairs GPT wrote the codes followed by a stray " } invalid?" and spent two more requests rewriting and editing it; the failure is the whitespace guard stopping that write after 2,001 whitespace characters. Each fault starts where the content string closes and pi's write schema ends, but bake-rs offers the D24 sandbox_permissions and justification parameters, which GPT also filled. pi's three GPT samples show neither fault. That parameter position is a lead for the GPT provider finding, not a measured cause. This record compares two agents and is evidence, not a gate.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- gpt-6.1-sol vs pi-1.3.0: total tokens 24.6% [21.2, 28.1]
- gpt-6.1-sol vs pi-1.3.0: requests 13.3% [13.3, 13.3]

## Against `pi-1.3.0`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | 3/3 | -4.7% [-10.3, 0.9] | -5% [-22.3, 8.4] | 48 → 40 (-16.7% [-23.5, -12.5]) | 51 → 46 | 87% → 87% | 1 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | +12.6% [-1.2, 47.1] | -6.7% [-18.9, 1.4] | 42 → 44 (+4.8% [0, 14.3]) | 44 → 46 | 88% → 90% | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-high | 3/3 | -26.7% [-27.5, -25.6] | -25.2% [-27.5, -21.8] | 45 → 36 (-20% [-20, -20]) | 42 → 43 | 2% → 0% | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 2/3 | +24.6% [21.2, 28.1] | +17.4% [-14.9, 88.7] | 30 → 34 (+13.3% [13.3, 13.3]) | 28 → 32 | 81% → 82% | 0 → 0 | 0 → 1 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | long_session | 0/3 | n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | long_session | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | long_session | 3/3 | -4.7% [-10.3, 0.9] | 48 → 40 | 0 → 0 |
| deepseek-v4-pro | long_session | 3/3 | +12.6% [-1.2, 47.1] | 42 → 44 | 0 → 0 |
| gemini-3.8-flash-high | long_session | 3/3 | -26.7% [-27.5, -25.6] | 45 → 36 | 0 → 0 |
| gpt-6.1-sol | long_session | 2/3 | +24.6% [21.2, 28.1] | 30 → 34 | 0 → 1 |

</details>

## `03941ee9e3-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 94715 | 13 | 81% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 93780 | 13 | 81% | 0 |
| deepseek-flash | 3/3 | n/a | 101659 | 13.33 | 87% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 117578 | 14.67 | 90% | 0 |
| gemini-3.8-flash-high | 3/3 | n/a | 84433 | 12 | 0% | 0 |
| gpt-6.1-sol | 2/3 | n/a | 140908 | 17 | 82% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 0 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 0 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 1 | 2 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 5 | 3 | 0 | 3/3 | 0 |
| gemini-3.8-flash-high | 3 | 0 | 0 | 0 | 3/3 | 0 |
| gpt-6.1-sol | 3 | 8 | 2 | 0 | 2/2 | 1 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | long_session | 0 | pi-1.3.0 | empty final reply |
| claude-opus-5-5 | long_session | 1 | pi-1.3.0 | empty final reply |
| claude-opus-5-5 | long_session | 2 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 0 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 1 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 2 | pi-1.3.0 | empty final reply |
| gpt-6.1-sol | long_session | 0 | 03941ee9e3-native | runaway |
