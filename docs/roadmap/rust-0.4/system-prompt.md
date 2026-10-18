# System prompt design for the Rust harness

## Summary

`bake-rs` sends the system prompt of Pi v1.1.0 (`abe508e1`), the release it ports, byte for byte, plus Bake's permissions section; Pi v1.2.0 (`06a466935536`) leaves the prompt and the tools' prompt text unchanged. That prompt names the harness `pi`, points the model at Pi documentation that does not ship beside the binary, and lists environment variables named `PI_*`. It also has no guidance on batching tool calls, which Bake's own evals show is the sentence that matters most for some models. This page designs Bake's own system prompt for the Rust harness from three sources: Bake's paired eval records, published research, and the prompts and guides of current coding agents and model vendors.

The design keeps Pi's prompt machinery (named sections that a session records and later patches) and replaces the text: a one-sentence identity, Bake 0.3's measured rules, the tool-contributed guidelines, a one-line pointer to Bake's own documentation, a run-mode line, the permissions section, and a small environment section last. The prompt states the outcome and the boundaries and leaves the method to the model. A rule stays only when an eval or a cited failure shows that the model behaves worse without it, and the one rule that promises harness behavior, that tool calls in one response run in order, is made true by the harness. One prompt serves every model family until a paired eval shows that a family needs a delta.

