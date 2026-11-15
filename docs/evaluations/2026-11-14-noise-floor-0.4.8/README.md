# Evaluation: noise floor of 0.4.8

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/.local/share/bake-native/releases/0.4.8/bin/bake`, 71256528 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/.local/share/bake-native/releases/0.4.8/bin/bake`, 71256528 bytes). 6 tasks, 3 models, and 5 trials of each by each arm: 180 samples, of which 180 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

**Both arms are the same executable.** This run measures no change: its intervals are the noise floor of this task set, these models, and this trial count. A later change inside them cannot be told from noise.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 113 | 114 | +0.9% | -4.3% to +6.6% |
| Total tokens | 477,969 | 480,720 | +0.6% | -6.3% to +8.6% |
| Uncached input tokens | 325,479 | 313,526 | -3.7% | -9.6% to +2.2% |
| Output tokens | 45,357 | 44,486 | -1.9% | -7.7% to +5.9% |
| Tool calls by the model | 258 | 267 | +3.5% | -4.3% to +12.7% |
| Tool calls inside codemode | 10 | 0 | -100.0% | n/a |
| Failed tool calls | 13 | 11 | -15.4% | -31.2% to +0.0% |
| Run time, seconds | 487 | 459 | -5.7% | -14.9% to +3.7% |

With every task weighing the same: model requests +1.2% (-4.4% to +6.8%), total tokens +0.7% (-6.8% to +8.4%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 126 | 126 | +0.0% | -4.4% to +5.0% |
| Total tokens | 367,144 | 362,111 | -1.4% | -7.2% to +5.0% |
| Uncached input tokens | 133,623 | 139,766 | +4.6% | -12.8% to +25.1% |
| Output tokens | 21,297 | 19,209 | -9.8% | -20.3% to +0.5% |
| Tool calls by the model | 96 | 97 | +1.0% | -4.1% to +7.6% |
| Tool calls inside codemode | 214 | 205 | -4.2% | -9.9% to +0.5% |
| Failed tool calls | 12 | 11 | -8.3% | -37.5% to +20.0% |
| Run time, seconds | 713 | 695 | -2.5% | -11.6% to +6.9% |

With every task weighing the same: model requests +0.2% (-4.5% to +5.3%), total tokens -0.1% (-5.6% to +6.0%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 30/30, candidate 30/30. 30 of 30 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 110 | 111 | +0.9% | -2.9% to +4.8% |
| Total tokens | 616,718 | 623,512 | +1.1% | -3.8% to +6.0% |
| Uncached input tokens | 220 | 222 | +0.9% | -2.9% to +4.8% |
| Output tokens | 35,950 | 35,375 | -1.6% | -8.0% to +5.1% |
| Tool calls by the model | 113 | 108 | -4.4% | -15.1% to +8.3% |
| Tool calls inside codemode | 26 | 37 | +42.3% | n/a |
| Failed tool calls | 5 | 3 | -40.0% | n/a |
| Run time, seconds | 419 | 433 | +3.4% | -2.4% to +9.6% |

With every task weighing the same: model requests +0.9% (-2.4% to +4.3%), total tokens +1.0% (-2.9% to +5.3%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| add-flag | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 19,003 | 19,451 |
| add-flag | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,051 | 12,961 |
| add-flag | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,190 | 20,002 |
| cascade-fix | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,011 | 15,897 |
| cascade-fix | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,424 | 13,106 |
| cascade-fix | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 20,155 | 20,319 |
| fix-test | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 15,090 | 15,732 |
| fix-test | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 13,033 | 13,409 |
| fix-test | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 21,796 | 20,600 |
| log-count | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 16,053 | 16,130 |
| log-count | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 6,923 | 7,366 |
| log-count | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 15,914 | 15,427 |
| rename-api | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 19,068 | 18,438 |
| rename-api | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 16,703 | 14,488 |
| rename-api | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 26,500 | 26,837 |
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 10,368 | 10,496 |
| trace-question | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 10,293 | 11,092 |
| trace-question | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 18,788 | 21,518 |

## Samples that did not pass

None.

## Prompts

- `add-flag`: Add a `--words` option to linecount.py. With `--words`, each line of output is `<lines> <words> <path>`, where words are separated by whitespace, and the total line is `<lines> <words> total`. Without the option the output must stay exactly as it is now. `--words` may come before or after the file names. Add unit tests for the option to tests/test_linecount.py.
- `cascade-fix`: Run `python3 verify.py` and fix the project until it prints OK. Do not edit verify.py.
- `fix-test`: The unit tests in tests/ fail. Fix the bugs in the inventory package so that they all pass. Do not change the tests.
- `log-count`: app.log is a service log; README.md describes its format. How many ERROR lines have code=E417, and what is the timestamp of the first of them? Answer with the count and the timestamp. Do not change any file.
- `rename-api`: Rename the function `normalize_ws` to `collapse_whitespace` everywhere in this project: the code, the tests, the README, and the filter name the command line accepts. Keep the behaviour the same and leave no alias or mention of the old name behind. Run the tests when you are done.
- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
