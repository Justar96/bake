# Task router edge cases against a simulated ing router: one valid route and eight degradation paths

Candidate `sim routed` at `7428f19adb`, compared with `v0.3.2+7428f19 router-off` in the same run.
Models: claude-opus-5-5, claude-sonnet-5-5, deepseek-flash, deepseek-v4-pro, gemini-3.8-flash-medium, gpt-6-astra, gpt-6.1-sol. Scenarios: delegation_auto. Trials: 1. Started: 2026-10-01T17:24:32.595Z.

Simulated ing router (evals/agent-loop/sim-router.ts) on the new opt-in delegation_auto case, which tells the model to leave the route to the host so every delegation reaches the router. One trial per arm and ten arms, so each model ran ten samples. claude-sonnet-5-5 ran in a later run of its own: CLIProxyAPI lists it without effort levels, so every run first exited at startup with UNSUPPORTED_REASONING_EFFORT medium; once the saved route took its efforts from pi-ai's Anthropic catalog entry, all ten of its arms succeeded with the same outcomes as below. router-off runs a clean worktree of 7428f19. The other arms run the working tree on 7428f19, whose only product change moves Auto routing to its own /settings tab, which nothing model-visible depends on. Every arm allows only the model's own route. All but router-off turn the router on with timeoutMs 1500. The comparison table covers routed against router-off only; samples.jsonl carries every arm with its routingDecisions. Outcomes for all 7 models: routed recorded source auto at effort low with a normal assessment, and the router received priority cost and the bearer token. nearest asked for max and recorded auto at max where the route lists it, and at high for gemini-3.8-flash-medium, which lists low to high, with a cautious assessment. policy (a route outside the allowlist), slow (a 3 s reply), refusal (HTTP 503), malformed (an HTML body), down (a closed port), and notoken (HTTP 401 without a token) all recorded source fallback at the default effort with the generic reason 'Router unavailable; default route retained.' fallback recorded source fallback with the router's own reason and its fallback assessment. No sample failed because of routing. The one failure, gpt-6-astra routed, is the gateway answering 'Our servers are currently overloaded' to the parent. gpt-6-astra's fallback and policy arms each made one tool error: the child's first request streamed 200 without completing, the tool returned only 'subagent run failed', and the model retried and succeeded; that child ran on the default route, as router-off does. deepseek-v4-pro's down arm made one tool error by passing a subagent id to job_output. The flagged token regressions are artifacts of a single pair, whose interval collapses to a point: deepseek-flash took 7 requests against 6, with one more bash call, and gemini-3.8-flash-medium 7 against 5, with an extra glob and read after the low-effort child answered. They are accepted as single-trial noise; a routing change that needs a token comparison should rerun delegation_auto at three trials.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

- deepseek-flash vs v0.3.2+7428f19 router-off: total tokens 36.4% [36.4, 36.4]
- gemini-3.8-flash-medium vs v0.3.2+7428f19 router-off: total tokens 65.9% [65.9, 65.9]

## Against `v0.3.2+7428f19 router-off`

| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 1/1 | 0% [0, 0] | -95.3% [-95.3, -95.3] | 5 → 5 | 0 → 0 | 0 → 0 |
| claude-sonnet-5-5 | 1/1 | -0.1% [-0.1, -0.1] | -98% [-98, -98] | 5 → 5 | 0 → 0 | 0 → 0 |
| deepseek-flash | 1/1 | +36.4% [36.4, 36.4] | +15.7% [15.7, 15.7] | 6 → 7 | 0 → 0 | 0 → 0 |
| deepseek-v4-pro | 1/1 | -23.3% [-23.3, -23.3] | -51.1% [-51.1, -51.1] | 7 → 6 | 0 → 0 | 0 → 0 |
| gemini-3.8-flash-medium | 1/1 | +65.9% [65.9, 65.9] | +67.7% [67.7, 67.7] | 5 → 7 | 0 → 0 | 0 → 0 |
| gpt-6-astra | 0/1 | n/a | n/a | 0 → 0 | 0 → 0 | 0 → 1 |
| gpt-6.1-sol | 1/1 | -45.6% [-45.6, -45.6] | -38.5% [-38.5, -38.5] | 7 → 6 | 0 → 0 | 0 → 0 |

<details><summary>Per scenario</summary>

| Model | Scenario | Pairs | Total tokens | Requests | Failures |
|---|---|---|---|---|---|
| claude-opus-5-5 | delegation_auto | 1/1 | 0% [0, 0] | 5 → 5 | 0 → 0 |
| claude-sonnet-5-5 | delegation_auto | 1/1 | -0.1% [-0.1, -0.1] | 5 → 5 | 0 → 0 |
| deepseek-flash | delegation_auto | 1/1 | +36.4% [36.4, 36.4] | 6 → 7 | 0 → 0 |
| deepseek-v4-pro | delegation_auto | 1/1 | -23.3% [-23.3, -23.3] | 7 → 6 | 0 → 0 |
| gemini-3.8-flash-medium | delegation_auto | 1/1 | +65.9% [65.9, 65.9] | 5 → 7 | 0 → 0 |
| gpt-6-astra | delegation_auto | 0/1 | n/a | 0 → 0 | 0 → 1 |
| gpt-6.1-sol | delegation_auto | 1/1 | -45.6% [-45.6, -45.6] | 7 → 6 | 0 → 0 |

</details>

## `sim routed` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-opus-5-5 | 1/1 | n/a | 25216 | 5 | 98% | 0 |
| claude-sonnet-5-5 | 1/1 | n/a | 25156 | 5 | 99% | 0 |
| deepseek-flash | 1/1 | n/a | 32720 | 7 | 97% | 0 |
| deepseek-v4-pro | 1/1 | n/a | 24973 | 6 | 98% | 0 |
| gemini-3.8-flash-medium | 1/1 | n/a | 28394 | 7 | 0% | 0 |
| gpt-6-astra | 0/1 | n/a | n/a | n/a | n/a | 0 |
| gpt-6.1-sol | 1/1 | n/a | 12709 | 6 | 29% | 0 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gpt-6-astra | delegation_auto | 0 | sim routed | exit 1 |
