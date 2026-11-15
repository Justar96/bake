# Evaluation: noise floor of the twelve tasks

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m2`, 71403256 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m2`, 71403256 bytes). 12 tasks, 3 models, and 5 trials of each by each arm: 360 samples, of which 360 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

**Both arms are the same executable.** This run measures no change: its intervals are the noise floor of this task set, these models, and this trial count. A later change inside them cannot be told from noise.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 363 | 371 | +2.2% | -4.7% to +10.2% |
| Total tokens | 5,374,542 | 5,748,710 | +7.0% | -10.4% to +28.2% |
| Uncached input tokens | 1,438,650 | 1,466,763 | +2.0% | -6.0% to +10.4% |
| Output tokens | 354,626 | 331,095 | -6.6% | -14.8% to +4.0% |
| Tool calls by the model | 925 | 1,014 | +9.6% | -5.4% to +30.0% |
| Tool calls inside codemode | 426 | 334 | -21.6% | -51.7% to +23.5% |
| Failed tool calls | 44 | 43 | -2.3% | -24.4% to +28.6% |
| Run time, seconds | 2,780 | 2,208 | -20.6% | -48.5% to +5.5% |

With every task weighing the same: model requests +2.2% (-2.5% to +7.0%), total tokens +3.2% (-4.4% to +11.4%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 338 | 342 | +1.2% | -3.5% to +5.6% |
| Total tokens | 2,551,771 | 2,651,643 | +3.9% | -4.2% to +13.4% |
| Uncached input tokens | 638,992 | 556,926 | -12.8% | -23.7% to +0.4% |
| Output tokens | 121,931 | 125,181 | +2.7% | -5.4% to +14.6% |
| Tool calls by the model | 276 | 281 | +1.8% | -3.5% to +7.1% |
| Tool calls inside codemode | 1,209 | 1,150 | -4.9% | -11.8% to +3.1% |
| Failed tool calls | 29 | 39 | +34.5% | -3.1% to +94.7% |
| Run time, seconds | 3,289 | 3,490 | +6.1% | -2.4% to +17.8% |

With every task weighing the same: model requests +1.2% (-2.5% to +5.1%), total tokens +5.0% (-0.5% to +10.5%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 304 | 303 | -0.3% | -6.1% to +5.8% |
| Total tokens | 4,570,990 | 4,807,106 | +5.2% | -10.1% to +22.8% |
| Uncached input tokens | 606 | 606 | +0.0% | -5.8% to +6.2% |
| Output tokens | 226,834 | 223,625 | -1.4% | -9.1% to +8.0% |
| Tool calls by the model | 347 | 334 | -3.7% | -12.3% to +7.3% |
| Tool calls inside codemode | 381 | 359 | -5.8% | -40.5% to +46.8% |
| Failed tool calls | 16 | 15 | -6.2% | -50.0% to +70.0% |
| Run time, seconds | 2,577 | 2,644 | +2.6% | -7.0% to +13.6% |

With every task weighing the same: model requests -1.7% (-6.1% to +2.9%), total tokens -1.5% (-8.0% to +5.7%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,683 | 17,310 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,140 | 12,831 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,911 | 20,013 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,018 | 16,618 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,056 | 13,283 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,229 | 20,055 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,300 | 15,162 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,117 | 13,594 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,966 | 20,784 |
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 78,753 | 78,923 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 63,626 | 58,926 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 65,150 | 74,756 |
| hidden-cause-bug | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 144,715 | 111,697 |
| hidden-cause-bug | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 46,525 | 58,907 |
| hidden-cause-bug | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 75,204 | 61,435 |
| large-output | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 188,570 | 193,937 |
| large-output | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 23,421 | 29,607 |
| large-output | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 35,196 | 31,593 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 11,786 | 12,999 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 6,755 | 7,241 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,342 | 15,914 |
| multi-module-refactor | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 324,160 | 403,048 |
| multi-module-refactor | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 211,953 | 218,187 |
| multi-module-refactor | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 391,950 | 445,583 |
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 91,689 | 107,226 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 26,155 | 28,638 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 114,787 | 115,695 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,230 | 18,863 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 14,873 | 14,220 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 24,205 | 22,645 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 158,610 | 163,460 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 67,334 | 63,721 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 112,453 | 116,817 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,396 | 10,499 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,399 | 11,173 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 16,803 | 16,131 |

## Samples that did not pass

None.

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
- `skill-procedure`: We're shipping 2.3.0 of tidy today. Please prepare the release.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
