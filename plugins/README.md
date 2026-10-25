# Plugin fixtures

`guard` is the protected-paths example and `fixtures/hostile` exercises the Wasm host's limits and failure paths. Cargo tests load their committed components. Build both from the repository root with the pinned toolchain:

```sh
rustup target add wasm32-wasip2
export CARGO_TARGET_WASM32_WASIP2_RUSTFLAGS="--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$PWD=/bake/rust"
cargo build --locked --profile plugin --target wasm32-wasip2 -p bake-plugin-guard -p bake-plugin-fixture-hostile
cmp target/wasm32-wasip2/plugin/bake_plugin_guard.wasm plugins/guard/plugin.wasm
cmp target/wasm32-wasip2/plugin/bake_plugin_fixture_hostile.wasm plugins/fixtures/hostile/hostile.wasm
```

The `/bake/rust` remap destination is a fixed virtual source prefix used by the committed fixtures, independent of the checkout layout.

The comparisons report whether the pinned build reproduces the committed bytes. A toolchain change can alter them; inspect a difference before replacing a fixture. To record an intentional plugin change, copy the corresponding build output to its committed path and run the CLI plugin integration tests (`cargo test --locked -p bake-cli --test wasm_plugin_host --test wasm_plugin_session`).
