# Agent-loop evals

English | [中文](README.zh.md)

Every Bake version records what its agent loop costs on a fixed task suite, measured against the version before it, so a regression shows up as a number rather than an impression. A record comes from a paired live run. The built headless CLI of each checkout runs the same scenarios through the same model gateway, and the arms are interleaved so that gateway and cache drift affect both alike.

## Layout

```text
evals/agent-loop/
  run.ts          paired runner: arms, scenarios, fixtures, wire capture
  record.ts       raw output to a committed record and a regression check
  accounting.ts   provider usage normalization
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

Run one process per model, in parallel. The runner reads the `cliproxyapi` route from `~/.bake/settings.yaml`, sends DeepSeek models to the official Anthropic-format endpoint through `llm-deepseek`, and copies `~/.bake/.credentials.yaml`, which holds `CLIPROXYAPI_API_KEY` and `DEEPSEEK_API_KEY`, into a private home for each sample. A record needs those locally; neither is ever written into the output. Raw output, including transcripts and captured requests, stays under the ignored `.preflight/`. The standard set alone takes about 40 minutes and 4 million tokens at three trials; each extended model adds its own process and tokens, and Opus costs the most per token.

A record covers both model sets. `EVAL_MODELS` takes model ids, set names, or both, so `EVAL_MODELS=standard` reruns the first three and `EVAL_MODELS=deepseek/deepseek-flash` one model.

| Set | Model | Route | Effort |
|---|---|---|---|
| `standard` | `gemini-3.8-flash-medium` | `cliproxyapi`, OpenAI Responses | medium |
| `standard` | `gpt-6.1-sol` | `cliproxyapi`, OpenAI Responses | medium |
| `standard` | `claude-sonnet-5-5` | `cliproxyapi`, Anthropic Messages | medium |
| `extended` | `gpt-6-astra` | `cliproxyapi`, OpenAI Responses | medium |
| `extended` | `claude-opus-5-5` | `cliproxyapi`, Anthropic Messages | medium |
| `extended` | `deepseek-flash` | DeepSeek official, Anthropic Messages | high |
| `extended` | `deepseek-v4-pro` | DeepSeek official, Anthropic Messages | high |

DeepSeek offers `low`, `high`, and `max` but no `medium`, so it runs at `high`, its default. `design.json` and each sample record the provider, wire format, and effort.

| Variable | Meaning |
|---|---|
| `EVAL_ARMS` | `name=checkout` pairs, two or more |
| `EVAL_MODELS` | model ids or set names, default `standard,extended`; a bare id is a `cliproxyapi` model, and `deepseek/<id>`, or a bare `deepseek-*` id the gateway does not list, is a DeepSeek model |
| `EVAL_CASES` | scenarios; the default is the standard suite below |
| `EVAL_TRIALS` | trials per scenario, default 3 |
| `EVAL_OUTPUT` | raw output directory |
| `EVAL_EXTRA_<ARM>` | a JSON array of extra overlay rows for one arm, for an attribution arm such as `[{"id":"fs-observation-policy","config":{"editGuard":"version"}}]` |
| `EVAL_SETTINGS_<ARM>` | a JSON object merged into one arm's `settings.yaml`, for settings no overlay row carries; `$PROVIDER` and `$MODEL` become the route under test |

| Scenario | What it checks |
|---|---|
| `no_tools` | the fixed prefix: first-request bytes and billed input |
| `ordinary_edit`, `unprompted_edit` | a one-line fix, with and without naming the tools |
| `path_discovery` | finding a moved module among 40 decoys |
| `stale_edit` | an external writer changes the file after the read |
| `multi_site_edit`, `multi_file_edit` | several changes in one file and across two |
| `shell_then_edit` | a shell step rewrites the file before the edit |
| `workflow_script` | a `workflow` script that runs two subagents |

`delegation` is outside the standard suite; name it in `EVAL_CASES` to measure subagent routing. It delegates the same two reads through the `subagent` tool and checks the same `summary.txt`. Each sample records its `subagentCalls` and the `routingDecisions` its session logs carry: who chose each child's route, and the model and effort. For a routing arm, give both arms the same `subagent-model-selection` allowlist through `EVAL_SETTINGS_<ARM>`, since the allowlist appears in the `subagent` tool's schema, and turn `router.enabled` on in one of them only.

A sample succeeds when the agent exits cleanly and an external check passes. That check is the exact `no_tools` reply, the fixture's `node test.cjs` with the test file unmodified, or the expected `summary.txt`. Where a scenario injects content, that content must also be kept.

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

`--fail-on-regression` exits non-zero for scripts. A flagged regression is either fixed before merge or explained in the record's `--note` and in the PR, with its cause. Some regressions are the intended price of a change, as when `workflow_script` pays for a `tool_help` read. Compare two versions only through a paired run. Absolute counts from runs on different days drift with the gateway, the provider's cache, and model updates.

## Rules

- Never edit or delete a committed record, and move one only when a release renames `unreleased`. A mistaken record is superseded by a new one whose note says why. `--replace` exists only for re-recording your own unmerged record.
- Keep raw output out of git. A record carries counters and one-line error texts only.
- Keep the suite stable. A new scenario joins the default list in its own change, so later records compare like with like, and its first record says it is new.
