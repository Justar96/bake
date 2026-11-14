# Bake direction

This page says where Bake is going and the rules that shape it. Keep it short and current: update a line when its priority is done or dropped instead of adding history. Git history keeps how each rule was reached.

## Where Bake is

Bake is one Rust executable, `bake`, released for Linux x86_64 (glibc 2.34 or newer) and Windows x64. Windows has no qualified sandbox yet, so only `danger-full-access` runs commands there. Linux arm64 is no longer released; macOS comes later. The earlier TypeScript agent (0.3 and before) is gone: Bake reads no 0.3 files or `DSH_*` variables, and its download route is retired.

## Rules Bake keeps

- **Pi is the upstream.** Port from Pi's latest official release, record the revision and adapted files, keep its MIT notice, and port its tests. Bake reads and writes Pi's session format.
- **Bake's own contracts.** The CLIProxyAPI provider route, the sandbox with its approvals and permission presets, and the Bake home (`BAKE_HOME`, else `~/.bake`) with its settings and credentials are Bake's, not Pi's. So is the system prompt.
- **Opt-in by measurement.** Subagents, reminders, and telecom stay behind settings until a paired evaluation shows they help by default.
- **Signed releases only.** Installs trust one Ed25519 key, the channel never moves backward, and a version is never republished with other bytes ([releases](../distribution/README.md)).
- **Linux glibc floor 2.34.** A release build that needs newer glibc symbols fails to pack.

## Priorities

1. **Stability and cleanup.** Remove code, docs, and settings that no longer pay for themselves; keep crate docs and `docs/development.md` short enough to read; keep `cargo test --workspace` fast and free of flakes; fix the bugs users report before adding features.
2. **Skills.** Make skills the first way to extend Bake: discovery, a clear listing in the prompt, and checks that a skill's file and frontmatter are valid (`bake docs skills`).
3. **Plugins.** Grow the Wasm plugin API (`bake:plugin`, today a veto on tool calls) one capability at a time, each with a named consumer in `plugins/`, an isolation test, and a versioned WIT change (`bake docs plugins`, [plugins/README.md](../plugins/README.md)).
4. **Releases.** Every release goes through `bake-release prepare` and `bake-release publish`; any manual step that remains is a candidate for the tool.

## Not planned now

macOS releases, a qualified Windows sandbox, Linux arm64, goals and the task router, MCP OAuth, MCP resource subscriptions and prompts, and provider routes beyond CLIProxyAPI and `models.json`. Each needs the owner's request before work starts.

## Versions

Releases are SemVer `0.4.x` until the owner calls 1.0. Every user-visible change adds a line under `## [Unreleased]` in [CHANGELOG.md](../CHANGELOG.md) when it lands, so `prepare` can turn that section into the release notes without editing.
