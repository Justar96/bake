# Evaluation: Pi #9986 tool-call batch cancellation settlement

Run on 2026-11-20 on linux/x86_64. Base: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/base-4083bfae10/target/release/bake`, 73274560 bytes). Candidate: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/cand-pi-9986/target/release/bake`, 73273256 bytes). 1 tasks, 1 models, and 5 trials of each by each arm: 10 samples, of which 10 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 2 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 5/5, candidate 5/5. 5 of 5 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 24 | 24 | +0.0% | +0.0% to +0.0% |
| Total tokens | 83,821 | 85,111 | +1.5% | -1.2% to +4.5% |
| Uncached input tokens | 61,722 | 61,784 | +0.1% | -0.8% to +1.0% |
| Output tokens | 3,970 | 4,071 | +2.5% | -3.2% to +8.6% |
| Tool calls by the model | 50 | 51 | +2.0% | +0.0% to +6.0% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 5 | 5 | +0.0% | +0.0% to +0.0% |
| Run time, seconds | 70 | 67 | -4.4% | -14.2% to +5.5% |

With every task weighing the same: model requests +0.0% (+0.0% to +0.0%), total tokens +1.5% (-1.2% to +4.5%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,764 | 17,022 |

## Samples that did not pass

None.

## Prompts

- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
