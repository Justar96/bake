# Evaluation: skills listing

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m4`, 71521840 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5a`, 71661272 bytes). 2 tasks, 3 models, and 10 trials of each by each arm: 120 samples, of which 120 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 139 | 138 | -0.7% | -7.6% to +6.9% |
| Total tokens | 2,451,625 | 2,197,123 | -10.4% | -17.5% to -1.4% |
| Uncached input tokens | 601,665 | 580,605 | -3.5% | -7.7% to +1.5% |
| Output tokens | 89,008 | 93,280 | +4.8% | -1.3% to +11.5% |
| Tool calls by the model | 374 | 381 | +1.9% | -7.8% to +10.4% |
| Tool calls inside codemode | 0 | 19 | n/a | n/a |
| Failed tool calls | 4 | 1 | -75.0% | n/a |
| Run time, seconds | 545 | 541 | -0.8% | -6.7% to +5.7% |

With every task weighing the same: model requests +1.1% (-4.2% to +6.9%), total tokens -12.0% (-17.9% to -5.0%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 101 | 104 | +3.0% | -2.9% to +8.3% |
| Total tokens | 1,263,268 | 1,129,644 | -10.6% | -20.6% to -0.1% |
| Uncached input tokens | 341,592 | 360,738 | +5.6% | -17.2% to +29.7% |
| Output tokens | 36,172 | 38,154 | +5.5% | -4.1% to +20.6% |
| Tool calls by the model | 81 | 84 | +3.7% | -3.6% to +10.5% |
| Tool calls inside codemode | 334 | 344 | +3.0% | +0.3% to +5.9% |
| Failed tool calls | 0 | 1 | n/a | n/a |
| Run time, seconds | 958 | 1,028 | +7.3% | -1.5% to +19.3% |

With every task weighing the same: model requests +3.0% (-2.8% to +8.3%), total tokens -13.8% (-19.6% to -8.5%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 98 | 98 | +0.0% | -6.7% to +7.1% |
| Total tokens | 1,893,444 | 1,628,041 | -14.0% | -25.6% to -1.0% |
| Uncached input tokens | 196 | 196 | +0.0% | -6.7% to +7.1% |
| Output tokens | 48,635 | 47,918 | -1.5% | -7.7% to +5.1% |
| Tool calls by the model | 102 | 100 | -2.0% | -9.2% to +5.3% |
| Tool calls inside codemode | 103 | 78 | -24.3% | -68.4% to +61.7% |
| Failed tool calls | 0 | 1 | n/a | n/a |
| Run time, seconds | 489 | 468 | -4.1% | -10.2% to +1.7% |

With every task weighing the same: model requests -0.3% (-6.5% to +6.0%), total tokens -18.3% (-26.3% to -9.7%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| skill-crowd | cliproxyapi/gemini-4-hydrogen-xhigh:high | 10/10 | 10/10 | 78,216 | 65,599 |
| skill-crowd | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 58,587 | 40,962 |
| skill-crowd | cliproxyapi/claude-opus-5-5:high | 10/10 | 10/10 | 81,720 | 53,860 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 10/10 | 10/10 | 166,946 | 154,113 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 67,740 | 72,003 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 10/10 | 10/10 | 107,624 | 108,944 |

## Samples that did not pass

None.

## Prompts

- `skill-crowd`: Product wants to show when each user last signed in, so we need a nullable `last_login_at` timestamp column on the `users` table. Can you write the migration for it?
- `skill-procedure`: We're shipping 2.3.0 of tidy today. Please prepare the release.
