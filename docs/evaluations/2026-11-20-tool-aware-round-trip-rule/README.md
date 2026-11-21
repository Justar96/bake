# Evaluation: Tool-aware round-trip batch rule (read,grep)

Run on 2026-11-20 on linux/x86_64. Base: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/alpha8-base-4083bfae10/target/release/bake`, 73274248 bytes). Candidate: `bake 0.4.10` (`/home/jtar-wsl/bake/.worktrees/alpha8-cand-prompt-batch-rule/target/release/bake`, 73283440 bytes). 1 tasks, 1 models, and 5 trials of each by each arm: 10 samples, of which 10 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 2 samples ran at once.

Settings of the samples, besides the provider account: base `{"defaultTools":["read","grep"]}`, candidate `{"defaultTools":["read","grep"]}`.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gemini-4-hydrogen-xhigh:high

Passed: base 5/5, candidate 5/5. 5 of 5 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 20 | 20 | +0.0% | +0.0% to +0.0% |
| Total tokens | 39,977 | 38,765 | -3.0% | -4.6% to -2.0% |
| Uncached input tokens | 38,041 | 36,832 | -3.2% | -4.4% to -2.3% |
| Output tokens | 1,936 | 1,933 | -0.2% | -9.1% to +9.2% |
| Tool calls by the model | 20 | 20 | +0.0% | +0.0% to +0.0% |
| Tool calls inside codemode | 0 | 0 | n/a | n/a |
| Failed tool calls | 0 | 0 | n/a | n/a |
| Run time, seconds | 55 | 55 | +0.7% | -6.0% to +7.9% |

With every task weighing the same: model requests +0.0% (+0.0% to +0.0%), total tokens -3.0% (-4.6% to -2.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| trace-question | cliproxyapi/gemini-4-hydrogen-xhigh:high | 5/5 | 5/5 | 7,995 | 7,753 |

## Samples that did not pass

None.

## Prompts

- `trace-question`: In this codebase, which function applies the bulk (volume) discount to an order line, which file is it in, and how many units of one item must a line hold to get it? Answer in one or two sentences. Do not change any file.
