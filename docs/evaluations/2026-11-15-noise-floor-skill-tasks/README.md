# Evaluation: noise floor of the skill tasks

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m4`, 71521840 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m4`, 71521840 bytes). 2 tasks, 3 models, and 10 trials of each by each arm: 120 samples, of which 120 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

**Both arms are the same executable.** This run measures no change: its intervals are the noise floor of this task set, these models, and this trial count. A later change inside them cannot be told from noise.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 145 | 138 | -4.8% | -8.4% to -1.3% |
| Total tokens | 2,468,973 | 2,439,807 | -1.2% | -9.4% to +10.0% |
| Uncached input tokens | 617,826 | 614,705 | -0.5% | -4.9% to +5.1% |
| Output tokens | 91,244 | 90,226 | -1.1% | -8.0% to +6.1% |
| Tool calls by the model | 383 | 366 | -4.4% | -7.1% to -1.9% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 2 | 4 | +100.0% | n/a |
| Run time, seconds | 530 | 533 | +0.7% | -10.7% to +11.6% |

With every task weighing the same: model requests -5.2% (-8.5% to -1.8%), total tokens -2.8% (-9.6% to +5.9%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 101 | 100 | -1.0% | -5.9% to +4.0% |
| Total tokens | 1,272,642 | 1,266,833 | -0.5% | -7.8% to +7.2% |
| Uncached input tokens | 373,715 | 350,381 | -6.2% | -22.2% to +12.1% |
| Output tokens | 35,695 | 35,684 | -0.0% | -2.9% to +3.0% |
| Tool calls by the model | 81 | 80 | -1.2% | -7.3% to +5.0% |
| Tool calls inside codemode | 342 | 350 | +2.3% | -2.5% to +7.8% |
| Failed tool calls | 0 | 0 | n/a | n/a |
| Run time, seconds | 960 | 943 | -1.7% | -5.6% to +2.3% |

With every task weighing the same: model requests -1.1% (-6.0% to +3.8%), total tokens -0.7% (-7.4% to +6.2%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 20/20, candidate 20/20. 20 of 20 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 97 | 95 | -2.1% | -7.2% to +4.1% |
| Total tokens | 1,850,740 | 1,825,127 | -1.4% | -8.3% to +7.0% |
| Uncached input tokens | 194 | 190 | -2.1% | -7.2% to +4.1% |
| Output tokens | 48,709 | 46,952 | -3.6% | -8.3% to +1.6% |
| Tool calls by the model | 103 | 97 | -5.8% | -14.2% to +3.2% |
| Tool calls inside codemode | 117 | 62 | -47.0% | -79.8% to +4.8% |
| Failed tool calls | 0 | 0 | n/a | n/a |
| Run time, seconds | 479 | 465 | -2.9% | -7.7% to +2.6% |

With every task weighing the same: model requests -2.4% (-7.4% to +2.8%), total tokens -1.8% (-8.5% to +5.6%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| skill-crowd | cliproxyapi/gemini-4-hydrogen-xhigh:high | 10/10 | 10/10 | 86,372 | 79,907 |
| skill-crowd | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 59,696 | 57,241 |
| skill-crowd | cliproxyapi/claude-opus-5-5:high | 10/10 | 10/10 | 80,952 | 76,930 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 10/10 | 10/10 | 160,526 | 164,074 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 67,568 | 69,442 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 10/10 | 10/10 | 104,122 | 105,582 |

## Samples that did not pass

None.

## Prompts

- `skill-crowd`: Product wants to show when each user last signed in, so we need a nullable `last_login_at` timestamp column on the `users` table. Can you write the migration for it?
- `skill-procedure`: We're shipping 2.3.0 of tidy today. Please prepare the release.
