#!/usr/bin/env bash
# Build and exercise the supported profile through a real terminal.
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [ "${1:-}" = "--built" ]; then shift; fi
"$TASK_ROOT/tui/scripts/build.sh"
exec python3 "$TASK_ROOT/tui/scripts/pty-smoke.py" "$@"
