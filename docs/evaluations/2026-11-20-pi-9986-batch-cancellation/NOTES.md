# Evaluation Notes: Pi #9986 Tool-Call Batch Cancellation Settlement

## Provenance and Build Scope

- **Base arm**: clean detached worktree at `develop` commit `4083bfae10ad40e6a8bfe12f5e23a6e6d4b9cd75` (`release: 0.4.10`).
- **Candidate arm**: clean detached worktree at `4083bfae10ad40e6a8bfe12f5e23a6e6d4b9cd75` with only the scoped production diff in `crates/bake-agent/src/agent_loop.rs` (`+13 -10`, SHA-256 `f22a71095903b421fd91388daa45c9af3c56268debf7c7d4fd2d78b8f0b44810`), porting Pi #9986 from `earendil-works/pi` `v1.6.0` (`faac585884ae2c013a6b1400f3e9e77f10b34bac`).
- Neither arm included unrelated working-tree changes. `README.md` is the unmodified `bake-eval` output from `run.json` and `samples.jsonl`.

## Interpretation

- Both arms passed **5/5** on `cascade-fix` with `cliproxyapi/gemini-4-hydrogen-xhigh:high` (5 paired trials, 10 samples). Batched tool responses occurred in every trial (16 in base, 19 in candidate).
- Model requests (24 vs 24) and failed tool calls (5 vs 5) were unchanged (`+0.0%`). Total tokens showed a small delta of `+1.5%` (`-1.2%` to `+4.5%`; mean 16,764 vs 17,022) and run time `-4.4%` (`-14.2%` to `+5.5%`); every 95% bootstrap interval contains `0.0%`.
- No regression was observed on this normal multi-tool fix-and-verify task.

## Limitations

- **No cancellation exercised**: these live runs complete normally without aborting a tool-call batch. Correctness of Pi #9986 cancellation settlement (recording aborted results with no orphan tool calls) is established by deterministic tests in `crates/bake-agent`, not by this evaluation.
- **No task-specific calibrated threshold**: inference is bounded to one task (`cascade-fix`), one model (`gemini-4-hydrogen-xhigh:high`), and 5 pairs. Multi-task suite noise floors in `docs/evaluations/README.md` do not apply to a single task, and this record makes no general model-quality claim.
