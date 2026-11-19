# Evaluation: flow robustness against develop

Run on 2026-11-19 on linux/x86_64. Base: `bake 0.4.9` (`/home/jtar-wsl/bake/target/release/bake`, 72504000 bytes). Candidate: `bake 0.4.9` (`/home/jtar-wsl/bake/.artifacts/eval/bake-flow-robustness`, 73271144 bytes). 13 tasks, 3 models, and 5 trials of each by each arm: 390 samples, of which 390 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 395 | 398 | +0.8% | -3.7% to +5.4% |
| Total tokens | 5,979,313 | 6,019,897 | +0.7% | -9.2% to +11.2% |
| Uncached input tokens | 1,586,662 | 1,597,822 | +0.7% | -5.0% to +6.9% |
| Output tokens | 340,963 | 343,035 | +0.6% | -4.7% to +5.1% |
| Tool calls by the model | 992 | 1,046 | +5.4% | -10.8% to +26.0% |
| Tool calls inside codemode | 462 | 409 | -11.5% | -52.1% to +40.2% |
| Failed tool calls | 41 | 41 | +0.0% | -20.5% to +23.7% |
| Run time, seconds | 2,466 | 2,376 | -3.6% | -10.1% to +2.8% |

With every task weighing the same: model requests +0.6% (-2.8% to +4.0%), total tokens -0.2% (-6.1% to +5.5%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 384 | 373 | -2.9% | -7.8% to +2.2% |
| Total tokens | 3,129,774 | 3,410,872 | +9.0% | -6.9% to +27.4% |
| Uncached input tokens | 696,706 | 818,924 | +17.5% | +2.7% to +33.8% |
| Output tokens | 132,908 | 131,404 | -1.1% | -7.3% to +5.4% |
| Tool calls by the model | 317 | 305 | -3.8% | -9.6% to +1.9% |
| Tool calls inside codemode | 1,276 | 1,244 | -2.5% | -6.9% to +3.3% |
| Failed tool calls | 51 | 35 | -31.4% | -63.6% to +22.0% |
| Run time, seconds | 3,540 | 3,520 | -0.6% | -6.5% to +5.3% |

With every task weighing the same: model requests -1.9% (-5.8% to +2.0%), total tokens +5.5% (-2.6% to +12.0%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 330 | 335 | +1.5% | -5.2% to +10.7% |
| Total tokens | 4,974,953 | 5,503,254 | +10.6% | -11.8% to +45.3% |
| Uncached input tokens | 660 | 670 | +1.5% | -5.2% to +10.7% |
| Output tokens | 221,904 | 226,803 | +2.2% | -8.2% to +13.0% |
| Tool calls by the model | 377 | 436 | +15.6% | -0.3% to +36.9% |
| Tool calls inside codemode | 455 | 461 | +1.3% | -28.7% to +42.9% |
| Failed tool calls | 14 | 12 | -14.3% | -52.4% to +50.0% |
| Run time, seconds | 2,591 | 2,659 | +2.6% | -8.1% to +14.0% |

With every task weighing the same: model requests +0.2% (-3.9% to +4.6%), total tokens +2.2% (-4.3% to +10.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 17,200 | 17,743 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,001 | 14,800 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,778 | 20,266 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,891 | 17,166 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,110 | 13,470 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,555 | 20,860 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,537 | 15,466 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 14,541 | 12,357 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,550 | 20,939 |
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 81,293 | 75,269 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 59,673 | 62,322 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 54,325 | 52,621 |
| hidden-cause-bug | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 120,469 | 124,701 |
| hidden-cause-bug | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 57,045 | 46,199 |
| hidden-cause-bug | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 84,296 | 65,104 |
| large-output | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 209,592 | 209,966 |
| large-output | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 37,539 | 42,331 |
| large-output | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 37,274 | 34,049 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 19,353 | 17,630 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 7,691 | 7,691 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,326 | 17,688 |
| multi-module-refactor | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 322,093 | 368,251 |
| multi-module-refactor | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 261,144 | 295,255 |
| multi-module-refactor | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 419,424 | 520,666 |
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 125,131 | 115,718 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 26,499 | 55,516 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 105,834 | 147,820 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,293 | 18,657 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 15,673 | 14,202 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 22,981 | 24,502 |
| skill-crowd | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 69,271 | 66,133 |
| skill-crowd | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 45,865 | 43,386 |
| skill-crowd | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 55,248 | 55,482 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 172,166 | 146,306 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 63,635 | 63,745 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 119,012 | 100,105 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 9,574 | 10,971 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,540 | 10,900 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,389 | 20,550 |

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
- `skill-crowd`: Product wants to show when each user last signed in, so we need a nullable `last_login_at` timestamp column on the `users` table. Can you write the migration for it?
- `skill-procedure`: We're shipping 2.3.0 of tidy today. Please prepare the release.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
