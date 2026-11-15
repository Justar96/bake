# Evaluation: runtime 0.5 so far against develop

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.worktrees/base-4ed086385c/target/release/bake`, 71221360 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5c`, 71880808 bytes). 13 tasks, 3 models, and 5 trials of each by each arm: 390 samples, of which 390 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 64/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 387 | 391 | +1.0% | -4.3% to +7.2% |
| Total tokens | 5,772,885 | 6,047,711 | +4.8% | -6.4% to +18.1% |
| Uncached input tokens | 1,589,561 | 1,583,993 | -0.4% | -8.0% to +8.3% |
| Output tokens | 330,114 | 337,540 | +2.2% | -4.6% to +10.0% |
| Tool calls by the model | 999 | 993 | -0.6% | -14.4% to +15.4% |
| Tool calls inside codemode | 419 | 466 | +11.2% | -29.7% to +112.7% |
| Failed tool calls | 37 | 37 | +0.0% | -22.7% to +27.3% |
| Run time, seconds | 1,986 | 2,017 | +1.6% | -5.1% to +10.5% |

With every task weighing the same: model requests +0.3% (-3.8% to +4.9%), total tokens -0.2% (-6.4% to +7.5%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 64/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 358 | 368 | +2.8% | -2.3% to +8.1% |
| Total tokens | 2,799,423 | 2,847,280 | +1.7% | -9.8% to +18.8% |
| Uncached input tokens | 667,517 | 577,000 | -13.6% | -27.1% to +2.3% |
| Output tokens | 115,906 | 126,664 | +9.3% | -1.2% to +25.5% |
| Tool calls by the model | 292 | 303 | +3.8% | -2.3% to +10.3% |
| Tool calls inside codemode | 1,249 | 1,252 | +0.2% | -5.6% to +6.5% |
| Failed tool calls | 38 | 37 | -2.6% | -25.0% to +28.6% |
| Run time, seconds | 4,012 | 3,455 | -13.9% | -33.8% to +5.8% |

With every task weighing the same: model requests +1.9% (-1.9% to +6.2%), total tokens +1.3% (-6.3% to +9.2%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 324 | 324 | +0.0% | -4.7% to +4.9% |
| Total tokens | 4,887,744 | 5,077,790 | +3.9% | -8.5% to +16.4% |
| Uncached input tokens | 644 | 648 | +0.6% | -4.0% to +5.5% |
| Output tokens | 232,831 | 241,094 | +3.5% | -4.8% to +10.5% |
| Tool calls by the model | 380 | 369 | -2.9% | -11.5% to +5.8% |
| Tool calls inside codemode | 492 | 555 | +12.8% | -15.4% to +66.4% |
| Failed tool calls | 15 | 11 | -26.7% | -63.6% to +27.3% |
| Run time, seconds | 2,643 | 2,802 | +6.0% | -3.9% to +17.8% |

With every task weighing the same: model requests -1.3% (-4.7% to +2.2%), total tokens -2.8% (-8.1% to +2.7%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,805 | 16,002 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 12,923 | 12,986 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,298 | 19,967 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,573 | 15,562 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,168 | 13,030 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,999 | 20,258 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,640 | 14,958 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 14,235 | 13,176 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,930 | 21,933 |
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 77,925 | 82,592 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 56,573 | 57,656 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 48,178 | 56,585 |
| hidden-cause-bug | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 113,501 | 126,838 |
| hidden-cause-bug | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 52,733 | 52,360 |
| hidden-cause-bug | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 68,109 | 60,551 |
| large-output | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 217,656 | 223,078 |
| large-output | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 25,327 | 36,075 |
| large-output | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 33,709 | 34,326 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 14,648 | 15,129 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 7,553 | 7,193 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,340 | 16,801 |
| multi-module-refactor | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 330,467 | 352,297 |
| multi-module-refactor | cliproxyapi/gpt-6.1-sol:high | 4/5 | 5/5 | 203,270 | 202,605 |
| multi-module-refactor | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 384,264 | 441,409 |
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 91,275 | 88,161 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 24,588 | 28,232 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 130,185 | 140,526 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 21,018 | 21,379 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 17,881 | 15,010 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 28,029 | 24,535 |
| skill-crowd | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 82,241 | 63,169 |
| skill-crowd | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 53,319 | 48,974 |
| skill-crowd | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 76,977 | 54,477 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 4/5 | 5/5 | 145,916 | 180,471 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 67,559 | 71,432 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 110,055 | 107,999 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,913 | 9,908 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,756 | 10,728 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,475 | 16,192 |

## Samples that did not pass

- `multi-module-refactor`, cliproxyapi/gpt-6.1-sol:high, base, trial 2: `budget_exhausted`. Run deadline reached
- `skill-procedure`, cliproxyapi/gemini-4-hydrogen-xhigh:high, base, trial 4: `error`. The service is currently unavailable.

## Prompts

- `add-flag`: Add a `--words` option to linecount.py. With `--words`, each line of output is `<lines> <words> <path>`, where words are separated by whitespace, and the total line is `<lines> <words> total`. Without the option the output must stay exactly as it is now. `--words` may come before or after the file names. Add unit tests for the option to tests/test_linecount.py.
- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
- `fix-test`: The unit tests in tests/ fail. Fix the bugs in the inventory package so that they all pass. Do not change the tests.
- `hanging-command`: The tests of this project don't pass in CI. Run the test suite as the README says, find what is wrong in the code, and fix it. Do not change the tests or the job files.
- `hidden-cause-bug`: Some orders come out at a different total than the old billing system charged for them. tests/test_replay.py replays yesterday's orders against what was billed, and it fails. Find the causes and fix them; do not change the tests, their data, or the docs.
- `large-output`: `python3 build.py` fails. Make the build succeed without changing build.py or anything in modules/.
- `log-count`: app.log is a service log; README.md describes its format. How many ERROR lines have code=E417, and what is the timestamp of the first of them? Answer with the count and the timestamp. Do not change any file.
- `multi-module-refactor`: reportgen keeps its readers, aggregators, formatters, and filters in module-level dictionaries in reportgen/registry.py, so two reports in one process cannot use different sets. Replace them with a `Registry` class exported from `reportgen`, with `add_reader(suffix, fn)`, `add_aggregator(name, fn)`, `add_formatter(name, fn)`, and `add_filter(name, fn)` methods, and a `reportgen.default_registry()` that returns a new registry holding every built-in one; nothing may be registered at import time any more. Keep every public function, adding the registry as its first argument to each one that needs it, and give plugins a `register(registry)` function instead of decorators. The command line must print exactly what it prints now; update the tests and the README.
- `nested-instructions`: Add `GET /products/{id}/stock`, answering `{"id": <id>, "in_stock": <units>}` with the number of units of the product we can sell right now, and a 404 for a product that does not exist. Also include the same `in_stock` number in what `GET /products/{id}` returns. Add tests for both.
- `rename-api`: Rename the function `normalize_ws` to `collapse_whitespace` everywhere in this project: the code, the tests, the README, and the filter name the command line accepts. Keep the behaviour the same and leave no alias or mention of the old name behind. Run the tests when you are done.
- `skill-crowd`: Product wants to show when each user last signed in, so we need a nullable `last_login_at` timestamp column on the `users` table. Can you write the migration for it?
- `skill-procedure`: We're shipping 2.3.0 of tidy today. Please prepare the release.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
