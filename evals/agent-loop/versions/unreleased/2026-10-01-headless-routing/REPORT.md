# Subagent routing in headless: task router on vs off on the delegation case, with blank route fields read as omitted

Candidate `router-on` at `794e39d0cb`, compared with `router-off` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6-astra, gpt-6.1-sol. Scenarios: delegation. Trials: 3. Started: 2026-10-01T11:43:24.335Z.

Both arms are the same working tree on 794e39d (dirty), run headless with subagent-model-selection enabled and an allowlist of the model's own route; router-on also turns router.enabled on against https://ing.gissx.org. The scenario is the opt-in delegation case, outside the standard suite; this is its first record. Model-visible changes covered: dsh headless now composes model selection, so with the setting on the subagent tool gains provider, model, and reasoning_effort and list_subagent_models joins the tools (with it off, the tool definitions were checked byte-identical to HEAD's headless patch, so the standard suite is unaffected); subagent and list_subagent_models read a blank route field as omitted; and a refused provider or route names the allowed routes. An earlier run of this branch before the blank-field change had gpt-6.1-sol send provider '' and model '' in every subagent and list_subagent_models call: 7 to 20 tool errors per run, 2 of 3 router-off runs failed, and router-on recorded the default route because the call never reached the router. Here gpt-6.1-sol made one tool error in six runs, a guessed deepseek/deepseek-chat route that it corrected on the next call from the allowed routes the error named. With one allowed route the router can change only the child's effort: it chose that route at effort low in all three trials for six models. For deepseek-flash it answered fallback all three times, because it has no benchmark or declared quality for that model, so the default route kept effort high. gemini-3.8-flash-medium's +45% tokens come from two router-on trials of 7 requests instead of 5: the low-effort child wrapped its answer in Markdown, and the parent re-read the notes or summary.txt before finishing; the interval includes zero, so it is not flagged. gpt-6.1-sol's router-on runs took 52 to 77 seconds against 25 to 33 router-off; the runner records no per-request timing, so that is not attributed.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

None under the rule in [evals/README.md](../../../../README.md#regressions).

## Against `router-off`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | +0.1% [-0.1, 0.3] | -86.5% [-95.3, 4862.5] | 15 → 15 | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 3/3 | 0% [-0.2, 0.2] | -86.7% [-95.5, 4562.5] | 15 → 15 | 0 → 0 | 0 → 0 |
| deepseek-flash | 3/3 | -17.3% [-24.1, -2.6] | -58.1% [-81.5, 0.8] | 20 → 18 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 3/3 | -13.7% [-22.7, 1.4] | -79.7% [-90, -27.8] | 20 → 18 | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 3/3 | +45% [-1.3, 68.4] | +46% [-0.1, 69.5] | 15 → 19 | 0 → 0 | 0 → 0 |
| gpt-6-astra | 3/3 | +11.2% [-0.1, 25.4] | -58.5% [-88.2, 222.3] | 17 → 16 | 0 → 0 | 0 → 0 |
| gpt-6.1-sol | 3/3 | -10.8% [-21.9, 0.2] | -28.3% [-46.7, -5.7] | 17 → 17 | 1 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | delegation | 3/3 | +0.1% [-0.1, 0.3] | 15 → 15 | 0 → 0 |
| claude-sonnet-5-5 | delegation | 3/3 | 0% [-0.2, 0.2] | 15 → 15 | 0 → 0 |
| deepseek-flash | delegation | 3/3 | -17.3% [-24.1, -2.6] | 20 → 18 | 0 → 0 |
| deepseek-v4-pro | delegation | 3/3 | -13.7% [-22.7, 1.4] | 20 → 18 | 0 → 0 |
| gemini-3.8-flash-medium | delegation | 3/3 | +45% [-1.3, 68.4] | 15 → 19 | 0 → 0 |
| gpt-6-astra | delegation | 3/3 | +11.2% [-0.1, 25.4] | 17 → 16 | 0 → 0 |
| gpt-6.1-sol | delegation | 3/3 | -10.8% [-21.9, 0.2] | 17 → 17 | 0 → 0 |

</details>

## `router-on` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 3/3 | n/a | 25108 | 5 | 98% | 0 |
| claude-sonnet-5-5 | 3/3 | n/a | 25046 | 5 | 99% | 0 |
| deepseek-flash | 3/3 | n/a | 23968 | 6 | 96% | 0 |
| deepseek-v4-pro | 3/3 | n/a | 24999 | 6 | 98% | 0 |
| gemini-3.8-flash-medium | 3/3 | n/a | 24301 | 6.33 | 0% | 0 |
| gpt-6-astra | 3/3 | n/a | 14679 | 5.33 | 81% | 0 |
| gpt-6.1-sol | 3/3 | n/a | 13192 | 5.67 | 40% | 0 |
