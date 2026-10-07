# Agent-loop evals

Every Bake version records what its agent loop costs on a fixed task suite, measured against the version before it, so a regression shows up as a number rather than an impression. A record comes from a paired live run. The built headless CLI of each checkout runs the same scenarios through the same model gateway, and the arms are interleaved so that gateway and cache drift affect both alike.

## Layout

```text
evals/agent-loop/
  run.ts          paired runner: routes, the capturing proxy, sample summaries
  arms.ts         checkout resolution, per-sample Bake overlays, and built CLI arguments
  scenarios.ts    each scenario's prompt, fixture, success predicate, request floor and cap
  composition.ts  each arm's overlay rows, read from its own checkout, and the rendered-prompt check
  run.test.ts     keyless checks of actual Bake and pi launches, configuration, and test cleanup
  native-fixture.ts  keyless executable adapter for the existing ordinary_edit fixture
  native-smoke.ts    compiled Rust fake-arm checks for the native preflight gate
  metrics.ts      loop-shape metrics from a sample's event stream
  record.ts       raw output to a committed record and a regression check
  rules.ts        the paired statistics and the regression rule
  accounting.ts   provider usage normalization
  sim-router.ts   simulated task router for routing edge cases
  *.test.ts       unit tests and fixture dry checks, run by the preflight evals-unit step
  versions/
    v0.2.0/release/                       a release's own baseline
    unreleased/<YYYY-MM-DD>-<topic>/      one change since the last release
      REPORT.md  summary.json  samples.jsonl
      steps/<n>-<name>/                   optional intermediate measurements
```

`versions/<tag>` holds the records of a release, and `versions/unreleased` holds those of changes not yet released. When a release is cut, rename `unreleased` to the new tag in the release commit, as the changelog's `[Unreleased]` section is renamed. `REPORT.md` is the readable result, `summary.json` carries the same comparisons for tools, and `samples.jsonl` has one line of counters per run: no transcripts, file contents, or stderr.

## When to record

