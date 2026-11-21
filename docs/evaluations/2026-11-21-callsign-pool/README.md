# Evaluation: 2026-11-21-callsign-pool

The base is a clean build of `4083bfae10` (0.4.10). The candidate is the same commit with only the model-facing part of the callsign pool change applied: `crates/bake-coding-agent/src/telecom/*` (callsigns drawn from a pool, `names.rs`), the `callsigns` setting (`settings.rs`, `cli/build.rs`), the `telecom` tool description's example callsigns, from `(alpha, bravo, ...;` to `(such as nova or rook;`, and the Mermaid leniency with the `visualize` tool's diagram errors (`crates/bake-mermaid`, `tools/visualize.rs`). Both arms ran with `{"telecom": true}`, so the `telecom` tool and its description reached the model in every sample; no sample had a team to talk to. The `visualize` tool is not among a print-mode run's tools, so its changed errors were not read by any model here.

What this shows: all 20 samples passed, and with every task weighing the same, model requests moved +5.2% (-5.7% to +16.8%) and total tokens +2.6% (-9.6% to +15.4%), each interval containing zero and each within the six-task Gemini noise floor's width (about ±7% requests, ±9% tokens). No change beyond noise. Limits: one model, two small tasks, five pairs each; GPT and Claude, the other eleven tasks, a team with members, and the `visualize` errors were not measured. A first attempt of this run gave `--tasks` as a relative path, so every check failed to open; it was discarded and this record is its rerun.

Run on 2026-11-21 on linux/x86_64. Base: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/base-4083bfae10/target/release/bake`, 73274728 bytes). Candidate: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/cand-bravo-telecom/target/release/bake`, 73320920 bytes). 2 tasks, 1 models, and 5 trials of each by each arm: 20 samples, of which 20 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 2 samples ran at once.

Settings of the samples, besides the provider account: base `{"telecom":true}`, candidate `{"telecom":true}`.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 40 | 42 | +5.0% | -4.3% to +15.4% |
| Total tokens | 140,491 | 143,788 | +2.3% | -8.1% to +14.8% |
| Uncached input tokens | 107,377 | 110,560 | +3.0% | -4.3% to +10.7% |
| Output tokens | 6,847 | 7,081 | +3.4% | -7.0% to +15.0% |
| Tool calls by the model | 90 | 93 | +3.3% | -8.8% to +18.9% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 5 | 5 | +0.0% | n/a |
| Run time, seconds | 145 | 131 | -9.7% | -28.1% to +17.7% |

With every task weighing the same: model requests +5.2% (-5.7% to +16.8%), total tokens +2.6% (-9.6% to +15.4%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 17,142 | 17,368 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,956 | 11,390 |

## Samples that did not pass

None.

## Prompts

- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
