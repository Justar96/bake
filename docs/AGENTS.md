# Bake documentation

Follow the root [agent instructions](../AGENTS.md). The [root README](../README.md) owns the product overview and entrypoints; [CONTRIBUTING](../CONTRIBUTING.md) owns contribution and branch policy; [docs/development.md](development.md) owns workspace APIs and Cargo checks. User documentation lives in [the CLI docs](../crates/bake-cli/docs/) and is embedded in the executable. This directory owns architecture, testing guidance, and roadmap records.

Describe the current Rust executable and direct Cargo workflow. Check commands against the owning source before documenting them. The build hosts in CI do not by themselves establish released platform support. Keep upstream acknowledgements at the bottom of the root README and retain license attribution in the owning notices.

Write current behavior, prerequisites, failure conditions, and commands that have been exercised. Keep paragraphs on one physical line. Documentation is English only. Preserve API obligations, security rules, and source attribution. Prefer a link to the owning source over repeating API catalogs.

Roadmap records describe their recorded subject. Preserve relevant decisions without treating historical tooling commands as current requirements or restoring deleted archives. Link historical source and evidence to a specific existing Git revision; use local links for current files.

For documentation-only changes, check local file links and run `git diff --check -- docs/`. Explain any unverified commands or behavior rather than claiming comprehensive validation. Follow the root instructions when changing documentation embedded in the executable.
