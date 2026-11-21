# Evaluation: Lean model-neutral default prompt

Run on 2026-11-20 on linux/x86_64. Base: `bake 0.4.10` (`/tmp/bake-eval-alpha11-base-4083bfae10/target/release/bake`, 73274648 bytes). Candidate: `bake 0.4.10` (`/tmp/bake-eval-alpha11-cand-lean-prompt/target/release/bake`, 73280856 bytes). 2 tasks, 1 models, and 5 trials of each by each arm: 20 samples, of which 20 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 2 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 39 | 48 | +23.1% | +11.4% to +35.0% |
| Total tokens | 133,777 | 160,310 | +19.8% | +8.4% to +31.2% |
| Uncached input tokens | 104,262 | 124,505 | +19.4% | +10.5% to +28.0% |
| Output tokens | 6,438 | 6,997 | +8.7% | -7.1% to +31.8% |
| Tool calls by the model | 82 | 89 | +8.5% | -18.5% to +44.8% |
| Tool calls inside codemode | 0 | 11 | n/a | n/a |
| Failed tool calls | 5 | 5 | +0.0% | n/a |
| Run time, seconds | 119 | 172 | +44.4% | +20.1% to +75.9% |

With every task weighing the same: model requests +22.4% (+12.1% to +34.5%), total tokens +19.8% (+9.5% to +32.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,038 | 19,219 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,718 | 12,843 |

## Samples that did not pass

None.

## Prompts

- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
