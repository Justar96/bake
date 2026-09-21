#!/usr/bin/env bash
# Bundle the plugin and drive the supported profile through a real terminal.
#
#   tui/scripts/pty-smoke.sh                     # every scenario, recorded replay
#   tui/scripts/pty-smoke.sh --list
#   tui/scripts/pty-smoke.sh --only cancel       # that scenario and its prerequisites
#   tui/scripts/pty-smoke.sh --no-build --trace  # iterate on the driver, not the plugin
#   tui/scripts/pty-smoke.sh --live node24       # real API on the engine floor
#
# Every argument other than --no-build reaches pty-smoke.py; run it with --help
# for the full list.
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
build=1
args=()
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    # The built path is the only one the smoke can use, so --built is redundant;
    # it stays accepted because PLAN.md records that invocation.
    --built) ;;
    *) args+=("$arg") ;;
  esac
done
if [ "$build" = 1 ]; then "$TASK_ROOT/tui/scripts/build.sh"; fi
exec python3 "$TASK_ROOT/tui/scripts/pty-smoke.py" ${args[@]+"${args[@]}"}
