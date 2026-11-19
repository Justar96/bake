# Bake direction

This page says where Bake is going and the rules that shape it. Keep it short and current: update a line when its priority is done or dropped instead of adding history. Git history keeps how each rule was reached.

## Where Bake is

Bake is one Rust executable, `bake`, released for Linux x86_64 (glibc 2.34 or newer) and Windows x64. Windows has no qualified sandbox yet, so only `danger-full-access` runs commands there. Linux arm64 is no longer released; macOS comes later. The earlier TypeScript agent (0.3 and before) is gone: Bake reads no 0.3 files or `DSH_*` variables, and its download route is retired.

## Rules Bake keeps

- **Pi is the upstream.** Port from Pi's latest official release, record the revision and adapted files, keep its MIT notice, and port its tests. Bake reads and writes Pi's session format.
- **Bake's own contracts.** The CLIProxyAPI provider route, the sandbox with its approvals and permission presets, and the Bake home (`BAKE_HOME`, else `~/.bake`) with its settings and credentials are Bake's, not Pi's. So is the system prompt.
- **Opt-in by measurement.** Subagents, reminders, and telecom stay behind settings until a paired evaluation shows they help by default. Evaluations run with `bake-eval` and their records are kept ([evaluations](evaluations/README.md)).
- **A command or script cannot hold a turn forever.** A foreground `bash` command without a `timeout` continues as a job after `bash.foregroundSeconds`, and the call returns with its output so far; a `codemode` script that runs JavaScript without waiting on a call ends after its busy budget.
- **Nothing waits without a bound.** A waiting ask ends when other mail reaches its session, mail a session accepted is read at its next step or within a minute, and work past a cap waits in a bounded queue instead of being refused.
- **What depends on the model is a profile.** Tools, codemode mode, thinking level, compaction, and prompt notes that suit one model are set in `modelProfiles`; Bake ships none, and adds one only with an evaluation record.
- **Listings in the prompt have a budget.** The skills listing is shortened to `skills.listingTokens`, descriptions before names, and never drops a skill.
- **Tools declare their effect.** Every tool Bake registers says how its calls are scheduled: a read runs beside other reads, a file change or a command runs alone and in order, and a coordination tool runs beside other calls. A tool Bake cannot see into, from an MCP server or the SDK, runs alone unless the user trusts the server's read-only hints (`bake docs mcp`).
- **Signed releases only.** Installs trust one Ed25519 key, the channel never moves backward, and a version is never republished with other bytes ([releases](../distribution/README.md)).
- **Linux glibc floor 2.34.** A release build that needs newer glibc symbols fails to pack.

## Priorities

1. **The 0.5.0 runtime.** What remains of the [runtime design](runtime-0.5.md): plugin tools, which wait on the owner's decisions listed there. Commands as jobs, model profiles, the skills listing budget, context files found while working (off by default), and the recovery texts have landed, each with an evaluation record.
2. **Stability and cleanup.** Remove code, docs, and settings that no longer pay for themselves; keep crate docs and `docs/development.md` short enough to read; keep `cargo test --workspace` fast and free of flakes; fix the bugs users report before adding features.
3. **Skills.** Make skills the first way to extend Bake: discovery, a clear listing in the prompt, and checks that a skill's file and frontmatter are valid (`bake docs skills`).
4. **Plugins.** Grow the Wasm plugin API (`bake:plugin`, today a veto on tool calls) one capability at a time, each with a named consumer in `plugins/`, an isolation test, and a versioned WIT change (`bake docs plugins`, [plugins/README.md](../plugins/README.md)).
5. **Releases.** Every release goes through `bake-release prepare` and `bake-release publish`; any manual step that remains is a candidate for the tool.

## Not planned now

macOS releases, a qualified Windows sandbox, Linux arm64, goals and the task router, MCP OAuth, MCP resource subscriptions and prompts, and provider routes beyond CLIProxyAPI and `models.json`. Each needs the owner's request before work starts.

## Versions

Releases are SemVer `0.x`: 0.4.x fixes, then 0.5.0 once the runtime milestones land, until the owner calls 1.0. Every user-visible change adds a line under `## [Unreleased]` in [CHANGELOG.md](../CHANGELOG.md) when it lands, so `prepare` can turn that section into the release notes without editing.
