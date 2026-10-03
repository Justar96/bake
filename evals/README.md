# Agent-loop evals

Every Bake version records what its agent loop costs on a fixed task suite, measured against the version before it, so a regression shows up as a number rather than an impression. A record comes from a paired live run. The built headless CLI of each checkout runs the same scenarios through the same model gateway, and the arms are interleaved so that gateway and cache drift affect both alike.

## Layout

```text
evals/agent-loop/
  run.ts          paired runner: arms, scenarios, fixtures, wire capture
  record.ts       raw output to a committed record and a regression check
  accounting.ts   provider usage normalization
  sim-router.ts   simulated task router for routing edge cases
  versions/
    v0.2.0/release/                       a release's own baseline
    unreleased/<YYYY-MM-DD>-<topic>/      one change since the last release
      REPORT.md  summary.json  samples.jsonl
      steps/<n>-<name>/                   optional intermediate measurements
```

`versions/<tag>` holds the records of a release, and `versions/unreleased` holds those of changes not yet released. When a release is cut, rename `unreleased` to the new tag in the release commit, as the changelog's `[Unreleased]` section is renamed. `REPORT.md` is the readable result, `summary.json` carries the same comparisons for tools, and `samples.jsonl` has one line of counters per run: no transcripts, file contents, or stderr.

## When to record

Record an eval for any change that can alter what a model sees or how many round trips a task takes. That includes prompts and personas; tool names, schemas, descriptions, arguments, results, and errors; context assembly, compaction, and caching; the agent loop; and LLM adapters. A change limited to the terminal UI, docs, or tests needs none.

## Run

Build a clean worktree of the base, which is the PR base or the previous release, and of the candidate. A dirty checkout is flagged in the record.

```sh
git worktree add --detach .agents/worktrees/eval-base <base-commit>
(cd .agents/worktrees/eval-base && bun install --frozen-lockfile && bun run build)
bun run build
EVAL_ARMS=base=.agents/worktrees/eval-base,candidate=. \
EVAL_MODELS=claude-sonnet-5-5 EVAL_OUTPUT=.preflight/evals/agent-loop/<topic>/claude-sonnet-5-5 \
  bun run eval
```

