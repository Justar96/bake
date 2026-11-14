# Evaluation: foreground budget probe

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m3`, 71565800 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m3`, 71565800 bytes). 1 tasks, 3 models, and 5 trials of each by each arm: 30 samples, of which 30 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

Settings of the samples, besides the provider account: base `{}`, candidate `{"bash":{"foregroundSeconds":20}}`.

**Both arms are the same executable; they differ in their settings.**

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 5/5, candidate 5/5. 5 of 5 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 39 | 41 | +5.1% | -15.6% to +32.4% |
| Total tokens | 429,851 | 496,927 | +15.6% | -25.2% to +82.9% |
| Uncached input tokens | 128,285 | 149,485 | +16.5% | -13.0% to +65.5% |
| Output tokens | 34,742 | 36,401 | +4.8% | -26.7% to +44.7% |
| Tool calls by the model | 99 | 116 | +17.2% | -37.7% to +139.7% |
| Tool calls inside codemode | 34 | 18 | -47.1% | n/a |
| Failed tool calls | 8 | 7 | -12.5% | -58.3% to +83.3% |
| Run time, seconds | 313 | 257 | -18.1% | -43.4% to +23.2% |

With every task weighing the same: model requests +5.1% (-15.6% to +32.4%), total tokens +15.6% (-25.2% to +82.9%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 5/5, candidate 5/5. 5 of 5 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 42 | 42 | +0.0% | -14.9% to +24.3% |
| Total tokens | 304,313 | 320,548 | +5.3% | -15.1% to +38.6% |
| Uncached input tokens | 64,255 | 67,767 | +5.5% | -16.4% to +31.6% |
| Output tokens | 7,226 | 7,533 | +4.2% | -12.3% to +30.2% |
| Tool calls by the model | 36 | 36 | +0.0% | -17.5% to +28.1% |
| Tool calls inside codemode | 112 | 117 | +4.5% | -0.9% to +9.3% |
| Failed tool calls | 4 | 5 | +25.0% | n/a |
| Run time, seconds | 299 | 275 | -8.0% | -17.6% to +1.4% |

With every task weighing the same: model requests +0.0% (-14.9% to +24.3%), total tokens +5.3% (-15.1% to +38.6%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 5/5, candidate 5/5. 5 of 5 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 28 | 34 | +21.4% | +6.9% to +40.7% |
| Total tokens | 240,567 | 374,362 | +55.6% | +34.5% to +79.3% |
| Uncached input tokens | 56 | 66 | +17.9% | +6.9% to +29.6% |
| Output tokens | 11,171 | 14,226 | +27.3% | +11.8% to +46.1% |
| Tool calls by the model | 33 | 41 | +24.2% | +17.1% to +32.3% |
| Tool calls inside codemode | 51 | 68 | +33.3% | n/a |
| Failed tool calls | 0 | 8 | n/a | n/a |
| Run time, seconds | 646 | 559 | -13.5% | -25.6% to -0.2% |

With every task weighing the same: model requests +21.4% (+6.9% to +40.7%), total tokens +55.6% (+34.5% to +79.3%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| hanging-command | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 85,970 | 99,385 |
| hanging-command | cliproxyapi/gpt-6.1-sol:high | 5/5 | 5/5 | 60,863 | 64,110 |
| hanging-command | cliproxyapi/claude-opus-5-5:high | 5/5 | 5/5 | 48,113 | 74,872 |

## Samples that did not pass

None.

## Prompts

- `hanging-command`: The tests of this project don't pass in CI. Run the test suite as the README says, find what is wrong in the code, and fix it. Do not change the tests or the job files.
