# Evaluations

A change to what a model reads or to how the agent loop runs is measured before it lands: prompts, tools, results and errors, context, compaction, caching, the loop, and model adapters. `bake-eval` runs two built `bake` executables, the base and the candidate, on the same tasks, models, and trial numbers, and reports the difference pair by pair. Each record here is one such run.

## Running one

Build the base from a clean worktree of the base `develop` commit and the candidate from the change, both in release mode, then run from the repository root:

```sh
cargo build --release --locked -p bake-eval
target/release/bake-eval check-tasks
target/release/bake-eval run \
  --base <base worktree>/target/release/bake \
  --candidate target/release/bake \
  --models gemini-4-hydrogen-xhigh:high,gpt-6.1-sol:high,claude-opus-5-5:high \
  --trials 5 --jobs 6 \
  --out docs/evaluations/<date>-<name>
```

- `check-tasks` reaches no model. It proves that every task's check fails on the untouched workspace and passes on the task's solution.
- `run` needs live models. It copies the provider account, and nothing else, from the Bake home (`BAKE_HOME`, else `~/.bake`; `--home` names another) into each sample's own home and removes it when the sample ends. Models are named as `--model` takes them, with `:level` for the thinking level; one without a provider belongs to `--provider`, `cliproxyapi` unless given.
- It writes `run.json` (the parameters), `samples.jsonl` (one record a sample, appended as samples finish), and `README.md` (the report) into `--out`, and refuses a directory that already holds a run. `bake-eval report --out <dir>` writes the report again from the other two.
- `--base-settings <file>` and `--candidate-settings <file>` give an arm's samples `settings.json` members, so one executable can be measured with a setting off and on.
- `--only <task,...>` runs some tasks, `--keep-work` with `--work <dir>` keeps each sample's workspace, session, and event stream for reading, and `bake-eval help` lists the rest.

Tasks are shell and Python scripts, so evaluations run on Linux with `python3`. Each run uses Bake's default permission, `danger-full-access`, in a temporary directory.

## Reading one

- A sample passes when its run ends in `success` and the task's check exits 0. Compare the pass counts first: a change that saves tokens and fails more tasks is a regression.
- A change is the ratio of the sums over the pairs both arms measured, with a 95% bootstrap interval over those pairs. One large task can decide that ratio, so under each model's table the report also gives requests and tokens with every task weighing the same; with the whole task set, read that line first.
- An interval is a guide, not a test. Two of the runs whose arms were identical produced one interval that narrowly excludes zero (Gemini's model requests), so a single such interval among many is weak evidence; a change counts when it is larger than the noise floor's width, appears in more than one model, or repeats.
- Samples share the host. When several samples of one task run at once, an agent that finds processes by name can see or stop another sample's: in `hanging-command`, agents were seen running `pkill -f` on the name of the CLI the task starts. Read that task's numbers with this in mind.
- Read every change against the noise floor: a record whose two arms are the same executable, run with the same tasks, models, and trial count. A change whose interval lies inside the noise floor's interval is not distinguishable from noise.
- `add-flag`, `cascade-fix`, `fix-test`, `log-count`, `rename-api`, and `trace-question` are small and take three to six model requests each; the other seven took four to fourteen in their first live runs, with many tool calls a request, and each exercises one harness behavior: a command that never returns, output larger than a tool result keeps, a nested `AGENTS.md`, a user-level skill, one skill among forty-three, a cause far from its symptom, and a refactor across many files. In those runs the three models passed every task, so the set measures cost far better than it measures correctness. None of them measures compaction, subagents, MCP servers, or the terminal.

## Tasks

The tasks are in [`crates/bake-eval/tasks`](../../crates/bake-eval/tasks/); [the crate](../../crates/bake-eval/src/task.rs) documents their layout.

| Task | What it asks for |
|---|---|
| `add-flag` | Add a command-line option with tests, keeping the old output |
| `cascade-fix` | Make a release check pass that reveals one failure at a time |
| `fix-test` | Fix two bugs that failing unit tests point at |
| `hanging-command` | Fix a job runner whose test suite never returns, because a CLI it starts in its own session hangs, then a second failure |
| `hidden-cause-bug` | Fix wrong order totals with two causes in shared helpers, one behind two indirections, in a 30-module package |
| `large-output` | Make a build pass whose problems sit mid-output in 150 to 220 KB per run, revealed one phase at a time |
| `log-count` | Count and locate lines in a 6,000-line log with look-alike lines |
| `multi-module-refactor` | Replace a global registry with an explicit object across a 36-module package with a lookup cache and lazy readers |
| `nested-instructions` | Add an endpoint and change another in a subtree whose own `AGENTS.md`, one of several, sets conventions |
| `rename-api` | Rename a function across code, tests, documentation, and a string key |
| `skill-crowd` | File a database migration with the exact files, template, and lookup table one user-level skill prescribes, found among 43 skills with long descriptions, several of them near misses |
| `skill-procedure` | Prepare a release with the exact files a user-level skill and its template prescribe, overriding an outdated in-repo guide |
| `trace-question` | Answer a question about a codebase without changing it |

Adding or changing a task changes what every later record measures: run a new noise floor with it.

## Records

| Record | What it measured |
|---|---|
| [2026-11-14, noise floor of 0.4.8](2026-11-14-noise-floor-0.4.8/README.md) | Bake 0.4.8 against itself on the six tasks, with Gemini 4 Hydrogen, GPT-6.1 Sol, and Claude Opus 5.5 at `high`, five trials. All 180 samples passed. With these tasks, models, and trial count, a change in total tokens within about ±9% (Gemini), ±7% (GPT), or ±6% (Claude), or in model requests within about ±7%, ±5%, or ±5%, is noise |
| [2026-11-15, noise floor of the twelve tasks](2026-11-15-noise-floor-twelve-tasks/README.md) | The tool-effects build against itself on all twelve tasks, same models, five trials. All 360 samples passed. With every task weighing the same, a change in total tokens within about ±11% (Gemini), ±11% (GPT), or ±8% (Claude), or in model requests within about ±7%, ±5%, or ±6%, is noise; the ratio of sums is far wider (up to ±28% in tokens), because `multi-module-refactor` alone uses a third or more of the tokens |
| [2026-11-15, noise floor of the skill tasks](2026-11-15-noise-floor-skill-tasks/README.md) | The model-profiles build against itself on `skill-procedure` and `skill-crowd`, ten trials. All 120 samples passed. With every task weighing the same, total tokens stayed within about ±10% and model requests within about ±8% |
