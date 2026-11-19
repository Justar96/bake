# Evaluation: probe nested-instructions gpt

Run on 2026-11-19 on linux/x86_64. Base: `bake 0.4.9` (`/home/jtar-wsl/bake/target/release/bake`, 72504000 bytes). Candidate: `bake 0.4.9` (`/home/jtar-wsl/bake/.artifacts/eval/bake-flow-robustness`, 73271144 bytes). 1 tasks, 1 models, and 10 trials of each by each arm: 20 samples, of which 20 printed a result. Each run had `--max-model-calls 60` and `--run-timeout-ms 900000`, and 6 samples ran at once.

## Method

`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.

## Results

### cliproxyapi/gpt-6.1-sol:high

Passed: base 10/10, candidate 10/10. 10 of 10 pairs measured.

| Measure | Base | Candidate | Change | 95% interval |
|---|---:|---:|---:|---|
| Model requests | 54 | 51 | -5.6% | -13.8% to +3.9% |
| Total tokens | 359,616 | 269,311 | -25.1% | -50.2% to +4.0% |
| Uncached input tokens | 110,847 | 104,829 | -5.4% | -30.0% to +38.6% |
| Output tokens | 18,881 | 18,690 | -1.0% | -3.9% to +1.5% |
| Tool calls by the model | 44 | 41 | -6.8% | -16.7% to +4.9% |
| Tool calls inside codemode | 242 | 247 | +2.1% | -2.4% to +6.8% |
| Failed tool calls | 3 | 1 | -66.7% | n/a |
| Run time, seconds | 487 | 505 | +3.8% | -3.6% to +16.1% |

With every task weighing the same: model requests -5.6% (-13.8% to +3.9%), total tokens -25.1% (-50.2% to +4.0%).

## Tasks

| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |
|---|---|---:|---:|---:|---:|
| nested-instructions | cliproxyapi/gpt-6.1-sol:high | 10/10 | 10/10 | 35,962 | 26,931 |

## Samples that did not pass

None.

## Prompts

- `nested-instructions`: Add `GET /products/{id}/stock`, answering `{"id": <id>, "in_stock": <units>}` with the number of units of the product we can sell right now, and a 404 for a product that does not exist. Also include the same `in_stock` number in what `GET /products/{id}` returns. Add tests for both.