Run one process per model, in parallel. The runner reads the `cliproxyapi` route from `~/.bake/settings.yaml`, sends DeepSeek models to the official Anthropic-format endpoint through the shipped `llm-pi-ai` `deepseek-official` route (or, for an arm whose base bundle still mounts `llm-deepseek`, through that adapter's settings section), and copies `~/.bake/.credentials.yaml`, which holds `CLIPROXYAPI_API_KEY` and `DEEPSEEK_API_KEY`, into a private home for each sample. A record needs those locally; neither is ever written into the output. Raw output, including transcripts and captured requests, stays under the ignored `.preflight/`. The standard set alone takes about 40 minutes and 4 million tokens at three trials; each extended model adds its own process and tokens, and Opus costs the most per token.

A record covers both model sets. `EVAL_MODELS` takes model ids, set names, or both, so `EVAL_MODELS=standard` reruns the first three and `EVAL_MODELS=deepseek/deepseek-flash` one model.

| Set | Model | Route | Effort |
|---|---|---|---|
| `standard` | `gemini-3.8-flash-medium` | `cliproxyapi`, OpenAI Responses | medium |
| `standard` | `gpt-6.1-sol` | `cliproxyapi`, OpenAI Responses | medium |
| `standard` | `claude-sonnet-5-5` | `cliproxyapi`, Anthropic Messages | medium |
| `extended` | `claude-opus-5-5` | `cliproxyapi`, Anthropic Messages | medium |
| `extended` | `deepseek-flash` | DeepSeek official, Anthropic Messages | high |
| `extended` | `deepseek-v4-pro` | DeepSeek official, Anthropic Messages | high |

DeepSeek offers `low`, `high`, and `max` but no `medium`, so it runs at `high`, its default. `design.json` and each sample record the provider, wire format, and effort. Each raw sample's `requestMetrics` also carries the stream timing of every request: milliseconds from the proxy receiving it to the response headers, the first and last byte, and the stream's end (null if it never ended), with the event count and the last event type. A sample stopped at the wall-clock cap whose last request shows events that stop well before the cap points to the gateway, not the model.

| Variable | Meaning |
|---|---|
| `EVAL_ARMS` | `name=checkout` pairs, two or more; `name=pi` or `name=pi:<bin>` is the pi coding agent instead of a checkout |
| `EVAL_MODELS` | model ids or set names, default `standard,extended`; a bare id is a `cliproxyapi` model, and `deepseek/<id>`, or a bare `deepseek-*` id the gateway does not list, is a DeepSeek model; a trailing `@<effort>`, as in `gemini-3.8-flash-high@high`, replaces the model's default effort |
| `EVAL_CASES` | scenarios; the default is the standard suite below |
| `EVAL_TRIALS` | trials per scenario, default 3 |
| `EVAL_OUTPUT` | raw output directory |
| `EVAL_GATEWAY` | a JSON file of `{ baseUrl, apiKey }`, such as pi's `~/.pi/agent/cliproxyapi.json`, that replaces the `cliproxyapi` upstream and key for every arm; the proxy swaps each arm's key for this one, so both arms reach one upstream with one account. DeepSeek routes are unaffected |
| `EVAL_EXTRA_<ARM>` | a JSON array of extra overlay rows for one arm, for an attribution arm such as `[{"id":"fs-observation-policy","config":{"editGuard":"version"}}]` |
| `EVAL_SETTINGS_<ARM>` | a JSON object deep-merged into one arm's `settings.yaml`, for settings no overlay row carries; it can change one route key, such as `{"llm-pi-ai":{"providers":{"$PROVIDER":{"strictTools":true}}}}`, without replacing the route, and arrays replace; `$PROVIDER` and `$MODEL` become the route under test |

| Scenario | What it checks |
|---|---|
| `no_tools` | the fixed prefix: first-request bytes and billed input |
| `ordinary_edit`, `unprompted_edit` | a one-line fix, with and without naming the tools |
| `path_discovery` | finding a moved module among 40 decoys |
| `stale_edit` | an external writer changes the file after the read |
| `multi_site_edit`, `multi_file_edit` | several changes in one file and across two |
| `shell_then_edit` | a shell step rewrites the file before the edit |

`delegation` is outside the standard suite; name it in `EVAL_CASES` to measure subagent routing. It delegates two file reads through the `subagent` tool and checks the `summary.txt` the parent writes from their answers. Each sample records its `subagentCalls` and the `routingDecisions` its session logs carry: who chose each child's route, and the model and effort. For a routing arm, give both arms the same `subagent-model-selection` allowlist through `EVAL_SETTINGS_<ARM>`, since the allowlist appears in the `subagent` tool's schema, and turn `router.enabled` on in one of them only.

`delegation_auto` is the same task with the route left to the host, so every delegation reaches the task router when one is on. Each sample's `routingDecisions` also carries the router's fallback flag, assessment status, and one-line reason. To measure how routing degrades without the hosted router, run `bun evals/agent-loop/sim-router.ts` and point each arm's `router.url` at `http://127.0.0.1:18917/<case>`. Its cases answer a valid route, an effort the route may not list, a route outside the allowlist, a marked fallback, a reply slower than `router.timeoutMs`, HTTP 503, and a body that is not JSON. It refuses a request without its bearer token, so export `ING_API_TOKEN=sim-eval-token` for the run, or point `router.tokenEnv` at an unset variable to measure a missing token.

A sample succeeds when the agent exits cleanly and an external check passes. That check is the exact `no_tools` reply, the fixture's `node test.cjs` with the test file unmodified, or the expected `summary.txt`. Where a scenario injects content, that content must also be kept.

## Comparing with pi

An arm named `pi` (or `pi:<path to the CLI>`) runs the installed pi coding agent through the same capturing proxy, fixtures, and checks, so a record can compare Bake with pi on tool calls, round trips, tokens, and cache reads. Each sample runs `pi --mode json` in a private agent directory whose `models.json` points one `eval` provider at the proxy. pi keeps its own system prompt and default tools (`read`, `bash`, `edit`, `write`), with no session, extensions, skills, prompt templates, or context files, and agent-level retry is off as in the Bake arms. A `cliproxyapi` model uses the same gateway wire and model metadata as the Bake arm. A DeepSeek model uses pi's own DeepSeek catalog entry, which is Chat Completions at `api.deepseek.com`, so the two arms reach DeepSeek over different wires. The pi arm passes `CLIPROXYAPI_API_KEY` or `DEEPSEEK_API_KEY` from the environment, or from the reference of that name in `~/.bake/.credentials.yaml`. It skips the delegation scenarios, which need tools pi does not ship, so leave them out of `EVAL_CASES`. The stale writer runs as a pi extension on pi's `tool_result` event.

```sh
EVAL_ARMS=bake=.,pi=pi EVAL_MODELS=gpt-6.1-sol EVAL_CASES=no_tools,ordinary_edit,path_discovery,stale_edit,unprompted_edit,multi_site_edit,multi_file_edit,shell_then_edit \
  EVAL_OUTPUT=.preflight/evals/agent-loop/bake-vs-pi/gpt-6.1-sol bun run eval
```

When the default gateway serves one agent but not the other, as when a subscription bills pi's requests as third-party extra usage, point both arms at a gateway that serves both with `EVAL_GATEWAY`, rather than giving the arms different upstreams.

Record such a run with `--candidate bake --base pi`. The regression rule still applies, but a comparison with pi measures two agents rather than one change, so its record is evidence for where Bake costs more, not a gate.

## Record

```sh
bun run eval:record --raw .preflight/evals/agent-loop/<topic> \
  --out evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic> \
  --arm base=<base label>,candidate=<candidate label> --candidate candidate --base base \
  --title '<one line>' --note '<what changed and anything a reader must know>'
```

Labels name what an arm measured: a tag such as `v0.2.0`, `<tag>+<short commit>` for an unreleased commit, or a step name. Commit the record with the change it measures, or right after it on the same branch. Its `summary.json` names the exact commits.

## Regressions

`record.ts` flags a regression when, over all tasks for one model:

- total tokens rose, with the whole 95% interval above zero;
- the candidate failed at least two more runs than the base; or
- tool errors rose.

`--fail-on-regression` exits non-zero for scripts. A flagged regression is either fixed before merge or explained in the record's `--note` and in the PR, with its cause. Some regressions are the intended price of a change, as when a scenario pays for a `tool_help` read. Compare two versions only through a paired run. Absolute counts from runs on different days drift with the gateway, the provider's cache, and model updates.

## Rules

- Never edit or delete a committed record, and move one only when a release renames `unreleased`. A mistaken record is superseded by a new one whose note says why. `--replace` exists only for re-recording your own unmerged record.
- Keep raw output out of git. A record carries counters and one-line error texts only.
- Keep the suite stable. A new scenario joins the default list in its own change, so later records compare like with like, and its first record says it is new.
