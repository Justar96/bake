# Evaluation: nested context files

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5b`, 72043920 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5b`, 72043920 bytes). 1 tasks, 3 models, and 10 trials of each by each arm: 60 samples, of which 60 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

Settings of the samples, besides the provider account: base `{}`, candidate `{"nestedContextFiles":true}`.

**Both arms are the same executable; they differ in their settings.**

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 87 | 77 | -11.5% | -20.2% to -2.5% |
| Total tokens | 955,830 | 1,137,502 | +19.0% | -9.4% to +59.6% |
| Uncached input tokens | 269,477 | 328,363 | +21.9% | +1.4% to +48.4% |
| Output tokens | 59,799 | 60,280 | +0.8% | -5.7% to +7.0% |
| Tool calls by the model | 271 | 245 | -9.6% | -41.0% to +31.2% |
| Tool calls inside codemode | 78 | 127 | +62.8% | n/a |
| Failed tool calls | 0 | 1 | n/a | n/a |
| Run time, seconds | 404 | 390 | -3.5% | -11.3% to +5.4% |

With every task weighing the same: model requests -11.5% (-20.2% to -2.5%), total tokens +19.0% (-9.4% to +59.6%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 55 | 54 | -1.8% | -10.7% to +7.5% |
| Total tokens | 287,765 | 282,918 | -1.7% | -13.3% to +8.3% |
| Uncached input tokens | 92,373 | 101,322 | +9.7% | -7.4% to +31.1% |
| Output tokens | 19,136 | 18,524 | -3.2% | -6.7% to +0.8% |
| Tool calls by the model | 45 | 44 | -2.2% | -13.0% to +9.3% |
| Tool calls inside codemode | 248 | 238 | -4.0% | -9.3% to +2.4% |
| Failed tool calls | 3 | 0 | -100.0% | n/a |
| Run time, seconds | 486 | 491 | +1.1% | -5.2% to +7.8% |

With every task weighing the same: model requests -1.8% (-10.7% to +7.5%), total tokens -1.7% (-13.3% to +8.3%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 51 | 48 | -5.9% | -15.1% to +2.3% |
| Total tokens | 1,497,103 | 1,146,071 | -23.4% | -35.2% to -12.2% |
| Uncached input tokens | 102 | 96 | -5.9% | -15.1% to +2.3% |
| Output tokens | 45,288 | 40,542 | -10.5% | -22.6% to -0.1% |
| Tool calls by the model | 101 | 92 | -8.9% | -34.5% to +17.6% |
| Tool calls inside codemode | 7 | 29 | +314.3% | n/a |
| Failed tool calls | 7 | 0 | -100.0% | n/a |
| Run time, seconds | 392 | 355 | -9.4% | -18.5% to -0.8% |

With every task weighing the same: model requests -5.9% (-15.1% to +2.3%), total tokens -23.4% (-35.2% to -12.2%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 10/10 | 10/10 | 95,583 | 113,750 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 28,777 | 28,292 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 10/10 | 10/10 | 149,710 | 114,607 |

## Samples that did not pass

None.

## Prompts

- `nested-instructions`: Add `GET /products/{id}/stock`, answering `{"id": <id>, "in_stock": <units>}` with the number of units of the product we can sell right now, and a 404 for a product that does not exist. Also include the same `in_stock` number in what `GET /products/{id}` returns. Add tests for both.
