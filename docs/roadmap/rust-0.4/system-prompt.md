# System prompt design for the Rust harness

## Summary

`bake-rs` sends Pi v1.1.0's system prompt byte for byte, plus Bake's permissions section. That prompt names the harness `pi`, points the model at Pi documentation that does not ship beside the binary, and lists environment variables named `PI_*`. It also has no guidance on batching tool calls, which Bake's own evals show is the sentence that matters most for some models. This page designs Bake's own system prompt for the Rust harness from three sources: Bake's paired eval records, published research, and the prompts and guides of current coding agents and model vendors.

The design keeps Pi's prompt machinery (named sections that a session records and later patches) and replaces the text: a one-line identity, five Bake rules, the tool-contributed guidelines, the permissions section, a one-line pointer to Bake's own documentation, a run-mode line, and a small environment section last. The prompt describes the outcome and the boundaries and leaves the method to the model. A rule stays only when an eval or a cited failure shows the model behaves worse without it. One prompt serves every model family until a paired eval shows that a family needs a delta.

**Status: proposed design, 2026-10-13.** [D33](scope-00/support.md#decision-register) makes the system prompt Bake's own. Nothing on this page is implemented; the [implementation plan](#implementation-plan) lands it in reviewable pull requests, and the [evaluation plan](#evaluation-plan) decides whether it ships.

## Table of Contents

- [Goals](#goals)
- [Evidence](#evidence)
- [Principles](#principles)
- [Design](#design)
- [Evaluation plan](#evaluation-plan)
- [Implementation plan](#implementation-plan)
- [Candidates not adopted](#candidates-not-adopted)
- [Open questions](#open-questions)

## Goals

- **Clean.** Every line describes Bake as it is: its name, its documentation, its environment variables, and its permissions. No text describes a feature `bake-rs` does not have.
- **Concise.** The prompt is the smallest set of instructions that fully describes the expected behavior. Brevity is a result, not the goal: a sentence that measurably saves model round trips stays.
- **Model-driven.** The prompt states what done looks like and where the boundaries are; the model chooses how to get there. Guarantees that must hold every time are enforced by the harness (the sandbox, approvals, and permission presets of [D24](scope-00/support.md#decision-register)), not by prose.
- **Cache-stable and reconstructable.** The system prompt is byte-stable for a session, with volatile facts last. Every change reaches the model as a recorded section patch, so model input stays reconstructable from the session file.

Non-goals: tool descriptions, schemas, results, and repair text keep following Pi under D33. Skills, prompt templates, and the `--system-prompt` and `--append-system-prompt` flags remain scope 10 work; this design leaves room for them without adding them.

## Evidence

### Bake's own measurements

Paired records compare two builds on the same day, models, and scenarios, so they measure an effect rather than drift. The noise floor matters: on identical code, the [eval fidelity record](../../../evals/agent-loop/versions/unreleased/2026-10-06-eval-fidelity/REPORT.md) found paired token changes of up to about 10% for Gemini, DeepSeek Flash, and GPT, and about 2% for Claude.

| Change | Effect | Record |
|---|---|---|
| The round-trip paragraph alone: "Put independent tool calls in one response: read the files you need together, and send an edit with the command that checks it… do not re-read a file to confirm an edit that succeeded" | Gemini 3.8 Flash total tokens −43.3% [−49.7, −36.9], requests 165 → 95. Claude Opus −9.6% [−16.3, −3.2], requests 100 → 90. DeepSeek V4 Pro and GPT-6.1 within noise | [trim-and-batch, step 1](../../../evals/agent-loop/versions/v0.3.4/2026-10-03-trim-and-batch/steps/1-guidance-only/REPORT.md) |
| Shorter tool descriptions on top of that (tool JSON about 19.7k → 13.1k characters) | Total tokens −22% to −32% on all four models, requests about unchanged | [trim-and-batch, step 2](../../../evals/agent-loop/versions/v0.3.4/2026-10-03-trim-and-batch/steps/2-trim-and-todo/REPORT.md) |
| Longer tool guidance (+1,940 first-request bytes) | Gemini +8.8% [3.3, 14.7] total tokens, flagged | [tool recovery](../../../evals/agent-loop/versions/v0.3.0/2026-09-30-tool-recovery/REPORT.md) |
| `bake-rs` with Pi's prompt against the 0.3 TypeScript agent with Bake's prompt, 2026-10-13 | Claude and DeepSeek about 30% fewer tokens (smaller tool surface). Claude requests +10% to +12%. Gemini requests 77 → 166 | The native-arm record in [#204](https://github.com/Justar96/bake/pull/204) |

The transcripts behind the native-arm record show which prompt text caused which behavior:

- **Batching.** Gemini put more than one tool call in 75% of its responses under Bake's prompt and in 2% under Pi's, so each read, edit, and check became its own request. Claude still batched reads under Pi's prompt but sent the edit and its check in separate responses (Opus in 4 of 21 samples, Sonnet in 7). Both behaviors match the absence of the round-trip paragraph.
- **Repository checks.** Gemini ran `git status` after a passing test in 18 of 24 samples under Pi's prompt and 6 under Bake's, in a fixture that is not a git repository, and re-read the edited file afterwards in 13. Neither prompt says whether the workspace is a repository.
- **Dead text.** No model, in the 144 native-arm samples, read the Pi documentation paths or inspected a `PI_*` variable. Those sections cost about 1,500 of the prompt's 3,006 characters and changed nothing.
- **Unaffected.** DeepSeek and GPT split edit and check in almost every sample under either prompt; the round-trip paragraph does not move them.

### Published research

Each row was checked against the source. "Applies" rates how directly the result transfers to a frontier-model coding agent's system prompt.

| Finding | Source | Applies |
|---|---|---|
| Instruction following degrades as instructions accumulate; at 500 instructions the best model reaches 68%, and earlier instructions are followed more often than later ones | Jaroslawicz et al., [How Many Instructions Can LLMs Follow at Once?](https://arxiv.org/abs/2507.11538), 2025 | Moderate: a synthetic task, but the prompt shares context with project files and tool text |
| Real agentic system prompts average 1,723 words and 11.9 constraints; the best model of its day followed fewer than 30% of instructions perfectly, and tool and conditional constraints fared worst | Qi et al., [AgentIF](https://arxiv.org/abs/2505.16944), 2025 | Moderate: real agent prompts, older models |
| Context files do not generally raise task success but raise cost by over 20%; agents do follow the tools they name | Gloaguen et al., [Evaluating AGENTS.md](https://arxiv.org/abs/2602.11988), 2026 | Strong |
| The size and position of instructions in a configuration file had no detectable effect on compliance over 1,650 Claude Code sessions; compliance decayed within long sessions | McMillan, [Instruction Adherence in Coding Agent Configuration Files](https://arxiv.org/abs/2605.10039), 2026 | Strong for moderate sizes |
| Context files lower runtime and output tokens without changing completion | Lulla et al., [On the Impact of AGENTS.md Files](https://arxiv.org/abs/2601.20404), 2026 | Moderate |
| Two short prompt additions on top of a minimal bash-only agent raised strong models' SWE-bench Verified scores and sharply lowered a small model's | Xia et al., [Live-SWE-agent](https://arxiv.org/abs/2511.13646), 2025 | Strong: small affordances help strong models and can break weak ones |
| A 100-line, bash-only agent scores above 74% on SWE-bench Verified | [mini-SWE-agent](https://github.com/SWE-agent/mini-swe-agent) | Strong evidence that capable models need little scaffolding |
| Model choice usually matters more than the scaffold; a neutral single-tool agent matched vendor agents for some models | Merrill et al., [Terminal-Bench](https://arxiv.org/abs/2601.11868), 2026 | Moderate; observational |
| The same model's score varies by a median 13.6 points across harnesses | Guo et al., [agent harness survey](https://arxiv.org/abs/2606.20683) §7.4, 2026 | Weak for prompt text alone: harnesses differ in tools, budgets, and prompts together |
| An "edit" framing cut correct abstention on already-fixed issues; framing abstention as success restored it | Gloaguen et al., [Coding Agents Don't Know When to Act](https://arxiv.org/abs/2605.07769), 2026 | Strong: wording shifts behavior more than headline scores |
| An automated harness search found that edits to the prompt template regressed and an additive environment snapshot helped | Lee et al., [Meta-Harness](https://arxiv.org/abs/2603.28052), 2026 | Weak: one search run |
| A prompt tuned for one model performs worse on another than a prompt tuned for that model | Wang et al., [PromptBridge](https://arxiv.org/abs/2512.01420), 2025 | Moderate: supports per-family deltas when measured |
| Personas in system prompts do not improve performance | Zheng et al., [When "A Helpful Assistant" Is Not Really Helpful](https://arxiv.org/abs/2311.10054), 2024; Salomon et al., [The Librarian Who Refused to Code](https://arxiv.org/abs/2607.17420), 2026 | Moderate: factual questions, and a small code study where effects differed by model |
| Overthinking correlates with lower resolve rates on SWE-bench | Cuadron et al., [The Danger of Overthinking](https://arxiv.org/abs/2502.08235), 2025 | Moderate: correlational; supports leaving reasoning to the effort setting |
| Rule files are an effective prompt-injection vector against coding agents, up to 84% attack success | Liu et al., ["Your AI, My Shell"](https://arxiv.org/abs/2509.22040), 2025 | Strong: safety needs enforcement, not prose |
| Caching only the system prompt gives the most consistent cost and latency benefit for long agent runs | Lumer et al., [Don't Break the Cache](https://arxiv.org/abs/2601.06007), 2026 | Moderate: web-research agents |

No published study ablates a current coding agent's system prompt text alone. The rows above set direction; Bake's paired evals decide the text.

### Current practice

| Source | What it says or does |
|---|---|
| Anthropic, [Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | Write at the right altitude: "the minimal set of information that fully outlines your expected behavior", where minimal does not mean short |
| Anthropic, [prompting best practices](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices) | Recent Claude models overtrigger on emphatic text such as "CRITICAL: You MUST"; plain wording works. Explain the reason behind a rule |
| OpenAI, GPT-5-Codex prompting guide | "Less is more": over-prompting lowers quality; remove requests for upfront plans and status updates |
| OpenAI, GPT-6 model guide | The models follow instructions in `AGENTS.md` and skills closely, so state that the user's instructions take precedence |
| Google, Gemini prompting strategies | Be precise and direct, avoid persuasive language, and do not ask for reasoning in the output |
| AWS Strands, model-driven agents | Let the model plan and choose tools; describe what success looks like instead of scripting steps |
| Pi v1.2.0 | About 700 tokens plus tool schemas; one prompt for every model; removed the date in July 2026 to keep the prompt cache stable ([#6621](https://github.com/earendil-works/pi/issues/6621)) |
| Codex CLI, Claude Code, Gemini CLI, opencode | Send the working directory, platform, and whether the workspace is a git repository, and most send the date; Codex CLI and opencode deliver these as context items that later turns update instead of rebuilding the prompt. Safety is framed by reversibility: do not commit, push, or discard the user's changes unless asked |

## Principles

1. **Keep a short fixed core.** Remove any line that no eval or cited failure supports. (IFScale, AgentIF, mini-SWE-agent, Pi.)
2. **State the outcome and the boundaries, not the procedure.** (Anthropic, OpenAI, Strands, Coding Agents Don't Know When to Act.)
3. **Keep the round-trip guidance.** It is the one behavioral sentence with a large measured effect, for Gemini and Claude. (Bake records.)
4. **Tool-specific guidance belongs to the tool.** The system prompt carries only cross-tool policy; tool descriptions and tool-contributed guidelines carry the rest. (Anthropic, SWE-agent, AgentIF.)
5. **No persona or reasoning scaffold.** One functional identity line; thinking is controlled by the effort setting. (Zheng, Salomon, Cuadron, Gemini guide.)
6. **Enforce with the harness, inform with the prompt.** The permissions section describes the policy the sandbox enforces; the prompt does not try to be the guard. (D24, "Your AI, My Shell".)
7. **Plain, calm wording.** No capitals, no "must" or "never" for emphasis, no contradictions. (Anthropic, Google.)
8. **Static first, volatile last, patched not rebuilt.** (Pi #6621, Lumer, vendor caching documentation.)
9. **Facts over instructions.** Tell the model what the environment is, such as whether the workspace is a git repository, instead of adding rules about what to do in each case. (Meta-Harness, Gemini's repository checks.)
10. **One prompt until measured otherwise.** A model-family delta needs a paired eval showing that it helps that family. (PromptBridge, Bake records.)

## Design

### Sections and order

The prompt keeps Pi's mechanism: an untagged `preamble`, then named sections wrapped in tags, recorded in the session's system message. A later change sends only the changed sections, which `bake-ai` renders as an in-place system message ("Updated system prompt section …"), so the cached prefix and earlier thinking survive.

| Order | Section | Content | Changes |
|---|---|---|---|
| 1 | `preamble` | One identity line | Never |
| 2 | `rules` | Bake's five rules, then the tool-contributed guidelines | When the tool set changes |
| 3 | `permissions` | The active preset's sandbox and approval sentences ([D24](scope-00/support.md#decision-register)) | When the permission changes |
| 4 | `docs` | One line naming `bake-rs docs` | Never |
| 5 | `run` | The run mode | Never within a session |
| 6 | `project_context` | `AGENTS.md`, `AGENTS.override.md`, or `CLAUDE.md` files, wrapped with their paths | When the files change |
| 7 | `environment` | Working directory, platform and shell, git repository or not, session date | When one of them changes |

Pi's `tools` section is removed: it repeats the tool schemas the request already carries, plus a sentence about custom tools that `bake-rs` cannot load. Pi's `addendum`, `skills`, and custom-prompt paths stay as they are for scope 10.

### The text

The default prompt for `bake-rs -p` with the four default tools, the `workspace-write` preset, and approvals that no one can answer:

```text
You are Bake, a coding agent working in the user's terminal. You help with software tasks in this workspace by reading code, running commands, and editing files.

<rules>
- Follow the conventions of the surrounding code and the project instructions below. The user's direct requests take precedence over project files.
- Do what was asked, and mention further changes instead of making them. Commit or push only when asked, and do not revert or discard changes you did not make.
- Work in as few model round trips as you can. Put independent tool calls in one response: read the files you need together, and send an edit with the command that checks it, since calls in one response run in order. Do not re-read a file to confirm an edit that succeeded.
- When a change can be checked, run a real check before you finish. Report results accurately, including failures and checks you could not run.
- Keep replies concise; they render as Markdown in a terminal. Refer to code as path or path:line.
- Use read to examine files instead of cat or sed.
- Use edit for precise changes (edits[].oldText must match exactly)
- When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls
- Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.
- Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.
- Use write only for new files or complete rewrites.
</rules>

<permissions>
File sandbox: workspace-write. Sandboxed tools can modify files under "/home/user/project", and possibly in some temporary directories.

Approval prompts: enabled. Actions that need approval request it before running, and fail if no one can answer.
</permissions>

<docs>
For questions about Bake itself, such as its settings, models, permissions, or sessions, run `/usr/local/bin/bake-rs docs` and read the topic it lists.
</docs>

<run>
This is a non-interactive run: no one can answer questions. Make reasonable assumptions, state them in your final reply, and finish the task.
</run>

<environment>
Working directory: /home/user/project
Platform: linux, shell: bash
Git repository: yes
Date: 2026-10-13
</environment>
```

How each Bake rule earns its place:

| Rule | Why it stays |
|---|---|
| Conventions, project instructions, precedence | Context files change process and cost more than correctness, and agents follow them closely; GPT-6 guidance asks for the precedence statement. Carried over from Bake 0.3 |
| Scope, commit and push, the user's changes | The reversibility rule every surveyed agent carries. The eval tasks state these limits themselves, so the rule matters most in real use. Carried over from Bake 0.3 |
| Round trips | The largest measured prompt effect in Bake's records; its absence explains the native arm's Gemini and Claude regressions. Carried over from Bake 0.3 |
| Real check, accurate report | Covers `unprompted_verify` and the honest-reporting requirement; carried over from Bake 0.3 |
| Concise Markdown, path:line | Output format for a terminal; replaces Pi's "Be concise" and "Show file paths clearly" |

The tool-contributed guidelines are Pi's, unchanged except that `bash` no longer contributes a line about environment variables. When `grep`, `find`, and `ls` are not selected, Pi's "Use bash for file operations like ls, rg, find" line still comes first among them.

### Removed text

| Pi text | Reason |
|---|---|
| "You are an expert coding assistant operating inside pi, a coding agent harness." | Names the wrong harness; personas do not improve performance |
| The `tools` section and "you may have access to other custom tools depending on the project" | Duplicates the tool schemas; `bake-rs` has no custom tools |
| "You can inspect PI_* environment variables for current model and session details." | No model used it in 144 samples; the variables are renamed (see below) |
| The `docs` section's eight lines about Pi's README, docs, examples, extensions, themes, MCP, and code mode | Points at files that do not ship and at features `bake-rs` does not have; no model read them |
| "Be concise in your responses", "Show file paths clearly when working with files" | Folded into the output rule |

### Environment section

The section states facts and gives no instructions:

- **Working directory**: the session's working directory, with `/` separators.
- **Platform and shell**: the operating system family and the shell the `bash` tool runs.
- **Git repository**: `yes` when the working directory or an ancestor holds `.git`, found without running git; otherwise `no`. A model that knows the workspace is not a repository has no reason to run `git status` to find out.
- **Date**: the calendar date of the session's start, taken from the session header, so a resumed session renders the same text and patches nothing. A run without a session file uses the current date.

The section comes last because it is the only one whose facts vary between sessions in the same workspace. When a fact changes in a resumed session, the change reaches the model as a section patch.

### Run mode

`-p` runs have no one to answer a question, so the `run` section asks for stated assumptions and a finished task. An interactive session, when scope 15 connects one, uses: "This is an interactive session. Ask the user only when you are blocked or before an action that is hard to reverse or visible to others." The line is the one place headless and interactive prompts differ.

### Self-documentation

The binary carries Bake's 0.4 user documentation, compiled in, so it always matches the installed version and works offline. `bake-rs docs` lists the topics and `bake-rs docs <topic>` prints one: the Bake home, settings, models and providers (including the imported CLIProxyAPI route), permissions and the sandbox, sessions, context files, and the CLI. The prompt's `docs` section is one line naming that command by the executable's absolute path. A built-in skill could replace the line when scope 10 adds skills; the command stays either way.

### Session environment variables

The `bash` tool exports the session's details to the commands it runs. They take Bake's names: `BAKE_SESSION_ID`, `BAKE_SESSION_FILE` (for a persisted session), `BAKE_PROVIDER`, `BAKE_MODEL`, and `BAKE_REASONING_LEVEL`. The `PI_*` names are not exported, since 0.4 has no Pi or TypeScript install to stay compatible with ([D32](scope-00/support.md#decision-register)). The documentation describes them; the prompt does not.

### Model families

Every family gets the same prompt. A family-specific delta lives in its own `model` section after `rules`, holds at most a few sentences, and is added only with a paired eval showing that it helps that family. Candidates from the vendor guides, in the order to test them if the baseline leaves a gap:

- GPT-6: OpenAI's initiative and follow-through paragraph, and its prose-over-lists line.
- Claude Sonnet 5.5 at low effort: Anthropic's paragraph on finishing and verifying without checking in.
- Gemini 3.8 Flash: none; the facts in the environment section and the round-trip rule address its measured gaps.
- DeepSeek V4: none. Its measured behavior does not change with either prompt; its needs are in the transport (returning `reasoning_content`), not the prompt.

## Evaluation plan

The candidate is measured against `bake-rs` at its pull request's base, both built in clean worktrees, with the native arm of the agent-loop evaluator:

- **Runs.** The standard suite and the extended suite, the standard and extended model sets, three trials. The extended suite covers what a prompt can change that the standard suite does not: `unprompted_verify`, `instructions_file`, `explore_answer`, `test_fix_loop`, `noisy_failure`, `edit_recovery`, and `long_session`. `background_test` is reported but not judged, since `bake-rs` has no background jobs.
- **Expected effects.** Gemini requests fall by at least a third and its repository-check tool errors disappear; Claude's edit-and-check splits fall toward zero; DeepSeek and GPT stay within noise; first-request size falls.
- **Acceptance.** No model's failures rise beyond gateway errors and noise, and every flagged regression has a stated cause. A second, informational record pairs the candidate with the 0.3 TypeScript agent to show the remaining gap.
- **Ablation.** If a rule's effect is unclear, a step record removes that rule alone, as the trim-and-batch record did.

## Implementation plan

Three pull requests, each based on the one before:

1. **This design and D33** (documentation only): this page, the decision register, and the statements in `AGENTS.md` and the roadmap that say prompts follow Pi.
2. **`bake-rs docs`** (no model-visible change): the 0.4 user documentation compiled into the binary, the `docs` subcommand, and tests that every topic renders.
3. **The prompt** (model-visible): the sections and text above, the environment and run sections, the `BAKE_*` variables, a fixture test that pins the full first request for each preset and run mode in place of Pi's prompt fixtures, and the eval record.

## Candidates not adopted

| Candidate | Why not now |
|---|---|
| A sentence that file contents and command output cannot override the user | Injection defense belongs to the sandbox and approvals; the sentence risks the model ignoring files the user points at (`instructions_file`). Revisit with an injection scenario in the evaluator |
| A git-status snapshot in the environment section | Volatile and invalidates the cache; the model can run `git status` in a repository |
| Moving Pi's edit guidelines into the `edit` tool description | Tool text follows Pi under D33; a separate change with its own record |
| An "expert" persona or a reasoning scaffold | No measured benefit; the effort setting controls thinking |
| A per-model prompt for every family | No family has a measured need yet |

## Open questions

- Whether the date helps any measured behavior. It costs one line and no cache; the evaluator has no date-sensitive scenario.
- Whether Pi's four `edit` guideline lines are all needed once the round-trip rule asks for one edit call per file.
- How large a context file can grow before it costs more than it saves; the published results are underpowered for sizes beyond typical files.
