# Third-Party Notices

Bake is derived from the MIT-licensed DeepSeek Harness runtime. The notices below preserve the upstream attributions; they are not a current inventory of Bake's installed packages. Each project remains under its own license, and nothing in this file changes those terms.

Bake's dependencies are locked in [`Cargo.lock`](Cargo.lock). Native archives carry generated dependency license texts and the linked crates' NOTICE files. Attribution for sources Bake no longer contains is kept in Git history.

## Ported source (`crates/bake-ai`)

The Rust `bake-ai` crate ports code from these MIT-licensed projects. Its [`NOTICE`](crates/bake-ai/NOTICE) file reproduces both license texts, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/ai`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |
| [partial-json](https://github.com/promplate/partial-json-parser-js), Copyright (c) 2023 Promplate Dev Team | 0.1.7 | MIT |

The crate's Cargo dependencies are locked in [`Cargo.lock`](Cargo.lock) and keep their own licenses.

## Ported source (`crates/bake-agent`)

The Rust `bake-agent` crate ports Pi's agent loop and stateful agent, and the tool-argument validation of Pi's `ai` package. Its [`NOTICE`](crates/bake-agent/NOTICE) file reproduces the license text, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/agent` and `packages/ai/src/utils/validation.ts`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |

## Ported source (`crates/bake-coding-agent`)

