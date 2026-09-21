#!/usr/bin/env bash
# Validate fork-local source and tests without running the upstream full suite.
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$TASK_ROOT"
node node_modules/typescript/bin/tsc -b tui/tsconfig.json --pretty false
node node_modules/typescript/bin/tsc -p tui/tsconfig.tests.json --pretty false
(cd tui && bun test tests/no-bun-runtime.test.ts packages/ui/tests/project.test.ts packages/ui/tests/editor.test.ts)
node node_modules/vitest/vitest.mjs run --config tui/vitest.config.ts
