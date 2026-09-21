#!/usr/bin/env bash
# Validate fork-local source and tests without running the upstream full suite.
#
# Targets run in the listed order and can be selected individually, so a failure
# is re-run on its own instead of behind everything that already passed.
#
#   tui/scripts/check.sh              # every target
#   tui/scripts/check.sh types spec   # only those, in that order
#   tui/scripts/check.sh --list
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$TASK_ROOT"

ORDER=(peers types unit spec layout docs)

# A case rather than an associative array: stock macOS ships bash 3.2, which
# has no `declare -A`, and this script is run by everyone touching tui/.
describe() {
  case "$1" in
    peers)  echo 'React instance identity across upstream DOM and Ink consumers' ;;
    types)  echo 'strict types for the three packages and the test programs' ;;
    unit)   echo 'pure modules under bun test' ;;
    spec)   echo 'component and integration specs under Node and vitest' ;;
    layout) echo 'rendered layout invariants from DESIGN-LAYOUT.md' ;;
    docs)   echo 'local Markdown links and anchors' ;;
  esac
}

target_peers() { node tui/scripts/check-react-peers.mjs; }

target_types() {
  node node_modules/typescript/bin/tsc -b tui/tsconfig.json --pretty false
  node node_modules/typescript/bin/tsc -p tui/tsconfig.tests.json --pretty false
}

# Filter by suffix rather than naming files: `bun test` matches `.spec.` too,
# and those need Node, while an explicit list silently omits every pure test
# added later.
target_unit() { (cd tui && bun test .test.ts); }

target_spec() { node node_modules/vitest/vitest.mjs run --config tui/vitest.config.ts; }

# Each scene renders through Ink and exits non-zero when a region overruns the
# budget that keeps Ink off its screen-clearing path, when a frame changes
# height while a turn runs, or when a rendered character rises above 0x80,
# where a terminal and string-width can disagree about its width.
target_layout() {
  for scene in frames stability realloop overlays stream separation chat ascii; do
    node "tui/prototype/$scene.mjs" > /dev/null
  done
  echo "layout invariants: budgets, stability, and the ASCII vocabulary hold"
}

target_docs() { node --import tsx/esm tui/scripts/check-docs.ts; }

if [ "${1:-}" = "--list" ]; then
  for target in "${ORDER[@]}"; do printf '%-8s %s\n' "$target" "$(describe "$target")"; done
  exit 0
fi

if [ "$#" -eq 0 ] || [ "${1:-}" = "all" ]; then
  selected=("${ORDER[@]}")
else
  selected=("$@")
  for target in "${selected[@]}"; do
    if [ -z "$(describe "$target")" ]; then
      echo "unknown target: $target" >&2
      echo "known: ${ORDER[*]}" >&2
      exit 2
    fi
  done
fi

started=$SECONDS
for target in "${selected[@]}"; do
  step=$SECONDS
  echo "--- $target: $(describe "$target")"
  "target_$target"
  echo "--- $target passed in $((SECONDS - step))s"
done
echo "check: ${#selected[@]} target(s) passed in $((SECONDS - started))s"