The Rust `bake-coding-agent` crate ports Pi's coding-agent package, the line diff and patch of jsdiff that Pi's edit tool uses, and the cached summary of the pi-cached-compaction extension, and adapts the design of the pi-telecom extension in its `telecom` module, without its source. Its [`NOTICE`](crates/bake-coding-agent/NOTICE) file reproduces their license texts, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/coding-agent`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |
| [jsdiff](https://github.com/kpdecker/jsdiff), Copyright (c) 2009-2015 Kevin Decker | 8.0.4 | BSD-3-Clause |
| [pi-cached-compaction](https://github.com/nicobailon/pi-cached-compaction), Copyright (c) 2026 Nico Bailon | 0.2.0 (`e1e20de0a7e433070414602f1105cdcac258ef4f`) | MIT (declared in its `package.json`) |
| [pi-telecom](https://github.com/Justar96/pi-telecom), Copyright (c) 2026 Justar96, portions copyright (c) 2026 Tinoy Thomas | 0.8.0 (`b6fe4d8317b387facd8d764b76f9f127f7547a85`), design only | MIT |

## Ported source (`crates/bake-mermaid`)

The Rust `bake-mermaid` crate adapts the `codex-mermaid` crate of OpenAI Codex, which is licensed under the Apache License, Version 2.0, not MIT. The adapted files remain under that license; Bake's own changes and additions are under the repository's MIT license. Its [`NOTICE`](crates/bake-mermaid/NOTICE) file names each adapted file and reproduces the upstream NOTICE and the full Apache-2.0 license text, and each adapted source file states at its head that it was adapted and how Bake changed it. `bake` links the crate, so native archives carry that NOTICE.

| Project | Ported revision | License |
| --- | --- | --- |
| [OpenAI Codex](https://github.com/openai/codex) `codex-rs/mermaid`, Copyright 2025 OpenAI | `rust-v0.167.2` (`ce21072c90d32c40af7a55b60b2fb35c84a43a6a`) | Apache-2.0 |

## Rust YAML reader (`crates/bake-coding-agent`)

`bake-coding-agent` reads YAML, such as a skill's frontmatter, with serde-saphyr. These crates entered [`Cargo.lock`](Cargo.lock) with it and keep their own licenses.

| Crate | Version | License |
| --- | --- | --- |
| [`serde-saphyr`](https://github.com/bourumir-wyngs/serde-saphyr) | 2.0.0 | MIT OR Apache-2.0 |
| [`granit-parser`](https://github.com/bourumir-wyngs/granit-parser) | 2.0.0 | MIT OR Apache-2.0 |
| [`annotate-snippets`](https://github.com/rust-lang/annotate-snippets-rs) | 0.12.17 | MIT OR Apache-2.0 |
| [`anstyle`](https://github.com/rust-cli/anstyle) | 1.0.14 | MIT OR Apache-2.0 |
| [`arraydeque`](https://github.com/andylokandy/arraydeque) | 0.5.1 | MIT OR Apache-2.0 |
| [`encoding_rs_io`](https://github.com/BurntSushi/encoding_rs_io) | 0.1.8 | MIT OR Apache-2.0 |
| [`encoding_rs`](https://github.com/hsivonen/encoding_rs) | 0.8.42 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| [`core_detect`](https://github.com/thomcc/core_detect) | 1.0.0 | MIT OR Apache-2.0 |
| [`multiversion_no_op`](https://github.com/hsivonen/multiversion_no_op) | 1.0.0 | Apache-2.0 OR MIT |
| [`simdutf8`](https://github.com/rusticstuff/simdutf8) | 0.1.5 | MIT OR Apache-2.0 |

## Rust image codecs (`crates/bake-coding-agent`)

The `read` tool decodes, orients, resizes, and re-encodes images with the `image` crate, in place of the Photon WebAssembly module Pi uses, built with only its PNG, JPEG, GIF, WebP, and BMP codecs. These crates entered [`Cargo.lock`](Cargo.lock) with it and keep their own licenses. The lock also lists `zlib-rs` 0.6.8 (Zlib), an optional `flate2` backend that no Bake build enables.

| Crate | Version | License |
| --- | --- | --- |
| [`image`](https://github.com/image-rs/image) | 0.25.10 | MIT OR Apache-2.0 |
| [`png`](https://github.com/image-rs/image-png) | 0.18.1 | MIT OR Apache-2.0 |
| [`gif`](https://github.com/image-rs/image-gif) | 0.14.2 | MIT OR Apache-2.0 |
| [`image-webp`](https://github.com/image-rs/image-webp) | 0.2.4 | MIT OR Apache-2.0 |
| [`zune-jpeg`](https://github.com/etemesi254/zune-image) | 0.5.15 | MIT OR Apache-2.0 OR Zlib |
| [`zune-core`](https://github.com/etemesi254/zune-image) | 0.5.3 | MIT OR Apache-2.0 OR Zlib |
| [`moxcms`](https://github.com/awxkee/moxcms) | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| [`pxfm`](https://github.com/awxkee/pxfm) | 0.1.30 | BSD-3-Clause OR Apache-2.0 |
| [`fdeflate`](https://github.com/image-rs/fdeflate) | 0.3.7 | MIT OR Apache-2.0 |
| [`flate2`](https://github.com/rust-lang/flate2-rs) | 1.1.10 | MIT OR Apache-2.0 |
| [`miniz_oxide`](https://github.com/Frommi/miniz_oxide) | 0.8.9, 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| [`adler2`](https://github.com/oyvindln/adler2) | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| [`crc32fast`](https://github.com/srijs/rust-crc32fast) | 1.5.2 | MIT OR Apache-2.0 |
| [`simd-adler32`](https://github.com/mcountryman/simd-adler32) | 0.3.10 | MIT |
| [`weezl`](https://github.com/image-rs/weezl) | 0.1.12 | MIT OR Apache-2.0 |
| [`color_quant`](https://github.com/image-rs/color_quant) | 1.1.0 | MIT |
| [`bytemuck`](https://github.com/Lokathor/bytemuck) | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| [`byteorder-lite`](https://github.com/image-rs/byteorder-lite) | 0.1.0 | Unlicense OR MIT |
| [`quick-error`](https://github.com/tailhook/quick-error) | 2.0.1 | MIT OR Apache-2.0 |

## Rust process ownership (`crates/bake-coding-agent`)

`bake-coding-agent`'s bash tool owns each command's process tree through these crates, which were already in [`Cargo.lock`](Cargo.lock) for Tokio and keep their own licenses.

| Crate | Version | License | Use |
| --- | --- | --- | --- |
| [`libc`](https://github.com/rust-lang/libc) | 0.2.190 | MIT OR Apache-2.0 | `setsid` and `kill` on Unix |
| [`windows-sys`](https://github.com/microsoft/windows-rs) | 0.61.2 | MIT OR Apache-2.0 | job objects on Windows |

## Rust WebAssembly plugin host (`crates/bake-coding-agent`)

`bake-coding-agent` checks, compiles, and runs third-party plugin components with Wasmtime 50.0.0 in Bake's plugin helper process (`plugins::wasm`). These crates entered [`Cargo.lock`](Cargo.lock) with it, as normal, build, or procedural-macro dependencies of `bake` on at least one target (`wit-component` and `wit-parser` 0.261.0 run only inside Wasmtime's `bindgen!` macro), and keep their own licenses; most Bytecode Alliance crates are `Apache-2.0 WITH LLVM-exception`. The `wasmtime-internal-fiber` and `wasmtime` crates compile small C files with the target's C compiler. The guest SDK and the test tooling are not part of `bake`: `wit-bindgen` 0.63.0 with the 0.262.0 releases of `wasm-encoder`, `wasmparser`, `wit-parser`, `wit-component`, and `wasm-metadata`, and `wat` and `wast` 261.

| Crate | Version | License |
| --- | --- | --- |
| [`anyhow`](https://github.com/dtolnay/anyhow) | 1.0.104 | MIT OR Apache-2.0 |
| [`arbitrary`](https://github.com/rust-fuzz/arbitrary/) | 1.5.0 | MIT OR Apache-2.0 |
| [`async-trait`](https://github.com/dtolnay/async-trait) | 0.1.92 | MIT OR Apache-2.0 |
| [`block-buffer`](https://github.com/RustCrypto/utils) | 0.10.4 | MIT OR Apache-2.0 |
| [`cobs`](https://github.com/jamesmunns/cobs.rs) | 0.3.0 | MIT OR Apache-2.0 |
| [`cpufeatures`](https://github.com/RustCrypto/utils) | 0.2.17 | MIT OR Apache-2.0 |
| `cranelift-assembler-x64` | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| `cranelift-assembler-x64-meta` | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-bforest`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-bitset`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen-meta`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen-shared`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-control`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-entity`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-frontend`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-isle`](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift/isle) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-native`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-srcgen`](https://github.com/bytecodealliance/wasmtime) | 0.137.0 | Apache-2.0 WITH LLVM-exception |
| [`crypto-common`](https://github.com/RustCrypto/traits) | 0.1.7 | MIT OR Apache-2.0 |
| [`digest`](https://github.com/RustCrypto/traits) | 0.10.7 | MIT OR Apache-2.0 |
| [`fnv`](https://github.com/servo/rust-fnv) | 1.0.7 | Apache-2.0 OR MIT |
| [`futures`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`futures-io`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`futures-sink`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`generic-array`](https://github.com/fizyk20/generic-array.git) | 0.14.7 | MIT |
| [`gimli`](https://github.com/gimli-rs/gimli) | 0.33.0 | MIT OR Apache-2.0 |
| [`id-arena`](https://github.com/fitzgen/id-arena) | 2.3.0 | MIT OR Apache-2.0 |
| [`leb128fmt`](https://github.com/bluk/leb128fmt) | 0.1.0 | MIT OR Apache-2.0 |
| [`mach2`](https://github.com/JohnTitor/mach2) | 0.6.0 | BSD-2-Clause OR MIT OR Apache-2.0 |
| [`memfd`](https://github.com/lucab/memfd-rs) | 0.6.6 | MIT OR Apache-2.0 |
| [`object`](https://github.com/gimli-rs/object) | 0.40.0 | Apache-2.0 OR MIT |
| [`postcard`](https://github.com/jamesmunns/postcard) | 1.1.3 | MIT OR Apache-2.0 |
| [`pulley-interpreter`](https://github.com/bytecodealliance/wasmtime/tree/main/pulley) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`pulley-macros`](https://github.com/bytecodealliance/wasmtime/tree/main/pulley/macros) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`regalloc2`](https://github.com/bytecodealliance/regalloc2) | 0.15.2 | Apache-2.0 WITH LLVM-exception |
| [`rustix-linux-procfs`](https://github.com/sunfishcode/rustix-linux-procfs) | 0.1.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`sha2`](https://github.com/RustCrypto/hashes) | 0.10.9 | MIT OR Apache-2.0 |
| [`target-lexicon`](https://github.com/bytecodealliance/target-lexicon) | 0.13.5 | Apache-2.0 WITH LLVM-exception |
| [`termcolor`](https://github.com/BurntSushi/termcolor) | 1.4.1 | Unlicense OR MIT |
| [`tracing-attributes`](https://github.com/tokio-rs/tracing) | 0.1.31 | MIT |
| [`typenum`](https://github.com/paholg/typenum) | 1.20.1 | MIT OR Apache-2.0 |
| [`version_check`](https://github.com/SergioBenitez/version_check) | 0.9.5 | MIT OR Apache-2.0 |
| [`wasm-encoder`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasm-metadata`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-metadata) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmparser`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmparser) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmprinter`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmprinter) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmtime`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-environ`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-component-macro`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-component-util`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| `wasmtime-internal-core` | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-cranelift`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-fiber`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-jit-icache-coherence`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-unwinder`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-versioned-export-macros`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-wit-bindgen`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-wasi`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-wasi-io`](https://github.com/bytecodealliance/wasmtime) | 50.0.0 | Apache-2.0 WITH LLVM-exception |
| [`wit-component`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-component) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wit-parser`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-parser) | 0.261.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |

## Ported source (`plugins/guard`)

The `guard` plugin component ports Pi's `protected-paths` extension example. Its [`NOTICE`](plugins/guard/NOTICE) file reproduces the license text.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/coding-agent/examples/extensions/protected-paths.ts`, Copyright (c) 2025 Mario Zechner | v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`) | MIT |

## Adapted designs

Earlier terminal turn header revisions included a Braille port (`GLYPH_PULSE`) of the "Glyph Pulse" loader (`dotm-square-9`) from [dot-matrix](https://dotmatrix.zzzzshawn.cloud) ([source](https://github.com/zzzzshawn/matrix)), copyright (c) 2026 dot-matrix contributors, all rights reserved. That adaptation is covered by the project's custom license, which permits use in commercial and non-commercial products and prohibits, without written permission, publishing or distributing its components as standalone reusable components or as part of another component library, and selling or sublicensing them as a standalone offering. Bake's MIT license does not cover this adaptation; do not extract it into a reusable component or library.