Record an eval for any change that can alter what a model sees or how many round trips a task takes. That includes prompts and personas; tool names, schemas, descriptions, arguments, results, and errors; context assembly, compaction, and caching; the agent loop; and LLM adapters. A change limited to the terminal UI, docs, or tests needs none. A diff in the [model-surface snapshots](../docs/testing.md#pin-the-model-surface), which pin each shipped composition's first request, means the change reached the model and needs a record; a change to how the snapshots are rendered does not.

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
| `EVAL_CASES` | scenario names or set names, `standard` or `extended`; the default is the standard suite below |
| `EVAL_ROSTER` | `headless`, the default, runs the headless bundle's own tool configs; `tui` gives every Bake arm the terminal's, as described under [Composition](#composition) |
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

### Extended scenarios

The `extended` set is opt-in (`EVAL_CASES=extended`, or any of its names) and stays out of the default suite, so records of the standard suite remain comparable. Its scenarios aim at regressions the standard suite cannot see in prompts and tools.

| Scenario | What it checks | Success also needs |
|---|---|---|
| `large_file_edit` | a bug near the end of a 1,392-line, 43 KB module, named by symbol only | |
| `explore_answer` | a question about a built TypeScript project, answerable by grep, with a stale `lib/` build (`*.js`, `*.d.ts`) and a `node_modules` dependency naming the same constant among about 30 source files | a final reply of exactly `47250` after trimming, and no file changed |
| `test_fix_loop` | two chained bugs: the second assertion fails only once the first bug is fixed | |
| `unprompted_verify` | the `roundMoney` fix with no mention of tests; `ranCheck` and `verifiedBeforeFinal` record whether the agent ran `node test.cjs` on its own | |
| `noisy_failure` | a test that prints about 74 KB of interleaved stdout and stderr, with one `ASSERTION FAILED` line in the middle | |
| `background_test` | a 20-second `slow-check.cjs` the prompt asks to start in the background while the agent fixes `roundMoney`; `backgroundStarts` counts `run_in_background` calls | the line `slow-check.cjs` prints, quoted in the final reply; `slow-check.cjs` and `src/report.js` unchanged |
| `instructions_file` | an `AGENTS.md` that names `node scripts/check.cjs --all` as the project's only accepted check; this scenario alone keeps `agent-instructions` mounted, and a pi arm keeps its context files | the stamp that check writes matches the final source, so it ran after the last edit; `scripts/check.cjs` and `AGENTS.md` unchanged |
| `edit_recovery` | the obvious `old_string` appears twice, in `roundMoney` and `truncateMoney` | `truncateMoney` still truncates |
| `long_session` | ten 4.4 KB notes chained by hashed names, each naming the next, so they are read one per response; a `codes.txt` built from them; then the `roundMoney` fix. The route's `contextWindow` is forced to 16,000 tokens: each read is about 1,200 tokens, under the 2,560-token tail compaction keeps, and the session crosses the 12,800-token threshold around the seventh note. `compactions` counts completed summaries | the ten codes in `codes.txt`; the notes unchanged |

Most scenarios keep the 14-request cap; `explore_answer`, `test_fix_loop`, and `background_test` allow 20, `noisy_failure` and `edit_recovery` 16, and `long_session` 30, which also counts its summary requests. `background_test` may run 240 s and `long_session` 360 s instead of 180 s. An arm whose route still goes through the retired `llm-deepseek` adapter cannot take the forced window, so its `long_session` samples record `contextWindow: null`. A pi arm gets the same forced window and compaction settings that mirror Bake's default policy at it (`reserveTokens` 3,200 and `keepRecentTokens` 2,560 at 16,000 tokens); `design.json` records them as `piCompaction`. Where a predicate trusts a fixture file, such as a check script, the fixture records each such file's hash and the sample fails if one changed; `fixturesUnchanged` records the result, null where nothing is protected. `ask_user_question` is never in the roster: a headless run has no one to answer it.

`bun test ./evals` builds every fixture, judges it untouched and after a hand-applied reference fix, and checks the sizes above, without a model.

## Composition

Each Bake arm runs its own built headless CLI under an overlay that `composition.ts` reads from that arm's checkout, so an older revision is measured as it shipped. A Cordis patch replaces a row's whole `config`, so every overlay row restates the config it needs:

- `system-prompt` is the headless bundle's own row, with the harness opener off (`includeHarnessIdentity: false`) and the `Your working directory is {{cwd}}.` suffix, and with the persona prefix replaced by that arm's standard-preset persona.
- With `EVAL_ROSTER=tui`, `tool-fs` (`readMaxBytes: 16384`), `tool-fs-search`, and `tool-result-pruner` (8,192, 4,096, and 1,024 characters) take the standard preset's configs, and `spill-policy` takes the terminal profile's inline cap (`maxInlineBytes: 16384` in `apps/tui/packages/app/cordis.built.patch.yml`, against the base bundle's 50,000). A checkout whose terminal patch sets no cap keeps the base bundle's.

Rows are found by id, so a checkout from before the `bake-*` rename, with `@deepseek-ai/dsh-*` row names, composes the same way. The runner captures the system prompt of each sample's first request in the raw sample (`systemPrompt`), and `composition` records whether it has the `powered by DeepSeek Harness` opener and the working-directory line. A Bake sample whose prompt fails that check stops the run, keeping the samples so far. `design.json` and every sample record the roster.

Records made before this composition, up to and including `versions/unreleased/2026-10-05-bake-core-names`, set only `personaPrefix` on `system-prompt`, which replaced the bundle row's config. Their prompts open with `You are an AI agent powered by DeepSeek Harness.` and lack the working-directory line, so their first requests differ from what Bake ships by those two lines. Both arms of each such record carried the same difference, so its paired comparisons stand. Its absolute counts, such as first-request bytes, are not comparable with later records; compare versions only through a new paired run.

### Launcher contract checks

Run the evaluator's launch checks without model credentials:

```sh
bun test --timeout=30000 evals/agent-loop/run.test.ts
```

The tests run the real evaluator with two small Bake checkout fixtures and a pi recorder. They check each arm's arguments, private homes, settings, overlay order, effort, context window, instruction policy, and stale-writer hooks under both rosters. A hung recorder, failed readiness check, and failed child lookup exercise the test harness's process cleanup. The fixtures use dummy credentials and send no model request; every recorded sample remains unsuccessful with no measured usage or prompt-composition result.

The process cases require Linux or macOS with `ps`; they are skipped on Windows because the live evaluator uses POSIX process groups and the pi fixture uses a shebang. The hook tests run separately. These checks establish launch configuration only; provider streams, token accounting, and live evaluation still need their own evidence. The `evals-unit` preflight gate includes them. `bun run typecheck` builds the dependency declarations before checking `arms.ts` and these tests through `tsconfig.launch.json`; running that project alone requires those declarations to exist.

### Native fixture adapter

Run a compiled Rust test arm through the evaluator's real `ordinary_edit` fixture without model credentials:

```sh
bun run check:rust
bun run test:rust:eval
```

The `bake-eval-fake-arm` binary makes a fixed edit; it has no agent or model implementation. The smoke command accepts the correct edit and rejects three controls: a rewritten `test.cjs` that itself exits successfully, a success claim without an edit, and a correct edit from a process that exits with failure. It also checks the exact prompt received by the compiled arm. Its final JSON line identifies the binary and prompt hashes, host platform, and case count. The `rust-eval` preflight gate runs this command after Cargo builds on Linux, macOS, and Windows; the check inside each fixture uses Node 24 in CI.

[`native-fixture.ts`](agent-loop/native-fixture.ts) exports `runNativeFixture` for trusted single-process executables. It sends the existing scenario prompt as UTF-8 on stdin, launches in a private workspace with private home and temporary directories, then calls the existing evaluator predicate after the process closes. The arm and the `node test.cjs` check receive a minimal environment. Success requires unchanged evaluator-owned tests, a passing check, exit 0, and no process fault; stdout is bounded diagnostic output and cannot establish success. The adapter removes its private root before returning, including on failure.

Only `ordinary_edit` is supported. Invalid arguments, unsupported scenarios, and a missing executable fail before creating a workspace. The conformance driver's launcher owns cancellation, timeouts, stream limits, POSIX group termination, and Windows direct-child termination. It does not stop descendants after a normal parent exit or contain a hostile executable. The fixture adapter is development tooling: live `EVAL_ARMS`, model routes, token accounting, session logs, composition parity, and committed eval records keep their existing behavior. A real native model arm remains open work.

The fixture check executes arm-editable code and resolves `node` through the host's command search path, so the check and that path must also be trusted. Its existing synchronous five-second timeout follows the arm's budget, including after cancellation, and blocks other callbacks in the driver while it runs. This adapter does not qualify hostile-check containment or strict end-to-end deadlines.

Run the adapter's script-arm tests with `bun test --timeout=30000 evals/agent-loop/native-fixture.test.ts`. They cover tampering, unsuccessful processes, cancellation, timeouts, overflow, environment isolation, and cleanup. The ordinary `evals-unit` gate runs them on Linux and macOS; the compiled-arm smoke supplies the Windows fixture evidence. The launch TypeScript project also checks the adapter and smoke command.

## Comparing with pi

An arm named `pi` (or `pi:<path to the CLI>`) runs the installed pi coding agent through the same capturing proxy, fixtures, and checks, so a record can compare Bake with pi on tool calls, round trips, tokens, and cache reads. Each sample runs `pi --mode json` in a private agent directory whose `models.json` points one `eval` provider at the proxy. pi keeps its own system prompt and default tools (`read`, `bash`, `edit`, `write`), with no session, extensions, skills, prompt templates, or context files, and agent-level retry is off as in the Bake arms. A `cliproxyapi` model uses the same gateway wire and model metadata as the Bake arm. A DeepSeek model uses pi's own DeepSeek catalog entry, which is Chat Completions at `api.deepseek.com`, so the two arms reach DeepSeek over different wires. The pi arm passes `CLIPROXYAPI_API_KEY` or `DEEPSEEK_API_KEY` from the environment, or from the reference of that name in `~/.bake/.credentials.yaml`. It skips the delegation scenarios, which need tools pi does not ship, so leave them out of `EVAL_CASES`. The stale writer runs as a pi extension on pi's `tool_result` event.

```sh
EVAL_ARMS=bake=.,pi=pi EVAL_MODELS=gpt-6.1-sol EVAL_CASES=no_tools,ordinary_edit,path_discovery,stale_edit,unprompted_edit,multi_site_edit,multi_file_edit,shell_then_edit \
  EVAL_OUTPUT=.preflight/evals/agent-loop/bake-vs-pi/gpt-6.1-sol bun run eval
```

When the default gateway serves one agent but not the other, as when a subscription bills pi's requests as third-party extra usage, point both arms at a gateway that serves both with `EVAL_GATEWAY`, rather than giving the arms different upstreams.

Record such a run with `--candidate bake --base pi`. The regression rule still applies, but a comparison with pi measures two agents rather than one change, so its record is evidence for where Bake costs more, not a gate.

## Metrics

Besides tokens, requests, tool calls, and errors, every sample records these loop-shape metrics, and `samples.jsonl` keeps them. A check call is a `bash` or `pwsh` call that runs the scenario's check: `node test.cjs`, or `node scripts/check.cjs --all` in `instructions_file`. An edit is an `edit` or `write` call, or a shell command that writes a source file (the `shellEdits` pattern).

| Metric | Definition |
|---|---|
| `excessRequests` | requests above the scenario's floor in `REQUEST_FLOORS` (`requestFloor`), the fewest a run that batches independent calls needs; never negative |
| `requestsOverFloor` | requests minus the floor, unclamped: a negative value means the sample beat the floor, so the floor is set too high; the `loop` block counts such samples as `belowFloor` |
| `editCheckSplits` | steps whose calls are all edits, followed by a step whose first call is the check: a round trip saved by sending the check with the edit |
| `orientationCalls` | a shell command whose every stage but `cd` is `pwd`, `ls` or `tree` without a path or at the workspace root, or `find` at the root with no `-name` or `-path` filter; an `ls` call at the root; or a `glob` at the root whose pattern is a bare listing (`*`, `**`, `**/*`, `**/*.ext`, no name stem). Counted before the first read, by `read` or by a shell `cat`, `head`, `tail`, `less`, or `sed -n` on a file (or in the whole sample, if it never reads) |
| `ranCheck` | whether any call ran the check |
| `verifiedBeforeFinal` | whether a check call came at or after the last edit; null when the sample made no edit |
| `backgroundStarts` | shell calls started with `run_in_background` |
| `compactions` | completed summarizing compactions (`compaction/summary`) and compactions that ended in an error, from the session log |
| `runaway` | the llm-pi-ai whitespace-runaway guard aborted a tool call's stream; such a failure is recorded as `runaway` rather than its exit code |

`summary.json` adds, per model, a `loop` block for the candidate (the sums of these metrics, `verifiedBeforeFinal` as verified over edited samples, runaways, and samples whose composition check failed) and, in each comparison, `requestsChange` (the paired change in requests with its interval), `excessRequests`, `editCheckSplits`, and `orientationCalls`. Existing fields keep their names and meaning. `REPORT.md` shows the requests change next to the request counts and a loop-shape table. Samples recorded before these metrics existed carry nulls and are left out of the loop sums.

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
- requests rose by more than 10%, with the whole 95% interval above zero;
- the candidate failed at least two more runs than the base; or
- tool errors rose.

`--fail-on-regression` exits non-zero for scripts. A flagged regression is either fixed before merge or explained in the record's `--note` and in the PR, with its cause. Some regressions are the intended price of a change, as when a scenario pays for a `tool_help` read. Compare two versions only through a paired run. Absolute counts from runs on different days drift with the gateway, the provider's cache, and model updates.

## Rules

- Never edit or delete a committed record, and move one only when a release renames `unreleased`. A mistaken record is superseded by a new one whose note says why. `--replace` exists only for re-recording your own unmerged record.
- Keep raw output out of git. A record carries counters and one-line error texts only.
- Keep the suite stable. A new scenario joins the default list in its own change, so later records compare like with like, and its first record says it is new. The `extended` set is opt-in for the same reason.
