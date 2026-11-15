# Evaluation: handover probe

Run on 2026-11-14 on linux/x86_64. Base: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m3`, 71565800 bytes). Candidate: `bake 0.4.8` (`/home/jtar-wsl/bake/.artifacts/eval/bake-m3`, 71565800 bytes). 2 tasks, 3 models, and 3 trials of each by each arm: 36 samples, of which 36 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

Settings of the samples, besides the provider account: base `{}`, candidate `{"bash":{"foregroundSeconds":20}}`.

**Both arms are the same executable; they differ in their settings.**

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 6/6, candidate 6/6. 6 of 6 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 18 | 21 | +16.7% | +4.5% to +35.7% |
| Total tokens | 49,615 | 67,330 | +35.7% | +10.3% to +84.7% |
| Uncached input tokens | 43,666 | 52,446 | +20.1% | +6.5% to +41.4% |
| Output tokens | 1,846 | 6,688 | +262.3% | +80.6% to +565.2% |
| Tool calls by the model | 16 | 21 | +31.2% | +12.5% to +66.7% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 0 | 0 | n/a | n/a |
| Run time, seconds | 304 | 308 | +1.0% | -0.6% to +2.6% |

With every task weighing the same: model requests +22.5% (+22.5% to +22.5%), total tokens +45.3% (+36.3% to +59.6%).

### cliproxyapi/gpt-6.1-sol:high

Passed: base 6/6, candidate 6/6. 6 of 6 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 23 | 24 | +4.3% | +0.0% to +14.3% |
| Total tokens | 47,989 | 49,920 | +4.0% | -1.6% to +15.4% |
| Uncached input tokens | 10,845 | 22,526 | +107.7% | +37.2% to +260.7% |
| Output tokens | 920 | 1,026 | +11.5% | -6.6% to +44.1% |
| Tool calls by the model | 17 | 19 | +11.8% | +0.0% to +26.7% |
| Tool calls inside codemode | 6 | 4 | -33.3% | n/a |
| Failed tool calls | 5 | 6 | +20.0% | n/a |
| Run time, seconds | 347 | 357 | +2.8% | -0.6% to +6.9% |

With every task weighing the same: model requests +4.4% (+0.0% to +15.5%), total tokens +4.4% (-1.1% to +16.2%).

### cliproxyapi/claude-opus-5-5:high

Passed: base 6/6, candidate 6/6. 6 of 6 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 18 | 25 | +38.9% | +27.3% to +62.5% |
| Total tokens | 76,827 | 109,733 | +42.8% | +29.9% to +68.4% |
| Uncached input tokens | 36 | 52 | +44.4% | +27.3% to +83.3% |
| Output tokens | 1,549 | 2,685 | +73.3% | +37.6% to +133.0% |
| Tool calls by the model | 12 | 18 | +50.0% | +37.5% to +75.0% |
| Tool calls inside codemode | 0 | 3 | n/a | n/a |
| Failed tool calls | 0 | 1 | n/a | n/a |
| Run time, seconds | 308 | 311 | +1.2% | -0.5% to +3.1% |

With every task weighing the same: model requests +44.3% (+36.9% to +58.1%), total tokens +49.6% (+41.1% to +64.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| slow-build | cliproxyapi/gemini-4-hydrogen-xhigh:high | 3/3 | 3/3 | 11,244 | 11,701 |
| slow-build | cliproxyapi/gpt-6.1-sol:high | 3/3 | 3/3 | 8,686 | 8,586 |
| slow-build | cliproxyapi/claude-opus-5-5:high | 3/3 | 3/3 | 17,508 | 22,413 |
| slow-build-plain | cliproxyapi/gemini-4-hydrogen-xhigh:high | 3/3 | 3/3 | 5,294 | 10,742 |
| slow-build-plain | cliproxyapi/gpt-6.1-sol:high | 3/3 | 3/3 | 7,311 | 8,054 |
| slow-build-plain | cliproxyapi/claude-opus-5-5:high | 3/3 | 3/3 | 8,101 | 14,165 |

## Samples that did not pass

None.

## Prompts

- `slow-build`: Build the release bundle and tell me the build id.
- `slow-build-plain`: Run `sh build.sh` with one plain bash call, giving it no timeout and not running it in the background, and tell me the build id it prints on its last line. Do not work the id out any other way.
