# Evaluation: tool effects

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.worktrees/base-4ed086385c/target/release/bake`, 71221360 bytes). Candidate: `bake 0.4.8` (`/tmp/bake-m2-candidate`, 71403256 bytes). 6 tasks, 3 models, and 5 trials of each by each arm: 180 samples, of which 180 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 112 | 113 | +0.9% | -4.6% to +7.2% |
| Total tokens | 470,498 | 473,890 | +0.7% | -7.3% to +9.5% |
| Uncached input tokens | 318,723 | 319,582 | +0.3% | -5.3% to +6.1% |
| Output tokens | 42,981 | 45,788 | +6.5% | -0.6% to +13.3% |
| Tool calls by the model | 265 | 260 | -1.9% | -8.7% to +4.5% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 13 | 11 | -15.4% | -37.5% to +0.0% |
| Run time, seconds | 384 | 419 | +9.1% | -0.5% to +19.3% |

With every task weighing the same: model requests +0.1% (-5.2% to +5.9%), total tokens +0.3% (-7.6% to +7.9%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 127 | 125 | -1.6% | -6.3% to +3.3% |
| Total tokens | 366,429 | 356,526 | -2.7% | -7.9% to +2.8% |
| Uncached input tokens | 139,858 | 152,200 | +8.8% | -8.3% to +29.1% |
| Output tokens | 20,491 | 20,390 | -0.5% | -7.7% to +8.4% |
| Tool calls by the model | 97 | 95 | -2.1% | -8.2% to +4.3% |
| Tool calls inside codemode | 206 | 207 | +0.5% | -3.9% to +4.1% |
| Failed tool calls | 13 | 15 | +15.4% | -11.8% to +60.0% |
| Run time, seconds | 672 | 679 | +1.0% | -6.6% to +9.7% |

With every task weighing the same: model requests -1.4% (-6.5% to +3.6%), total tokens -2.9% (-8.5% to +2.5%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 112 | 114 | +1.8% | -3.5% to +7.3% |
| Total tokens | 628,220 | 635,554 | +1.2% | -4.7% to +8.0% |
| Uncached input tokens | 224 | 228 | +1.8% | -3.5% to +7.3% |
| Output tokens | 35,040 | 35,770 | +2.1% | -5.0% to +10.4% |
| Tool calls by the model | 110 | 114 | +3.6% | -7.8% to +16.7% |
| Tool calls inside codemode | 36 | 30 | -16.7% | -63.2% to +54.8% |
| Failed tool calls | 3 | 5 | +66.7% | n/a |
| Run time, seconds | 436 | 430 | -1.3% | -7.6% to +6.0% |

With every task weighing the same: model requests +2.0% (-2.5% to +6.6%), total tokens +1.3% (-4.1% to +6.8%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,581 | 17,096 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,009 | 12,952 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,130 | 19,704 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,122 | 16,782 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,175 | 13,346 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,523 | 21,255 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,197 | 15,301 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,677 | 12,488 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,926 | 21,427 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 17,936 | 16,489 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 7,383 | 6,961 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,831 | 18,257 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 18,719 | 19,302 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 15,369 | 15,078 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 23,871 | 26,341 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,545 | 9,809 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,673 | 10,480 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 22,364 | 20,126 |

## Samples that did not pass

None.

## Prompts

- `add-flag`: Add a `--words` option to linecount.py. With `--words`, each line of output is `<lines> <words> <path>`, where words are separated by whitespace, and the total line is `<lines> <words> total`. Without the option the output must stay exactly as it is now. `--words` may come before or after the file names. Add unit tests for the option to tests/test_linecount.py.
- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
- `fix-test`: The unit tests in tests/ fail. Fix the bugs in the inventory package so that they all pass. Do not change the tests.
- `log-count`: app.log is a service log; README.md describes its format. How many ERROR lines have code=E417, and what is the timestamp of the first of them? Answer with the count and the timestamp. Do not change any file.
- `rename-api`: Rename the function `normalize_ws` to `collapse_whitespace` everywhere in this project: the code, the tests, the README, and the filter name the command line accepts. Keep the behaviour the same and leave no alias or mention of the old name behind. Run the tests when you are done.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
