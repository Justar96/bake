# Long-session compaction on every model

Candidate `575e220813-native` at `575e220813`, compared with `pi-1.3.0` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-high, gpt-6.1-sol. Scenarios: long_session. Trials: 3. Started: 2026-10-16T05:12:05.351Z.

long_session as fixed on this branch: pi and bake-rs arms get pi's default 16,384-token reserve on a 29,184-token window (03941ee9e3); the notes grow from 4.4 KB to about 6 KB so that the provider's reported context crosses the 12,800-token threshold (0b8cfded9c); each code follows the fifth paragraph, inside the 2,000 characters of a tool result pi's summarizer sees (48f25152b9); and the prompt no longer asks for each note to be read in full (575e220813). Each fix answers a failure measured on this branch, and the earlier fixtures are not part of this record. In the 2026-10-16-long-session-budget record, compaction ran in 8 of 18 bake-rs samples. With 6 KB notes and a mid-file code, GPT lost the codes at each compaction and reread the chain until the request cap in all six samples, in both arms. With the code moved but "in full" kept, the summarizer wrote that the notes were not read in full, and GPT again reread the chain until the cap in all six samples. In this run, bake-rs at 575e220813 against pi 1.3.0, with three trials and EVAL_GATEWAY, bake-rs passes 18/18 and compacts in 17. The one sample without compaction is a DeepSeek Flash run that found the codes with a shell search in five requests. Each compaction was triggered by the threshold, and none failed. pi passes 13/15 of its measured samples: DeepSeek V4 Pro wrote codes.txt under notes/, and Gemini ended with an empty reply. All six Claude pi samples failed on their first request because the gateway refused pi's requests as out of extra usage, so Claude has no pi measurement. gemini-3.8-flash-high at medium effort substitutes for the standard gemini-3.8-flash-medium alias. This record compares two agents and is evidence, not a gate. The two token flags, DeepSeek V4 Pro +5.9% and GPT +7.8%, compare two agents. bake-rs's tool schemas are about 870 characters longer than pi's: D24's sandbox_permissions and justification parameters on bash, edit, and write, and their descriptions. That adds about 220 tokens to every request of a 15-request session. GPT's trial 1 also took two more requests. Neither flag measures a change on this branch.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-v4-pro vs pi-1.3.0: total tokens 5.9% [4.4, 7.3]
- gpt-6.1-sol vs pi-1.3.0: total tokens 7.8% [1, 22.1]

## Against `pi-1.3.0`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool calls | Cache-read share | Tool errors | Failures |
|---|---|---|---|---|---|---|---|---|
| claude-opus-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | 0/3 | n/a | n/a | 0 → 0 (n/a) | 0 → 0 | n/a → n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | 3/3 | -27% [-77, 6.3] | -23.8% [-73.6, 3.2] | 45 → 35 (-22.2% [-66.7, 7.1]) | 49 → 40 | 83% → 82% | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 2/3 | +5.9% [4.4, 7.3] | +7% [6.3, 7.7] | 30 → 31 (+3.3% [0, 6.7]) | 31 → 32 | 82% → 82% | 0 → 0 | 1 → 0 |
| gemini-3.8-flash-high | 2/3 | -11.6% [-15, -8.2] | -12.6% [-17.1, -8.2] | 32 → 27 (-15.6% [-18.7, -12.5]) | 28 → 29 | 1% → 3% | 0 → 0 | 1 → 0 |
| gpt-6.1-sol | 3/3 | +7.8% [1, 22.1] | +23.8% [-5.7, 50.3] | 48 → 50 (+4.2% [0, 12.5]) | 42 → 44 | 71% → 67% | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | long_session | 0/3 | n/a | 0 → 0 | 3 → 0 |
| claude-sonnet-5-5 | long_session | 0/3 | n/a | 0 → 0 | 3 → 0 |
| deepseek-flash | long_session | 3/3 | -27% [-77, 6.3] | 45 → 35 | 0 → 0 |
| deepseek-v4-pro | long_session | 2/3 | +5.9% [4.4, 7.3] | 30 → 31 | 1 → 0 |
| gemini-3.8-flash-high | long_session | 2/3 | -11.6% [-15, -8.2] | 32 → 27 | 1 → 0 |
| gpt-6.1-sol | long_session | 3/3 | +7.8% [1, 22.1] | 48 → 50 | 0 → 0 |

</details>

## `575e220813-native` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 102333 | 14.67 | 75% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 91073 | 12.67 | 80% | 0 |
| deepseek-flash | 3/3 | n/a | 70584 | 11.67 | 82% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 97915 | 15.33 | 82% | 0 |
| gemini-3.8-flash-high | 3/3 | n/a | 79893 | 13.67 | 2% | 0 |
| gpt-6.1-sol | 3/3 | n/a | 93889 | 16.67 | 67% | 0 |

### Loop shape

| Model | Measured | Excess requests | Edit/check splits | Orientation calls | Verified before final | Runaways |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3 | 5 | 0 | 0 | 3/3 | 0 |
| claude-sonnet-5-5 | 3 | 1 | 0 | 0 | 3/3 | 0 |
| deepseek-flash | 3 | 4 | 3 | 0 | 3/3 | 0 |
| deepseek-v4-pro | 3 | 7 | 3 | 0 | 3/3 | 0 |
| gemini-3.8-flash-high | 3 | 2 | 0 | 0 | 3/3 | 0 |
| gpt-6.1-sol | 3 | 11 | 3 | 0 | 3/3 | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| claude-opus-5-5 | long_session | 0 | pi-1.3.0 | empty final reply |
| claude-opus-5-5 | long_session | 1 | pi-1.3.0 | empty final reply |
| claude-opus-5-5 | long_session | 2 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 0 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 1 | pi-1.3.0 | empty final reply |
| claude-sonnet-5-5 | long_session | 2 | pi-1.3.0 | empty final reply |
| deepseek-v4-pro | long_session | 0 | pi-1.3.0 | validation failed |
| gemini-3.8-flash-high | long_session | 2 | pi-1.3.0 | empty final reply |