**Status: implemented, 2026-10-13.** [D33](scope-00/support.md#decision-register) makes the system prompt Bake's own. `bake-rs` sends the prompt this page designs, measured in the [system prompt record](../../../evals/agent-loop/versions/unreleased/2026-10-13-bake-system-prompt/REPORT.md); the [implementation plan](#implementation-plan) and [evaluation plan](#evaluation-plan) record how it landed.

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

- **Clean.** Every line describes Bake as it is: its name, its documentation, its environment variables, its permissions, and how it runs tools. No text describes a feature `bake-rs` does not have.
- **Concise.** The prompt is the smallest set of instructions that fully describes the expected behavior. Brevity is a result, not the goal: a sentence that measurably saves model round trips stays.
- **Model-driven.** The prompt states what done looks like and where the boundaries are; the model chooses how to get there. Guarantees that must hold every time are enforced by the harness (the sandbox, approvals, and permission presets of [D24](scope-00/support.md#decision-register)), not by prose.
- **Cache-stable and reconstructable.** The system prompt is byte-stable for a session, with the facts most likely to differ between sessions last. Every change reaches the model as a recorded section patch, so model input stays reconstructable from the session file.

Non-goals: tool descriptions, schemas, results, and repair text keep following Pi, with D24's sandbox additions, under D33. Prompt templates and the `--system-prompt` and `--append-system-prompt` flags remain scope 10 work; this design leaves room for them without adding them. Skills landed in scope 10 with Pi's section; the [skills section](#skills-section) records why it stays as Pi wrote it.

## Evidence

### Bake's own measurements

Paired records compare two builds on the same day, models, and scenarios, so they measure an effect rather than drift. The noise floor matters: on identical code, the [eval fidelity record](../../../evals/agent-loop/versions/unreleased/2026-10-06-eval-fidelity/REPORT.md) found paired token changes of up to about 10% for Gemini, DeepSeek Flash, and GPT, and about 2% for Claude.

| Change | Effect | Record |
|---|---|---|
| The round-trip paragraph alone: "Put independent tool calls in one response: read the files you need together, and send an edit with the command that checks it… do not re-read a file to confirm an edit that succeeded" | Gemini 3.8 Flash total tokens −43.3% [−49.7, −36.9], requests 165 → 95. Claude Opus −9.6% [−16.3, −3.2], requests 100 → 90. DeepSeek V4 Pro and GPT-6.1 within noise | [trim-and-batch, step 1](../../../evals/agent-loop/versions/v0.3.4/2026-10-03-trim-and-batch/steps/1-guidance-only/REPORT.md) |
| Shorter tool descriptions and a `todo_write` note on top of that (tool JSON about 19.7k → 13.1k characters) | Total tokens −22% to −32% on all four models, requests about unchanged; GPT failures 0 → 4, flagged | [trim-and-batch, step 2](../../../evals/agent-loop/versions/v0.3.4/2026-10-03-trim-and-batch/steps/2-trim-and-todo/REPORT.md) |
| Longer tool guidance (+1,940 first-request bytes) | Gemini +8.8% [3.3, 14.7] total tokens, flagged | [tool recovery](../../../evals/agent-loop/versions/v0.3.0/2026-09-30-tool-recovery/REPORT.md) |
| `bake-rs` with Pi's prompt against the 0.3 TypeScript agent with Bake's prompt, 2026-10-13 | Claude about 30% fewer tokens (smaller tool surface); DeepSeek's arms used different transports, so its change is confounded. Claude requests +10% to +12%. Gemini requests 77 → 166 | [native arm](../../../evals/agent-loop/versions/unreleased/2026-10-13-native-arm/REPORT.md) |

The native-arm transcripts are consistent with the following causes, though the two arms also differ in tools, permission preset, and transport:

- **Batching.** Of Gemini's responses that called tools, 75% (42 of 56) held more than one call under Bake's prompt and 2% under Pi's, so each read, edit, and check became its own request. Claude still batched reads under Pi's prompt but sent the edit and its check in separate responses (Opus in 4 of 21 samples, Sonnet in 7). Both behaviors match the absence of the round-trip paragraph.
- **Repository checks.** Gemini ran `git status` in 18 of 24 samples under Pi's prompt, each time right after a passing test, in a fixture that is not a git repository; under Bake's prompt it ran git in 6 (five `git status`, one `git diff`). Neither prompt says whether the workspace is a repository.
- **Dead text.** No model, in the 144 native-arm samples, read the Pi documentation paths or inspected a `PI_*` variable. The docs section alone is 1,355 of the prompt's 3,006 characters.
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
| OpenAI, [Codex prompting guide](https://github.com/openai/openai-cookbook/blob/main/examples/gpt-5/codex_prompting_guide.ipynb) | Remove prompting for an upfront plan, preambles, or status updates, which can make the model stop early |
| OpenAI, [Rethinking skills and prompts for GPT-6 Astra](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra) | Instructions that helped earlier models can constrain newer ones; revisit every `AGENTS.md` instruction when the model changes |
| Google, [Gemini prompting strategies](https://ai.google.dev/gemini-api/docs/prompting-strategies) | Be precise and direct, avoid persuasive language, and do not ask for reasoning in the output |
| AWS, [Strands Agents' model-driven approach](https://strandsagents.com/blog/strands-agents-model-driven-approach/) | Let the model drive its own behavior and tool use "rather than trying to predict and code for every possible scenario" |
| Pi v1.1.0 and v1.2.0 | About 700 tokens plus tool schemas; one prompt for every model; removed the date in July 2026 to keep the prompt cache stable ([#6621](https://github.com/earendil-works/pi/issues/6621)); runs every batch of tool calls in parallel |
| Codex CLI, Claude Code, Gemini CLI, opencode | Send the working directory, platform, and whether the workspace is a git repository, and most send the date; Codex CLI and opencode deliver these as context items that later turns update instead of rebuilding the prompt. Safety is framed by reversibility: do not commit, push, or discard the user's changes unless asked. Codex CLI's base instructions add that "direct system/developer/user instructions (as part of a prompt) take precedence over AGENTS.md instructions" |

## Principles

1. **Keep a short fixed core.** Remove any line that no eval or cited failure supports. (IFScale, AgentIF, mini-SWE-agent, Pi.)
2. **State the outcome and the boundaries, not the procedure.** (Anthropic, OpenAI, Strands, Coding Agents Don't Know When to Act.)
3. **Keep the round-trip guidance, and make its promise true.** It is the one behavioral sentence with a large measured effect, for Gemini and Claude; it tells the model that calls in one response run in order, so the harness runs them in order. (Bake records.)
4. **Tool-specific guidance belongs to the tool.** The system prompt carries only cross-tool policy; tool descriptions and tool-contributed guidelines carry the rest. (Anthropic, SWE-agent, AgentIF.)
5. **No persona or reasoning scaffold.** One functional identity sentence; thinking is controlled by the effort setting. (Zheng, Salomon, Cuadron, Gemini guide.)
6. **Enforce with the harness, inform with the prompt.** The permissions section describes the policy the sandbox enforces in this run; the prompt does not try to be the guard. (D24, "Your AI, My Shell".)
7. **Plain, calm wording.** No capitals, no "must" or "never" for emphasis, no contradictions. (Anthropic, Google.)
8. **Stable first, most variable last, patched not rebuilt.** (Pi #6621, Lumer, vendor caching documentation.)
9. **Facts over instructions.** Tell the model what the environment is, such as whether the workspace is a git repository, instead of adding rules about what to do in each case. (Meta-Harness, Gemini's repository checks.)
10. **One prompt until measured otherwise.** A model-family delta needs a paired eval showing that it helps that family. (PromptBridge, Bake records.)

## Design

### Sections and order

The prompt keeps Pi's mechanism: an untagged `preamble`, then named sections wrapped in tags, recorded in the session's system message. A later change sends only the changed sections. On `anthropic-messages`, which accepts system messages mid-conversation, `bake-ai` renders the patch in place ("Updated system prompt section …"), so the cached prefix and earlier thinking survive. The `openai-responses` and `openai-completions` APIs do not accept them by default, so `bake-ai` folds every patch into a rebuilt leading system message and the provider's cache restarts at the first changed byte. Sections are therefore ordered from the most stable to the most variable across sessions, and no section changes within a session unless the user changes something.

| Order | Section | Content | Changes |
|---|---|---|---|
| 1 | `preamble` | One identity sentence | Never |
| 2 | `rules` | Bake's five rules, then the tool-contributed guidelines | When the tool set changes |
| 3 | `docs` | One line naming `bake-rs docs` by the executable's path | When the executable moves |
| 4 | `run` | The run mode | Never within a session |
| 5 | `addendum` | `--append-system-prompt` text, when scope 10 adds the flag | Never within a session |
| 6 | `project_context` | The loaded context files, wrapped with their paths | When the files change between runs |
| 7 | `skills` | The skills the model may load, when any load ([Skills section](#skills-section)) | When the skills change between runs |
| 8 | `permissions` | The run's sandbox and approval sentences ([D24](scope-00/support.md#decision-register)) | When the permission changes |
| 9 | `environment` | Working directory, platform, shell, git repository or not, session start date | When one of them changes between runs |

Pi's `tools` section is removed: it repeats the tool schemas the request already carries, plus a sentence about custom tools that `bake-rs` cannot load. Pi's `cwd` section becomes the first line of `environment`. A custom prompt (`--system-prompt`, scope 10) replaces `preamble`, `rules`, and `docs`, as in Pi, and keeps the other sections, since they state facts the harness enforces. Pi's `skills` section, the list of loaded skills, follows `project_context` when skills load: like the context files, it changes only between runs.

### The text

The default prompt for `bake-rs -p` with the four default tools, the `workspace-write` preset, no context files, and a git repository:

```text
You are Bake, a coding agent that works in the user's workspace by reading code, running commands, and editing files.

<rules>
- Follow the conventions of the surrounding code and the project's instructions. The user's requests take precedence over project instructions.
- Commit or push only when the task requests it, and do not discard changes you did not make.
- Work in as few model round trips as you can. Put independent tool calls in one response: read the files you need together, and send an edit with the command that checks it, since calls in one response run in order. Do not re-read a file to confirm an edit that succeeded.
- Report results and verification accurately, including failures and skipped checks.
- Keep the final answer concise, factual, and neutral.
- Use bash for file operations like ls, rg, find
- Use read to examine files instead of cat or sed.
- Use edit for precise changes (edits[].oldText must match exactly)
- When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls
- Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.
- Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.
- Use write only for new files or complete rewrites.
</rules>

<docs>
For questions about Bake itself, such as its settings, models, permissions, or sessions, run `/usr/local/bin/bake-rs docs` to list the topics and `/usr/local/bin/bake-rs docs <topic>` to read one.
</docs>

<run>
This is a non-interactive run: no one can answer questions. Make reasonable assumptions, state them in your final answer, and finish the task.
</run>

<permissions>
File sandbox: workspace-write. Sandboxed tools can modify files under "/home/user/project", and possibly in some temporary directories.

Approval prompts: disabled. Actions that need approval are rejected automatically.
</permissions>

<environment>
Working directory: /home/user/project
Platform: linux
Shell: bash
Git repository: yes
Session started: 2026-10-13 (UTC)
</environment>
```

Where each of Bake's rules comes from:

| Rule | Source and support |
|---|---|
| Conventions and project instructions | Bake 0.3's headless prompt ("Follow the conventions of the surrounding code"). The precedence sentence is new, after Codex CLI's base instructions: agents follow context files closely, and those files change process and cost more than correctness (Gloaguen, Lulla) |
| Commit, push, and the user's changes | Bake 0.3, verbatim. The reversibility rule every surveyed agent carries; the eval tasks state these limits themselves, so the evaluator cannot measure it |
| Round trips | Bake 0.3, without "Make several changes to one file in one edit call", which `edit`'s own guideline states. The largest measured prompt effect in Bake's records; its absence matches the native arm's Gemini and Claude regressions |
| Accurate report | Bake 0.3, verbatim |
| Final answer | Bake 0.3, verbatim; replaces Pi's "Be concise in your responses" and "Show file paths clearly when working with files" |

Without tools, the round-trip rule is left out. The tool-contributed guidelines are Pi's, except that `bash` contributes none; Pi's one `bash` guideline named the `PI_*` variables. When `grep`, `find`, and `ls` are not selected, Pi's "Use bash for file operations like ls, rg, find" line comes first among them.

### Removed text

| Pi text | Reason |
|---|---|
| "You are an expert coding assistant operating inside pi, a coding agent harness." | Names the wrong harness; personas do not improve performance |
| The `tools` section and "you may have access to other custom tools depending on the project" | Duplicates the tool schemas; `bake-rs` has no custom tools |
| The `cwd` section | Becomes the first line of `environment` |
| "You can inspect PI_* environment variables for current model and session details." | No model used it in 144 samples; the variables are renamed (see below) |
| The `docs` section's eight lines about Pi's README, docs, examples, extensions, themes, MCP, and code mode | Points at files that do not ship and at features `bake-rs` does not have; no model read them |
| "Be concise in your responses", "Show file paths clearly when working with files" | Replaced by Bake 0.3's final-answer rule |

### Tool calls in one response run in order

Pi runs the tool calls of one response in parallel, serializing only edits to the same file, and so does `bake-rs` today: an edit and the command that checks it, sent together as the round-trip rule asks, can race, and the check can see the old file. `bake-rs` therefore marks `bash`, `edit`, and `write` sequential with Pi's own per-tool execution mode. A response that holds any of them runs one call at a time in the order the model wrote them, and a response of reads alone still runs in parallel. The 0.3 TypeScript agent, whose prompt the rule comes from, ran calls in order.

### Run mode and permissions

`-p` runs have no one to answer a question, so the `run` section asks for stated assumptions and a finished task. An interactive session, when scope 15 connects one, uses: "This is an interactive session. Ask the user only when you are blocked or before an action that is hard to reverse or visible to others." The line is the one place headless and interactive prompts differ in their rules.

The permissions section states what the run enforces. A `-p` run has no one to answer an approval prompt, so every escalation fails; the section then carries D24's `never` sentence even under the `ask` preset, so the model does not spend a round trip on a request that cannot succeed. The session still records the preset's own approval policy, and an interactive session with someone to answer renders the `ask` sentence.

### Environment section

The section states facts and gives no instructions:

- **Working directory**: the session's working directory, with `/` separators.
- **Platform**: the operating system family, as Rust's `std::env::consts::OS` names it.
- **Shell**: the file name of the shell the `bash` tool runs, while `bash` is selected.
- **Git repository**: `yes` when the working directory or an ancestor holds a `.git` entry, a directory or the file a worktree or submodule has, found once per run without running git; otherwise `no`. `GIT_DIR` is not consulted. A model that knows the workspace is not a repository has no reason to run `git status` to find out.
- **Session started**: the UTC date in the session header. It stays true when a session is resumed on a later day, so resuming patches nothing; a run without a session file has a header in memory with the same date.

The section comes last because its facts are the most likely to differ between sessions in the same workspace. When a fact changes in a resumed session, such as a workspace that became a repository, the change reaches the model as a section patch.

### Self-documentation

The binary carries Bake's 0.4 user documentation, compiled in, so it always matches the installed version and works offline. `bake-rs docs` lists the topics and `bake-rs docs <topic>` prints one: the command line, the Bake home, settings, models and providers (including the imported CLIProxyAPI route), permissions and the sandbox, sessions, context files, and skills. The prompt's `docs` section names that command by the executable's absolute path, shell-quoted when the path holds spaces or other special characters. A built-in skill could replace the line when scope 10 adds skills; the command stays either way.

### Skills section

When skills load, the prompt carries Pi's `skills` section between `project_context` and `permissions`, as Pi's `formatSkillsForPrompt` writes it: three sentences, then one `<skill>` entry for each skill the model may invoke, with its `name`, `description`, and the absolute `location` of its `SKILL.md`. The model loads a skill by reading that file, with `read`, or with `bash` when `read` is not selected. A skill whose frontmatter sets `disable-model-invocation` is left out, and with no such skills the section is omitted. Skills load once per run, sorted by directory entry, so the section is byte-stable for a session.

Agents present skills in one of two ways (surveyed 2026-10-13 at the named releases):

| Agent | Where the listing goes | Entry | How a skill loads | Listing limit |
|---|---|---|---|---|
| Pi v1.2.0, `bake-rs` | System prompt | XML: name, description, location | `read` | None |
| [agentskills.io integration guide](https://agentskills.io/client-implementation/adding-skills-support) | System prompt or the activation tool's description, "Both work" | XML, JSON, or bullets | File read or a dedicated tool | About 50–100 tokens per skill |
| Codex CLI rust-v0.162.1 | Developer message | `- name: description (file: path)` | File read | 2% of the context window; descriptions shortened before entries drop |
| Claude Code 2.1.296 | System reminder in a user message | `- name: description` | `Skill` tool | 1% of the context window; names always kept |
| Gemini CLI v0.63.0 | System prompt | XML: name, description, location | `activate_skill`, names as an enum | None |
| opencode v1.19.0 | System prompt, last | XML: name, description, location | `skill` tool | None |
| VS Code Copilot (`main`, `3fd165b`) | Custom instructions | XML: name, description, file | `skill` tool, or a file read | 15,000 characters, then names only |

The published evidence is on triggering, not format. Vercel's [AGENTS.md eval](https://vercel.com/blog/agents-md-outperforms-skills-in-our-agent-evals) found a listed skill unused in 56% of runs; an AGENTS.md instruction to use it raised the pass rate from 53% to 79%, but "You MUST invoke the skill" made the agent anchor on the skill and miss project files, and "explore the project first, then invoke the skill" did not. A [sandboxed Claude Code study](https://scottspence.com/posts/measuring-claude-code-skill-activation-with-sandboxed-evals) (22 prompts) found a plain instruction did not change activation. Anthropic's [skill-creator](https://github.com/anthropics/skills) and [agentskills.io](https://agentskills.io/skill-creation/optimizing-descriptions) put the remedy in the description: say what the skill does and when to use it. No controlled comparison of XML and Markdown listings was found.

`bake-rs` keeps Pi's text, placement, and format:

- **Pi first.** The section is Pi's, ported under [D22](scope-00/support.md#decision-register), and it matches the agentskills.io template for agents that load skills with a file read. A dedicated skill tool, as most other agents use, adds a tool schema and diverges from Pi for no measured gain.
- **No stronger trigger rule.** The prompt's wording follows [principle 7](#principles), and the evidence shows emphatic "must use" wording trading one failure for another. Descriptions carry the match.
- **No listing budget yet.** The section grows with the skills a user installs. A budget that shortens descriptions before it drops names, as Codex CLI and Claude Code do, is the remedy if a measured skill set makes the section costly.
- **Cache-stable by construction.** Skills change only between runs, so the section never patches within a session. A mid-session reload, if a later scope adds one, should send the change as a recorded section patch.

The `skill_check` and `skill_distractors` scenarios of the [agent-loop evaluator](../../../evals/README.md) measure the section: whether a model loads the one skill a task needs, and whether it loads skills a task does not need. The [skills section record](../../../evals/agent-loop/versions/unreleased/2026-10-13-skills-section/REPORT.md) pairs `bake-rs` with and without the listing on six models:

- **The listing is what makes skills work.** With it, every model loaded the skill `skill_check` needs and passed, apart from two GPT runaways unrelated to skills. Without it, Gemini 3.8 Flash never found the skill and failed all three trials, and DeepSeek V4 Pro failed two; Claude and DeepSeek Flash found it only by listing `.agents`, sometimes printing every skill.
- **No model over-triggers.** With the listing, no sample loaded a skill its task did not need, in either scenario.
- **The cost is the listing's bytes.** Three skills add about 1,200 characters to every request, which is 10 to 20 percent of `skill_distractors`'s small task, with no extra round trips; `skill_check` saves round trips instead (Claude 12 to 9 requests).

Pi's wording needs no stronger trigger rule on these models. The evidence for a budget is the per-skill cost, which grows with the skills a user installs.

### Names the model sees

The `bash` tool exports the session's details to the commands it runs under Bake's names: `BAKE_SESSION_ID`, `BAKE_SESSION_FILE` (for a persisted session), `BAKE_PROVIDER`, `BAKE_MODEL`, and `BAKE_REASONING_LEVEL`. The `PI_*` names are not exported, since 0.4 has no Pi or TypeScript install to stay compatible with ([D32](scope-00/support.md#decision-register)). The documentation describes them; the prompt does not. The spill files that truncation notices name take Bake's prefixes, `bake-bash-` and `bake-output-`, in place of Pi's `pi-bash-` and `pi-output-`.

### Model families

Every family gets the same prompt. A family-specific delta lives in its own `model` section after `rules`, holds at most a few sentences, and is added only with a paired eval showing that it helps that family. Candidates from the vendor guides, in the order to test them if the baseline leaves a gap:

- GPT-6: the decision-boundary and persistence guidance in OpenAI's GPT-6 Astra post.
- Claude Sonnet 5.5 at low effort: Anthropic's paragraph on finishing and verifying without checking in.
- Gemini 3.8 Flash: none; the facts in the environment section and the round-trip rule address its measured gaps.
- DeepSeek V4: none. Its measured behavior does not change with either prompt; its needs are in the transport (returning `reasoning_content`), not the prompt.

## Evaluation plan

The candidate is measured against `bake-rs` at its pull request's base, both built in clean worktrees, with the native arm of the agent-loop evaluator:

- **Runs.** The standard suite and the extended suite, the standard and extended model sets, three trials. The extended suite covers what a prompt can change that the standard suite does not: `unprompted_verify`, `instructions_file`, `explore_answer`, `test_fix_loop`, `noisy_failure`, `edit_recovery`, and `long_session`. `background_test` runs and is judged like the others; `bake-rs` has no background jobs, so the model runs the test in the foreground.
- **What is measured.** The native arm runs `danger-full-access` with `--no-session`, so the measured permissions text is that preset's, the session date comes from the in-memory header, and the `workspace-write` text is pinned by the fixture test only. The candidate also carries the ordered batches, the `BAKE_*` variables, and the spill-file prefixes, so the record measures the whole change.
- **Expected effects.** Gemini requests fall by at least a third and its repository-check commands stop; Claude's edit-and-check splits fall toward zero; DeepSeek and GPT stay within noise; first-request size falls.
- **Acceptance.** No model's failures rise beyond gateway errors and noise, and every flagged regression has a stated cause. A second, informational record pairs the candidate with the 0.3 TypeScript agent to show the remaining gap.
- **Ablation.** If a rule's effect is unclear, a step record removes that rule alone, as the trim-and-batch record did.

## Implementation plan

Three pull requests, each based on the one before, on top of the native eval arm:

1. **Design** (documentation only): this page, D33 in the decision register with its Dev Note entry, and the statements in `AGENTS.md` and the roadmap that said prompts follow Pi.
2. **`bake-rs docs`** (no model-visible change): the 0.4 user documentation compiled into the binary, the `docs` subcommand, tests that every topic renders and that every flag it names exists, and the user-facing no-model and no-key messages pointing at it instead of Pi's paths.
3. **The prompt** (model-visible): the sections and text above, the ordered batches, the environment and run sections, the `never` sentence for runs without an approver, the `BAKE_*` variables and spill-file prefixes, a fixture test that pins the full first request for each preset in place of Pi's prompt fixtures, the evaluator's native-arm description, and the eval record.

Pull requests 2 and 3 change the public Rust API, so each updates the `rust/README.md` crate rows and test paragraph and the roadmap Dev Note, runs `bun run gen-rust-migration-inventory`, `verify-rust-migration-inventory`, and `bun run doc-sync`, and pastes a `bun run preflight:mac` summary.

## Candidates not adopted

| Candidate | Why not now |
|---|---|
| "Do what was asked, and mention further changes instead of making them" | Not in Bake 0.3 and not measurable: the eval tasks state their scope. It also risks stopping a fix whose first step uncovers the next error |
| "When a change can be checked, run a real check before you finish" | Not in Bake 0.3; test it alone against `unprompted_verify` in an ablation step if the baseline under-verifies |
| A sentence that file contents and command output cannot override the user | Injection defense belongs to the sandbox and approvals; the sentence risks the model ignoring files the user points at (`instructions_file`). Revisit with an injection scenario in the evaluator |
| The model's name in the identity sentence, as Bake 0.3 has | A model switch would patch the prompt, and on OpenAI-style APIs restart the cache |
| A git-status snapshot in the environment section | Volatile and invalidates the cache; the model can run `git status` in a repository |
| Today's date | Changes on resume and restarts the cache on OpenAI-style APIs; the session start date stays true |
| Moving Pi's edit guidelines into the `edit` tool description | Tool text follows Pi under D33; a separate change with its own record |
| An "expert" persona or a reasoning scaffold | No measured benefit; the effort setting controls thinking |
| A per-model prompt for every family | No family has a measured need yet |
| A dedicated skill tool, as Claude Code, Gemini CLI, and opencode have | Diverges from Pi's file read for no measured gain; see [Skills section](#skills-section) |
| "You must use a skill whose description matches the task" | Emphatic trigger wording made Vercel's agent anchor on the skill and miss project files; descriptions carry the match |
| A token budget for the skills listing | No measured skill set makes the section costly yet |

## Open questions

- Whether the session date helps any measured behavior. It costs one line and no cache; the evaluator has no date-sensitive scenario.
- Whether Pi's four `edit` guideline lines are all needed once the round-trip rule asks for one edit call per file.
- How large a context file can grow before it costs more than it saves; the published results are underpowered for sizes beyond typical files.
- Whether the `workspace-write` text needs its own measured cell once the evaluator can run a confining backend.
- How large the skills listing can grow before a budget pays for itself, and whether every model family loads a skill from its description alone.
