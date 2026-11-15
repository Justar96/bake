# Evaluation: commands as jobs

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m2`, 71403256 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m3`, 71565800 bytes). 12 tasks, 3 models, and 5 trials of each by each arm: 360 samples, of which 360 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 379 | 369 | -2.6% | -5.9% to +0.5% |
| Total tokens | 6,074,070 | 5,778,513 | -4.9% | -15.4% to +6.9% |
| Uncached input tokens | 1,514,166 | 1,468,949 | -3.0% | -11.3% to +6.2% |
| Output tokens | 332,462 | 332,210 | -0.1% | -5.0% to +5.0% |
| Tool calls by the model | 984 | 1,022 | +3.9% | -7.9% to +18.3% |
| Tool calls inside codemode | 399 | 325 | -18.5% | -46.9% to +29.2% |
| Failed tool calls | 43 | 39 | -9.3% | -31.0% to +17.2% |
| Run time, seconds | 2,166 | 2,430 | +12.2% | -5.5% to +45.9% |

With every task weighing the same: model requests -3.1% (-5.7% to -0.4%), total tokens -4.6% (-12.6% to +3.4%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 339 | 350 | +3.2% | -2.1% to +8.6% |
| Total tokens | 2,679,835 | 2,958,048 | +10.4% | -6.2% to +27.5% |
| Uncached input tokens | 589,366 | 658,635 | +11.8% | -8.7% to +38.1% |
| Output tokens | 119,525 | 123,029 | +2.9% | -6.5% to +12.2% |
| Tool calls by the model | 278 | 288 | +3.6% | -2.2% to +9.7% |
| Tool calls inside codemode | 1,165 | 1,162 | -0.3% | -7.7% to +6.2% |
| Failed tool calls | 40 | 32 | -20.0% | -40.4% to +5.0% |
| Run time, seconds | 3,285 | 3,385 | +3.0% | -6.2% to +12.4% |

With every task weighing the same: model requests +2.3% (-1.6% to +6.3%), total tokens +3.4% (-7.3% to +15.7%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 60/60, candidate 60/60. 60 of 60 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 299 | 300 | +0.3% | -4.6% to +5.2% |
| Total tokens | 4,545,160 | 4,317,676 | -5.0% | -14.1% to +5.4% |
| Uncached input tokens | 594 | 600 | +1.0% | -3.8% to +5.9% |
| Output tokens | 224,726 | 217,629 | -3.2% | -15.2% to +8.1% |
| Tool calls by the model | 367 | 373 | +1.6% | -9.7% to +12.5% |
| Tool calls inside codemode | 290 | 327 | +12.8% | -27.1% to +103.6% |
| Failed tool calls | 19 | 12 | -36.8% | -70.0% to +25.0% |
| Run time, seconds | 2,527 | 2,579 | +2.1% | -9.2% to +15.3% |

With every task weighing the same: model requests -0.1% (-4.5% to +4.0%), total tokens -2.3% (-8.1% to +3.8%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 19,055 | 17,931 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,006 | 13,074 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,630 | 21,899 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,716 | 16,527 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,264 | 13,986 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,015 | 21,229 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,452 | 15,139 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 12,192 | 12,431 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,292 | 18,440 |
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 85,220 | 74,106 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 59,752 | 72,068 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 53,589 | 59,237 |
| hidden-cause-bug | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 126,874 | 134,154 |
| hidden-cause-bug | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 60,503 | 61,214 |
| hidden-cause-bug | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 67,310 | 70,279 |
| large-output | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 251,451 | 210,493 |
| large-output | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 47,448 | 42,976 |
| large-output | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 37,384 | 33,799 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,596 | 13,905 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 7,355 | 8,263 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 16,614 | 18,508 |
| multi-module-refactor | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 370,016 | 366,945 |
| multi-module-refactor | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 199,611 | 250,134 |
| multi-module-refactor | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 369,044 | 363,988 |
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 96,977 | 123,858 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 26,949 | 25,677 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 134,665 | 97,475 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,992 | 18,134 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 14,068 | 14,099 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 28,051 | 25,604 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 184,848 | 152,962 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 71,200 | 67,272 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 123,068 | 113,873 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,618 | 11,549 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,618 | 10,415 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 17,371 | 19,204 |

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
