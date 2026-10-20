#!/usr/bin/env bash
# Remote commands are built here on purpose, from values this script controls.
# shellcheck disable=SC2029
# Run a check on a Mac over SSH against this checkout's working tree.
# Intel macOS is checked here. Tracked changes go too, staged or not; untracked
# files do not.
#
#   bun run preflight:mac [command...]   default: bun run preflight --only native
#   bun run preflight:mac --log          print the last run's log again
#
# BAKE_MAC_HOST names the SSH host (default `macpro`, an alias in
# ~/.ssh/config with key login). BAKE_MAC_DIR is the Mac's work directory,
# relative to its home (default `bake-ci`); the first run clones the
# repository there. The Mac needs git, Bun, and rustup; Cargo installs the
# pinned toolchain on first use. The native package check also needs
# cargo-about 0.9.2 on the Mac's PATH, or `BAKE_CARGO_ABOUT=<path>` in the
# command; this script forwards no environment.
#
# The command runs detached on the Mac, so a dropped connection or Ctrl+C here
# does not stop it; this script follows its log and exits with its status.
set -euo pipefail

host=${BAKE_MAC_HOST:-macpro}
dir=${BAKE_MAC_DIR:-bake-ci}
if [[ ${1:-} == --log ]]; then exec ssh "$host" "cat ~/$dir/last.log"; fi
cmd=${*:-bun run preflight --only native}

cd "$(git rev-parse --show-toplevel)"
url=$(git remote get-url origin)
head=$(git rev-parse HEAD)
# A commit holding the working tree's tracked changes, or HEAD when clean.
snap=$(git stash create)
snap=${snap:-$head}

bundle=$(mktemp "${TMPDIR:-/tmp}/mac-preflight.XXXXXX")
ref=refs/mac-preflight/snap
cleanup() {
  rm -f "$bundle"
  git update-ref -d "$ref" 2>/dev/null || true
}
trap cleanup EXIT
git update-ref "$ref" "$snap"
# Only what origin lacks; the Mac fetches origin before reading the bundle.
git bundle create -q "$bundle" "$ref" --not --remotes=origin 2>/dev/null \
  || git bundle create -q "$bundle" "$ref"
ssh "$host" "mkdir -p ~/$dir"
scp -q "$bundle" "$host:$dir/snap.bundle"

tree=''
[[ $snap != "$head" ]] && tree=' + working tree'
echo "preflight:mac: $(git rev-parse --short HEAD)$tree on $host: $cmd"

# The quoted heredoc reaches the Mac unexpanded; values arrive as arguments,
# quoted once more because SSH joins its arguments into one shell string.
ssh "$host" "bash -s -- $(printf '%q ' "$dir" "$url" "$ref" "$cmd")" <<'REMOTE'
set -euo pipefail
dir=$1 url=$2 ref=$3 cmd=$4
export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"
# Real binaries before version-manager shims: the eval fixture runs node in an
# environment without HOME, where a mise shim fails.
if command -v mise >/dev/null; then
  node_bin=$(dirname "$(mise which node 2>/dev/null || command -v node)")
  export PATH="$node_bin:$PATH"
fi
cd ~/"$dir"
[[ -d bake ]] || git clone -q --filter=blob:none "$url" bake
cd bake
git fetch -q origin
git fetch -q ../snap.bundle "+$ref:$ref"
git checkout -q --force --detach "$ref"
git clean -qfdx -e rust/target -e node_modules -e .preflight
rm -f ../last.log ../last.status
status_file=$HOME/$dir/last.status
# Cargo shares one process among concurrent cases. A macOS SSH shell's
# default 256 descriptors cannot hold their files, pipes, and sockets.
mac_nofile=$(ulimit -Sn)
if [[ $mac_nofile != unlimited ]] && (( mac_nofile < 4096 )); then
  ulimit -Sn 4096
fi
echo "preflight:mac: open-file limit $(ulimit -Sn)"
# The command runs in a subshell, so an `exit` in it still leaves a status.
nohup bash -c "bun install --frozen-lockfile >/dev/null 2>&1; ($cmd); echo \$? > '$status_file'" \
  > ../last.log 2>&1 < /dev/null &
REMOTE

offset=0
while :; do
  sleep 5
  size=$(ssh "$host" "stat -f%z ~/$dir/last.log 2>/dev/null || echo 0")
  if (( size > offset )); then
    ssh "$host" "tail -c +$((offset + 1)) ~/$dir/last.log | head -c $((size - offset))"
    offset=$size
  fi
  status=$(ssh "$host" "cat ~/$dir/last.status 2>/dev/null" || true)
  if [[ -n $status ]]; then
    ssh "$host" "tail -c +$((offset + 1)) ~/$dir/last.log"
    echo "preflight:mac: exit $status"
    exit "$status"
  fi
done
