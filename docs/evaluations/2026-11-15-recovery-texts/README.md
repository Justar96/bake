# Evaluation: recovery texts

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5b`, 72043920 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m5c`, 71880808 bytes). 13 tasks, 3 models, and 5 trials of each by each arm: 390 samples, of which 390 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 397 | 395 | -0.5% | -5.2% to +4.3% |
| Total tokens | 6,106,714 | 5,988,945 | -1.9% | -11.5% to +9.8% |
| Uncached input tokens | 1,584,566 | 1,583,298 | -0.1% | -6.5% to +7.2% |
| Output tokens | 360,096 | 350,542 | -2.7% | -7.0% to +3.1% |
| Tool calls by the model | 1,050 | 931 | -11.3% | -22.0% to -0.3% |
| Tool calls inside codemode | 408 | 537 | +31.6% | -9.7% to +121.7% |
| Failed tool calls | 40 | 35 | -12.5% | -32.5% to +11.1% |
| Run time, seconds | 2,079 | 1,990 | -4.2% | -9.0% to +1.5% |

With every task weighing the same: model requests +0.2% (-4.1% to +4.2%), total tokens +0.5% (-6.1% to +7.3%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 368 | 370 | +0.5% | -3.2% to +4.3% |
| Total tokens | 2,938,469 | 3,178,556 | +8.2% | -0.7% to +15.9% |
| Uncached input tokens | 628,582 | 631,816 | +0.5% | -11.7% to +13.6% |
| Output tokens | 130,687 | 129,204 | -1.1% | -6.4% to +3.0% |
| Tool calls by the model | 301 | 307 | +2.0% | -2.8% to +7.3% |
| Tool calls inside codemode | 1,231 | 1,251 | +1.6% | -3.4% to +6.5% |
| Failed tool calls | 40 | 39 | -2.5% | -25.6% to +29.3% |
| Run time, seconds | 3,570 | 3,693 | +3.4% | -4.7% to +11.3% |

With every task weighing the same: model requests +1.7% (-1.0% to +4.3%), total tokens +3.4% (-0.2% to +7.0%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 65/65, candidate 65/65. 65 of 65 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 316 | 335 | +6.0% | +1.6% to +10.4% |
| Total tokens | 4,522,389 | 5,198,015 | +14.9% | +2.1% to +25.6% |
| Uncached input tokens | 632 | 668 | +5.7% | +1.4% to +9.9% |
| Output tokens | 219,552 | 230,451 | +5.0% | -8.1% to +19.5% |
| Tool calls by the model | 384 | 403 | +4.9% | -6.4% to +16.4% |
| Tool calls inside codemode | 460 | 376 | -18.3% | -52.9% to +21.6% |
| Failed tool calls | 7 | 22 | +214.3% | +90.0% to +633.3% |
| Run time, seconds | 2,573 | 2,725 | +5.9% | -5.5% to +17.8% |

With every task weighing the same: model requests +4.2% (+1.0% to +7.6%), total tokens +4.9% (+0.0% to +10.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 21,268 | 17,479 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,045 | 13,057 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,759 | 19,835 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,113 | 15,978 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 12,997 | 14,133 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 19,836 | 20,342 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,511 | 15,617 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 12,210 | 13,011 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 22,207 | 21,906 |
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 84,447 | 88,134 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 56,341 | 46,926 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 54,650 | 57,981 |
| hidden-cause-bug | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 121,587 | 120,143 |
| hidden-cause-bug | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 50,948 | 54,089 |
| hidden-cause-bug | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 79,263 | 69,961 |
| large-output | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 171,345 | 197,480 |
| large-output | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 22,756 | 24,105 |
| large-output | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 37,940 | 37,322 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,537 | 19,235 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 6,492 | 7,326 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,574 | 16,974 |
| multi-module-refactor | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 410,483 | 360,492 |
| multi-module-refactor | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 252,016 | 295,152 |
| multi-module-refactor | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 326,079 | 433,152 |
| nested-instructions | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 104,820 | 103,394 |
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 26,411 | 27,517 |
| nested-instructions | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 108,738 | 126,391 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,256 | 21,145 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 15,256 | 14,345 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 25,373 | 25,100 |
| skill-crowd | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 60,289 | 64,298 |
| skill-crowd | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 41,255 | 40,536 |
| skill-crowd | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 58,369 | 53,300 |
| skill-procedure | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 170,085 | 164,777 |
| skill-procedure | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 67,236 | 74,749 |
| skill-procedure | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 120,370 | 138,672 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,602 | 9,616 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,732 | 10,765 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 16,320 | 18,667 |

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
