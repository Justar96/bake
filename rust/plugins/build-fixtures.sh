#!/usr/bin/env bash
# Builds the committed plugin components from their sources and the
# workspace lock: rust/plugins/guard/plugin.wasm and
# rust/plugins/fixtures/hostile/hostile.wasm. With --check it rebuilds and
# fails if either committed component differs from the build.
#
# Needs the pinned toolchain's wasm32-wasip2 target:
#   rustup target add wasm32-wasip2 --toolchain 1.99.0
# Source paths are remapped so the output does not depend on the checkout
# or Cargo home location.
set -euo pipefail
cd "$(dirname "$0")/.."
check=false
case "${1:-}" in
  --check) check=true ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
export CARGO_TARGET_WASM32_WASIP2_RUSTFLAGS="--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$PWD=/bake/rust"
cargo build --locked --profile plugin --target wasm32-wasip2 \
  -p bake-plugin-guard -p bake-plugin-fixture-hostile
built=target/wasm32-wasip2/plugin
status=0
for pair in bake_plugin_guard:plugins/guard/plugin.wasm \
            bake_plugin_fixture_hostile:plugins/fixtures/hostile/hostile.wasm; do
  source="$built/${pair%%:*}.wasm"
  committed="${pair#*:}"
  if $check; then
    if cmp -s "$source" "$committed"; then
      echo "ok: $committed"
    else
      echo "differs: $committed (rebuild with rust/plugins/build-fixtures.sh)" >&2
      status=1
    fi
  else
    cp "$source" "$committed"
    echo "wrote: $committed"
  fi
done
exit $status
