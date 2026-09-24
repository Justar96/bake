# Changelog

Notable changes to Bake. `/changelog` in the terminal prints the section for the running version, so each release heading must match the root `package.json` version.

## [0.1.0]

- Bake is an independent terminal coding agent with a Bun-managed workspace, a Node runtime, and `~/.bake` as its default home.
- A fresh session opens with a welcome block showing the Bake version, a short notice, and example commands.
- `/changelog` shows what changed in the running version.
- The terminal follows the newest line, prints answers as they stream, and merges each action's call and result into one block.
- Tool cards, a task checklist, and a subagent panel sit above the composer.
- Session discovery skips damaged compressed headers instead of hiding healthy sessions.
