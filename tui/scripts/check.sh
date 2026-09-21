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
