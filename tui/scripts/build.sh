#!/usr/bin/env bash
# Bundle the TUI packages to Node ESM so the built `dsh` CLI can load them.
#
# Why this exists: tools do not work under the tsx source launch. `dsh-tools`
# keys its scheduler with `Symbol('…')`, not `Symbol.for('…')`, and the source
# launch ends up with two module instances of that package — so the two symbols
# differ and `ctx.tools[TOOL_RUNTIME_SCHEDULER]` reads undefined. Upstream
# `headless` has the same failure from source and works from `lib/`.
#
# Every `@deepseek-ai/*` import stays external for exactly that reason: bundling
# a copy of a harness package would reintroduce the duplicate instance this
# build exists to avoid. Ink, React, and Commander remain external too; the
# renderer and its hooks must resolve the same React instance. App dependencies
# include every external import emitted from the embedded UI package.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/tui"

command -v bun >/dev/null || { echo "bun is required to build the TUI" >&2; exit 1; }

EXTERNALS=(
  --external '@deepseek-ai/*'
  --external 'ink' --external 'react' --external 'commander'
)

bun build packages/app/src/index.ts \
  --target=node --format=esm "${EXTERNALS[@]}" --outfile=packages/app/lib/index.js
bun build packages/app/src/startup.ts \
  --target=node --format=esm "${EXTERNALS[@]}" --outfile=packages/app/lib/startup.js

# The recorder builds too: fixtures worth having contain tool events, and tools
# only run on the built path. It lands in the app package because externals
# resolve from the output file's directory, and `harness/` has no node_modules.
bun build packages/harness/record.ts \
  --target=node --format=esm "${EXTERNALS[@]}" --outfile=packages/app/lib/record.js

echo "built:"
ls -l packages/app/lib/*.js | awk '{print "  " $NF " (" $5 " bytes)"}'
