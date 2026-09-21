#!/usr/bin/env bash
# Validate fork-local source and tests without running the upstream full suite.
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$TASK_ROOT"
node tui/scripts/check-react-peers.mjs
node node_modules/typescript/bin/tsc -b tui/tsconfig.json --pretty false
node node_modules/typescript/bin/tsc -p tui/tsconfig.tests.json --pretty false
# Filter by suffix rather than naming files: `bun test` matches `.spec.` too,
# and those need Node, while an explicit list silently omits every pure test
# added later.
(cd tui && bun test .test.ts)
node node_modules/vitest/vitest.mjs run --config tui/vitest.config.ts

# Layout invariants from tui/DESIGN-LAYOUT.md. Each script renders through Ink
# and exits non-zero when a region overruns the budget that keeps Ink off its
# screen-clearing path, when a frame changes height while a turn runs, or when a
# rendered character rises above 0x80, where a terminal and string-width can
# disagree about its width.
for scene in frames stability realloop overlays stream separation chat ascii; do
  node "tui/prototype/$scene.mjs" > /dev/null
done
echo "layout invariants: budgets, stability, and the ASCII vocabulary hold"
