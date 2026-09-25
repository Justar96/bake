# Changelog

Notable changes to Bake. `/changelog` in the terminal prints the section for the running version, so each release heading must match the root `package.json` version.

## [0.1.1] - 2026-09-25

- `bake update` installs the newest release in place, and `bake update --check` only reports; any failure leaves the install as it was. Installs of 0.1.0 run the installer once more to get it.
- Releases are signed: the installers and `bake update` refuse a manifest the Bake release key did not sign. Every release is also published on GitHub Releases.
- The status line names a newer release when one is available.
- Installs and updates show a small progress line in interactive terminals; `BAKE_NO_ANIMATION=1` turns it off.
- A header row above the composer shows what the current turn is doing and the active goal.
- Final answers end with a dim tokens-per-second line.
- Shift+Tab cycles the reasoning effort.
- The live area fits the terminal's size, and a long batch of tool calls keeps its heading, folding older calls into a count.
- Tool calls show a `●` heading with a `⎿` result line.
- Models whose streams report empty token usage no longer reset the context meter to 0%.

## [0.1.0]

- Bake is an independent terminal coding agent with a Bun-managed workspace, a Node runtime, and `~/.bake` as its default home.
- A fresh session opens with a welcome block showing the Bake version, a short notice, and example commands.
- `/changelog` shows what changed in the running version.
- The terminal follows the newest line, prints answers as they stream, and merges each action's call and result into one block.
- Tool cards, a task checklist, and a subagent panel sit above the composer.
- Session discovery skips damaged compressed headers instead of hiding healthy sessions.
