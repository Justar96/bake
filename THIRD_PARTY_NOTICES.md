# Third-Party Notices

Bake is derived from the MIT-licensed DeepSeek Harness runtime. The notices below preserve the upstream attributions; they are not a current inventory of Bake's installed packages. Each project remains under its own license, and nothing in this file changes those terms.

Bake's resolved dependency versions are recorded in [`bun.lock`](bun.lock). Review the licenses supplied by those packages before distributing Bake artifacts. Vendored source retains its original license files.

## Vendored source (`vendor/`)

The Cordis framework and its foundation libraries are source-vendored into this repository rather than consumed from npm, and republished under the `@deepseek-ai` scope. All are MIT-licensed; each directory preserves its upstream `LICENSE` file. Exact upstream commits and local modifications are recorded in [`vendor/README.md`](vendor/README.md).

| Package | Upstream name | Source | License |
| --- | --- | --- | --- |
| `@deepseek-ai/cosmokit` | `cosmokit` | [vendor/cosmokit](vendor/cosmokit/) | MIT |
| `@deepseek-ai/schemastery` | `schemastery` | [vendor/schemastery](vendor/schemastery/) | MIT |
| `@deepseek-ai/cordis` | `cordis` | [vendor/cordis](vendor/cordis/) | MIT |
| `@deepseek-ai/cordis-plugin-loader` | `@cordisjs/plugin-loader` | [vendor/loader](vendor/loader/) | MIT |
| `@deepseek-ai/cordis-plugin-include` | `@cordisjs/plugin-include` | [vendor/include](vendor/include/) | MIT |
| `@deepseek-ai/cordis-plugin-group` | `@cordisjs/plugin-group` | [vendor/group](vendor/group/) | MIT |
| `@deepseek-ai/cordis-plugin-timer` | `@cordisjs/plugin-timer` | [vendor/timer](vendor/timer/) | MIT |
| `@deepseek-ai/cordis-plugin-hmr` | `@cordisjs/plugin-hmr` | [vendor/hmr](vendor/hmr/) | MIT |
| `@deepseek-ai/cordis-plugin-logger-console` | `@cordisjs/plugin-logger-console` | [vendor/logger-console](vendor/logger-console/) | MIT |

## Runtime npm dependencies

External packages installed for runtime use or distributed inside the prebuilt browser artifacts. Browser inputs are resolved through the shipping tsdown and Vite configurations, independently of npm dependency sections. The tier covers every plugin a user can mount from `cordis.yml` — not only what the `dsh` CLI, Web UI, and Python SDK runtime load by default.

| Package | License |
| --- | --- |
| [`@agentclientprotocol/sdk`](https://github.com/agentclientprotocol/typescript-sdk) | Apache-2.0 |
| [`@anthropic-ai/claude-agent-sdk`](https://github.com/anthropics/claude-agent-sdk-typescript) | SEE LICENSE IN README.md |
| [`@anthropic-ai/sdk`](https://github.com/anthropics/anthropic-sdk-typescript) | MIT |
| [`@babel/code-frame`](https://github.com/babel/babel) | MIT |
| [`@browserbasehq/stagehand`](https://github.com/browserbase/stagehand) | MIT |
| [`@deepseek-ai/libreoffice-kit`](https://github.com/deepseek-harness/libreoffice-kit) | MPL-2.0 |
| [`@earendil-works/pi-ai`](https://github.com/earendil-works/pi) | MIT |
| [`@earendil-works/pi-codemode`](https://github.com/earendil-works/pi) | MIT |
| [`@earendil-works/pi-mcp`](https://github.com/earendil-works/pi) | MIT |
| [`@eslint-community/regexpp`](https://github.com/eslint-community/regexpp) | MIT |
| [`@joplin/turndown-plugin-gfm`](https://github.com/laurent22/joplin-turndown-plugin-gfm) | MIT |
| [`@jridgewell/gen-mapping`](https://github.com/jridgewell/sourcemaps) | MIT |
| [`@lexical/history`](https://github.com/facebook/lexical) | MIT |
| [`@lexical/plain-text`](https://github.com/facebook/lexical) | MIT |
| [`@lexical/text`](https://github.com/facebook/lexical) | MIT |
| [`@lexical/utils`](https://github.com/facebook/lexical) | MIT |
| [`@modelcontextprotocol/client`](https://github.com/modelcontextprotocol/typescript-sdk) | MIT |
| [`@modelcontextprotocol/sdk`](https://github.com/modelcontextprotocol/typescript-sdk) | MIT |
| [`@noble/hashes`](https://github.com/paulmillr/noble-hashes) | MIT |
| [`@octokit/webhooks`](https://github.com/octokit/webhooks.js) | MIT |
| [`@openai/codex`](https://github.com/openai/codex) | Apache-2.0 |
| [`@opentelemetry/api`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@opentelemetry/api-logs`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@opentelemetry/exporter-logs-otlp-http`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@opentelemetry/otlp-exporter-base`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@opentelemetry/resources`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@opentelemetry/sdk-logs`](https://github.com/open-telemetry/opentelemetry-js) | Apache-2.0 |
| [`@playwright/mcp`](https://github.com/microsoft/playwright-mcp) | Apache-2.0 |
| [`@puppeteer/browsers`](https://github.com/puppeteer/puppeteer/tree/main/packages/browsers) | Apache-2.0 |
| [`@shikijs/langs`](https://github.com/shikijs/shiki) | MIT |
| [`@standard-schema/spec`](https://github.com/standard-schema/standard-schema) | MIT |
| [`@tanstack/react-virtual`](https://github.com/TanStack/virtual) | MIT |
| [`@trycua/cua-driver`](https://github.com/trycua/cua) | MIT |
| [`@vscode/ripgrep`](https://github.com/microsoft/vscode-ripgrep) | MIT |
| [`@xterm/addon-fit`](https://github.com/xtermjs/xterm.js/tree/master/addons/addon-fit) | MIT |
| [`@xterm/addon-serialize`](https://github.com/xtermjs/xterm.js/tree/master/addons/addon-serialize) | MIT |
| [`@xterm/headless`](https://github.com/xtermjs/xterm.js) | MIT |
| [`@xterm/xterm`](https://github.com/xtermjs/xterm.js) | MIT |
| [`@yarnpkg/parsers`](https://github.com/yarnpkg/berry) | BSD-2-Clause |
| [`acorn`](https://github.com/acornjs/acorn) | MIT |
| [`ajv`](https://github.com/ajv-validator/ajv) | MIT |
| [`anser`](https://github.com/IonicaBizau/anser) | MIT |
| [`buffer`](https://github.com/feross/buffer) | MIT |
| [`chokidar`](https://github.com/paulmillr/chokidar) | MIT |
| [`chrome-devtools-mcp`](https://github.com/ChromeDevTools/chrome-devtools-mcp) | Apache-2.0 |
| [`clsx`](https://github.com/lukeed/clsx) | MIT |
| [`commander`](https://github.com/tj/commander.js) | MIT |
| [`compression`](https://github.com/expressjs/compression) | MIT |
| [`diff`](https://github.com/kpdecker/jsdiff) | BSD-3-Clause |
| [`eventsource-parser`](https://github.com/rexxars/eventsource-parser) | MIT |
| [`execa`](https://github.com/sindresorhus/execa) | MIT |
| [`fflate`](https://github.com/101arrowz/fflate) | MIT |
| [`immer`](https://github.com/immerjs/immer) | MIT |
| [`ink`](https://github.com/vadimdemedes/ink) | MIT |
| [`ipaddr.js`](https://github.com/whitequark/ipaddr.js) | MIT |
| [`js-yaml`](https://github.com/nodeca/js-yaml) | MIT |
| [`katex`](https://github.com/KaTeX/KaTeX) | MIT |
| [`koffi`](https://github.com/Koromix/koffi) | MIT |
| [`lexical`](https://github.com/facebook/lexical) | MIT |
| [`mdast-util-from-markdown`](https://github.com/syntax-tree/mdast-util-from-markdown) | MIT |
| [`mdast-util-gfm`](https://github.com/syntax-tree/mdast-util-gfm) | MIT |
| [`mdast-util-math`](https://github.com/syntax-tree/mdast-util-math) | MIT |
| [`micromark-core-commonmark`](https://github.com/micromark/micromark/tree/main/packages/micromark-core-commonmark) | MIT |
| [`micromark-extension-gfm`](https://github.com/micromark/micromark-extension-gfm) | MIT |
| [`micromark-extension-math`](https://github.com/micromark/micromark-extension-math) | MIT |
| [`micromark-factory-space`](https://github.com/micromark/micromark/tree/main/packages/micromark-factory-space) | MIT |
| [`micromark-util-character`](https://github.com/micromark/micromark/tree/main/packages/micromark-util-character) | MIT |
| [`micromark-util-classify-character`](https://github.com/micromark/micromark/tree/main/packages/micromark-util-classify-character) | MIT |
| [`micromark-util-sanitize-uri`](https://github.com/micromark/micromark/tree/main/packages/micromark-util-sanitize-uri) | MIT |
| [`micromark-util-symbol`](https://github.com/micromark/micromark/tree/main/packages/micromark-util-symbol) | MIT |
| [`mime-types`](https://github.com/jshttp/mime-types) | MIT |
| [`negotiator`](https://github.com/jshttp/negotiator) | MIT |
| [`node-addon-require-builtin`](https://www.npmjs.com/package/node-addon-require-builtin) | MIT |
| [`node-pty`](https://github.com/microsoft/node-pty) | MIT |
| [`open`](https://github.com/sindresorhus/open) | MIT |
| [`pdfjs-dist`](https://github.com/mozilla/pdf.js) | Apache-2.0 |
| [`picomatch`](https://github.com/micromatch/picomatch) | MIT |
| [`quickjs-wasi`](https://github.com/vercel-labs/quickjs-wasi) | MIT |
| [`react`](https://github.com/react/react) | MIT |
| [`react-dom`](https://github.com/facebook/react) | MIT |
| [`readable-stream`](https://github.com/nodejs/readable-stream) | MIT |
| [`resolve.exports`](https://github.com/lukeed/resolve.exports) | MIT |
| [`sharp`](https://github.com/lovell/sharp) | Apache-2.0 |
| [`shiki`](https://github.com/shikijs/shiki) | MIT |
| [`simple-icons`](https://github.com/simple-icons/simple-icons) | CC0-1.0 |
| [`supports-color`](https://github.com/chalk/supports-color) | MIT |
| [`tsx`](https://github.com/privatenumber/tsx) | MIT |
| [`turndown`](https://github.com/mixmark-io/turndown) | MIT |
| [`typescript`](https://github.com/microsoft/TypeScript) | Apache-2.0 |
| [`undici`](https://github.com/nodejs/undici) | MIT |
| [`use-sync-external-store`](https://github.com/facebook/react) | MIT |
| [`wrap-ansi`](https://github.com/chalk/wrap-ansi) | MIT |
| [`ws`](https://github.com/websockets/ws) | MIT |
| [`yaml`](https://github.com/eemeli/yaml) | ISC |
| [`zod`](https://github.com/colinhacks/zod) | MIT |
| [`zustand`](https://github.com/pmndrs/zustand) | MIT |

Bun applies local patches to the following packages at install time, so shipped artifacts carry modified copies; each patch file is the complete record of the modification:

- `node-pty@1.2.0-beta.15` — [`patches/node-pty@1.2.0-beta.15.patch`](patches/node-pty@1.2.0-beta.15.patch)

## Official Claude Code platform payloads

The project owner authorizes distribution of every version of the official `@anthropic-ai/claude-agent-sdk` package and the official Claude Code CLI/platform payloads that each version declares through `optionalDependencies`. This identity-scoped authorization does not classify their declared terms as permissive and does not cover any unrelated runtime package; version, declared-license, and payload-set changes still require the ordinary dependency, lockfile, compatibility, terms, and notices review.

The installed SDK 0.3.263 declares the following optional platform packages. Each carries the official Claude Code 2.1.263 executable; the package identities and versions come from the SDK manifest, while the declared license field is verified against the platform payload installed for the current host.

| Optional platform package | Version | Declared license |
| --- | --- | --- |
| [`@anthropic-ai/claude-agent-sdk-darwin-arm64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-darwin-arm64) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-darwin-x64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-darwin-x64) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-linux-arm64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-linux-arm64) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-linux-arm64-musl`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-linux-arm64-musl) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-linux-x64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-linux-x64) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-linux-x64-musl`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-linux-x64-musl) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-win32-arm64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-win32-arm64) | 0.3.263 | SEE LICENSE IN LICENSE.md |
| [`@anthropic-ai/claude-agent-sdk-win32-x64`](https://www.npmjs.com/package/@anthropic-ai/claude-agent-sdk-win32-x64) | 0.3.263 | SEE LICENSE IN LICENSE.md |


## LibreOffice conversion kit

`@deepseek-ai/libreoffice-kit`, `@deepseek-ai/libreoffice-kit-wasm`, `@deepseek-ai/libreoffice-kit-darwin-arm64`, `@deepseek-ai/libreoffice-kit-darwin-x64`, `@deepseek-ai/libreoffice-kit-win32-arm64`, `@deepseek-ai/libreoffice-kit-win32-x64` declare MPL-2.0, which remains outside the permissive-license allowlist; the notices check accepts only these package identities at those terms. The [distribution decision](.agents/notes/implemented/architecture/2026-09-14-independent-libreoffice-kit.md) records the source obligations.

The [kit repository](https://github.com/deepseek-harness/libreoffice-kit) supplies the corresponding LibreOffice source pin, modifications, build instructions, Node API, and artifact validation. Its engine packages retain their license and third-party notices; the Node API retains its MPL-2.0 declaration and NOTICE. Recipients must have access to those corresponding sources and notices.


## Development-only npm dependencies

External packages **directly declared** for development, tests, types, or tooling, without a runtime installation or browser-build relationship. A package here may still be pulled in transitively by a runtime dependency — `bun.lock` is the authority on that full closure.

| Package | License |
| --- | --- |
| [`@lexical/headless`](https://github.com/facebook/lexical) | MIT |
| [`@modelcontextprotocol/node`](https://github.com/modelcontextprotocol/typescript-sdk) | MIT |
| [`@modelcontextprotocol/server`](https://github.com/modelcontextprotocol/typescript-sdk) | MIT |
| [`@modelcontextprotocol/server-everything`](https://github.com/modelcontextprotocol/servers) | MIT / Apache-2.0 |
| [`@modelcontextprotocol/server-filesystem`](https://github.com/modelcontextprotocol/servers) | MIT / Apache-2.0 |
| [`@stylistic/eslint-plugin`](https://github.com/eslint-stylistic/eslint-stylistic) | MIT |
| [`@testing-library/dom`](https://github.com/testing-library/dom-testing-library) | MIT |
| [`@testing-library/react`](https://github.com/testing-library/react-testing-library) | MIT |
| [`@types/babel__code-frame`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/bun`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/compression`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/js-yaml`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/jsdom`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/mdast`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/mime-types`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/negotiator`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/node`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/picomatch`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/react`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/react-dom`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/readable-stream`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/spdx-expression-parse`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/turndown`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/use-sync-external-store`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@types/ws`](https://github.com/DefinitelyTyped/DefinitelyTyped) | MIT |
| [`@vitejs/plugin-react`](https://github.com/vitejs/vite-plugin-react) | MIT |
| [`@vitest/coverage-v8`](https://github.com/vitest-dev/vitest) | MIT |
| [`@vitest/spy`](https://github.com/vitest-dev/vitest) | MIT |
| [`@yarnpkg/cli-dist`](https://github.com/yarnpkg/berry) | BSD-2-Clause |
| [`esbuild`](https://github.com/evanw/esbuild) | MIT |
| [`eslint-plugin-sonarjs`](https://github.com/SonarSource/SonarJS) | LGPL-3.0-only |
| [`fast-check`](https://github.com/dubzzz/fast-check) | MIT |
| [`http-server`](https://github.com/http-party/http-server) | MIT |
| [`ink-testing-library`](https://github.com/vadimdemedes/ink-testing-library) | MIT |
| [`istanbul-lib-report`](https://github.com/istanbuljs/istanbuljs) | BSD-3-Clause |
| [`jscpd`](https://github.com/kucherenko/jscpd) | MIT |
| [`jsdom`](https://github.com/jsdom/jsdom) | MIT |
| [`lefthook`](https://github.com/evilmartians/lefthook) | MIT |
| [`lightningcss`](https://github.com/parcel-bundler/lightningcss) | MPL-2.0 |
| [`micromark-util-types`](https://github.com/micromark/micromark/tree/main/packages/micromark-util-types) | MIT |
| [`oxlint`](https://github.com/oxc-project/oxc) | MIT |
| [`oxlint-tsgolint`](https://github.com/oxc-project/tsgolint) | MIT |
| [`playwright`](https://github.com/microsoft/playwright) | Apache-2.0 |
| [`publint`](https://github.com/publint/publint) | MIT |
| [`smol-toml`](https://github.com/squirrelchat/smol-toml) | BSD-3-Clause |
| [`spdx-expression-parse`](https://github.com/jslicense/spdx-expression-parse.js) | MIT |
| [`tsdown`](https://github.com/rolldown/tsdown) | MIT |
| [`typescript-language-server`](https://github.com/typescript-language-server/typescript-language-server) | Apache-2.0 |
| [`vite`](https://github.com/vitejs/vite) | MIT |
| [`vite-tsconfig-paths`](https://github.com/aleclarson/vite-tsconfig-paths) | MIT |
| [`vitest`](https://github.com/vitest-dev/vitest) | MIT |

`eslint-plugin-sonarjs` (LGPL-3.0-only) and `lightningcss` (MPL-2.0) run only as development tooling; their code is not linked into or distributed with any DeepSeek Harness artifact.

## Python SDK dependencies (`python/`)

Direct dependencies of the `pyproject.toml` manifests, plus `uv` as the development workflow tool.

| Package | License | Role |
| --- | --- | --- |
| [`hatchling`](https://github.com/pypa/hatch) | MIT | build backend |
| [`pydantic`](https://github.com/pydantic/pydantic) | MIT | runtime dependency of `deepseek-harness-sdk` |
| [`pytest`](https://github.com/pytest-dev/pytest) | MIT | test-only |
| [`uv`](https://github.com/astral-sh/uv) | MIT / Apache-2.0 | development workflow tool |

## Ported source (`rust/crates/bake-ai`)

The Rust `bake-ai` crate ports code from these MIT-licensed projects. Its [`NOTICE`](rust/crates/bake-ai/NOTICE) file reproduces both license texts, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/ai`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |
| [partial-json](https://github.com/promplate/partial-json-parser-js), Copyright (c) 2023 Promplate Dev Team | 0.1.7 | MIT |

The crate's Cargo dependencies are locked in [`rust/Cargo.lock`](rust/Cargo.lock) and keep their own licenses.

## Ported source (`rust/crates/bake-agent`)

The Rust `bake-agent` crate ports Pi's agent loop and stateful agent, and the tool-argument validation of Pi's `ai` package. Its [`NOTICE`](rust/crates/bake-agent/NOTICE) file reproduces the license text, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/agent` and `packages/ai/src/utils/validation.ts`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |

## Ported source (`rust/crates/bake-coding-agent`)

The Rust `bake-coding-agent` crate ports Pi's coding-agent package, and the line diff and patch of jsdiff that Pi's edit tool uses. Its [`NOTICE`](rust/crates/bake-coding-agent/NOTICE) file reproduces both license texts, and its module documentation names each source file.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/coding-agent`, Copyright (c) 2025 Mario Zechner | v1.1.0 (`abe508e1b89912adde45528136c3221eb69acdd7`) | MIT |
| [jsdiff](https://github.com/kpdecker/jsdiff), Copyright (c) 2009-2015 Kevin Decker | 8.0.4 | BSD-3-Clause |

## Rust YAML reader (`rust/crates/bake-coding-agent`)

`bake-coding-agent` reads the 0.3 Bake home's `settings.yaml` and `.credentials.yaml` with serde-saphyr. These crates entered [`rust/Cargo.lock`](rust/Cargo.lock) with it and keep their own licenses.

| Crate | Version | License |
| --- | --- | --- |
| [`serde-saphyr`](https://github.com/bourumir-wyngs/serde-saphyr) | 1.3.0 | MIT OR Apache-2.0 |
| [`granit-parser`](https://github.com/bourumir-wyngs/granit-parser) | 1.3.0 | MIT OR Apache-2.0 |
| [`annotate-snippets`](https://github.com/rust-lang/annotate-snippets-rs) | 0.12.16 | MIT OR Apache-2.0 |
| [`anstyle`](https://github.com/rust-cli/anstyle) | 1.0.14 | MIT OR Apache-2.0 |
| [`arraydeque`](https://github.com/andylokandy/arraydeque) | 0.5.1 | MIT OR Apache-2.0 |
| [`encoding_rs_io`](https://github.com/BurntSushi/encoding_rs_io) | 0.1.8 | MIT OR Apache-2.0 |
| [`encoding_rs`](https://github.com/hsivonen/encoding_rs) | 0.8.42 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| [`core_detect`](https://github.com/thomcc/core_detect) | 1.0.0 | MIT OR Apache-2.0 |
| [`multiversion_no_op`](https://github.com/hsivonen/multiversion_no_op) | 1.0.0 | Apache-2.0 OR MIT |
| [`simdutf8`](https://github.com/rusticstuff/simdutf8) | 0.1.5 | MIT OR Apache-2.0 |

## Rust image codecs (`rust/crates/bake-coding-agent`)

The `read` tool decodes, orients, resizes, and re-encodes images with the `image` crate, in place of the Photon WebAssembly module Pi uses, built with only its PNG, JPEG, GIF, WebP, and BMP codecs. These crates entered [`rust/Cargo.lock`](rust/Cargo.lock) with it and keep their own licenses. The lock also lists `zlib-rs` 0.6.8 (Zlib), an optional `flate2` backend that no Bake build enables.

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

## Rust process ownership (`rust/crates/bake-coding-agent`)

`bake-coding-agent`'s bash tool owns each command's process tree through these crates, which were already in [`rust/Cargo.lock`](rust/Cargo.lock) for Tokio and keep their own licenses.

| Crate | Version | License | Use |
| --- | --- | --- | --- |
| [`libc`](https://github.com/rust-lang/libc) | 0.2.190 | MIT OR Apache-2.0 | `setsid` and `kill` on Unix |
| [`windows-sys`](https://github.com/microsoft/windows-rs) | 0.61.2 | MIT OR Apache-2.0 | job objects on Windows |

## Rust WebAssembly plugin host (`rust/crates/bake-coding-agent`)

`bake-coding-agent` checks, compiles, and runs third-party plugin components with Wasmtime 49.0.3 in Bake's plugin helper process (`plugins::wasm`). These crates entered [`rust/Cargo.lock`](rust/Cargo.lock) with it, as normal, build, or procedural-macro dependencies of `bake-rs` on at least one target (`wit-component` and `wit-parser` 0.258.3 run only inside Wasmtime's `bindgen!` macro), and keep their own licenses; most Bytecode Alliance crates are `Apache-2.0 WITH LLVM-exception`. The `wasmtime-internal-fiber` and `wasmtime` crates compile small C files with the target's C compiler. The guest SDK and the test tooling are not part of `bake-rs`: `wit-bindgen` 0.63.0 with the 0.262.0 releases of `wasm-encoder`, `wasmparser`, `wit-parser`, `wit-component`, and `wasm-metadata`, and `wat` and `wast` 258.

| Crate | Version | License |
| --- | --- | --- |
| [`ambient-authority`](https://github.com/sunfishcode/ambient-authority) | 0.0.2 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`anyhow`](https://github.com/dtolnay/anyhow) | 1.0.104 | MIT OR Apache-2.0 |
| [`arbitrary`](https://github.com/rust-fuzz/arbitrary/) | 1.5.0 | MIT OR Apache-2.0 |
| [`async-trait`](https://github.com/dtolnay/async-trait) | 0.1.92 | MIT OR Apache-2.0 |
| [`block-buffer`](https://github.com/RustCrypto/utils) | 0.10.4 | MIT OR Apache-2.0 |
| [`cap-primitives`](https://github.com/bytecodealliance/cap-std) | 4.0.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`cobs`](https://github.com/jamesmunns/cobs.rs) | 0.3.0 | MIT OR Apache-2.0 |
| [`cpufeatures`](https://github.com/RustCrypto/utils) | 0.2.17 | MIT OR Apache-2.0 |
| `cranelift-assembler-x64` | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| `cranelift-assembler-x64-meta` | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-bforest`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-bitset`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen-meta`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-codegen-shared`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-control`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-entity`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-frontend`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-isle`](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift/isle) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-native`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`cranelift-srcgen`](https://github.com/bytecodealliance/wasmtime) | 0.136.3 | Apache-2.0 WITH LLVM-exception |
| [`crypto-common`](https://github.com/RustCrypto/traits) | 0.1.7 | MIT OR Apache-2.0 |
| [`digest`](https://github.com/RustCrypto/traits) | 0.10.7 | MIT OR Apache-2.0 |
| [`fnv`](https://github.com/servo/rust-fnv) | 1.0.7 | Apache-2.0 OR MIT |
| [`fs-set-times`](https://github.com/bytecodealliance/fs-set-times) | 0.20.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`futures`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`futures-io`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`futures-sink`](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [`generic-array`](https://github.com/fizyk20/generic-array.git) | 0.14.7 | MIT |
| [`gimli`](https://github.com/gimli-rs/gimli) | 0.33.0 | MIT OR Apache-2.0 |
| [`id-arena`](https://github.com/fitzgen/id-arena) | 2.3.0 | MIT OR Apache-2.0 |
| [`io-extras`](https://github.com/sunfishcode/io-extras) | 0.19.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`io-lifetimes`](https://github.com/sunfishcode/io-lifetimes) | 2.0.4, 3.0.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`leb128fmt`](https://github.com/bluk/leb128fmt) | 0.1.0 | MIT OR Apache-2.0 |
| [`mach2`](https://github.com/JohnTitor/mach2) | 0.6.0 | BSD-2-Clause OR MIT OR Apache-2.0 |
| [`maybe-owned`](https://github.com/rustonaut/maybe-owned) | 0.3.4 | MIT OR Apache-2.0 |
| [`memfd`](https://github.com/lucab/memfd-rs) | 0.6.6 | MIT OR Apache-2.0 |
| [`object`](https://github.com/gimli-rs/object) | 0.40.0 | Apache-2.0 OR MIT |
| [`postcard`](https://github.com/jamesmunns/postcard) | 1.1.3 | MIT OR Apache-2.0 |
| [`pulley-interpreter`](https://github.com/bytecodealliance/wasmtime/tree/main/pulley) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`pulley-macros`](https://github.com/bytecodealliance/wasmtime/tree/main/pulley/macros) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`regalloc2`](https://github.com/bytecodealliance/regalloc2) | 0.15.2 | Apache-2.0 WITH LLVM-exception |
| [`rustix-linux-procfs`](https://github.com/sunfishcode/rustix-linux-procfs) | 0.1.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`sha2`](https://github.com/RustCrypto/hashes) | 0.10.9 | MIT OR Apache-2.0 |
| [`target-lexicon`](https://github.com/bytecodealliance/target-lexicon) | 0.13.5 | Apache-2.0 WITH LLVM-exception |
| [`termcolor`](https://github.com/BurntSushi/termcolor) | 1.4.1 | Unlicense OR MIT |
| [`tracing-attributes`](https://github.com/tokio-rs/tracing) | 0.1.31 | MIT |
| [`typenum`](https://github.com/paholg/typenum) | 1.20.1 | MIT OR Apache-2.0 |
| [`version_check`](https://github.com/SergioBenitez/version_check) | 0.9.5 | MIT OR Apache-2.0 |
| [`wasm-encoder`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasm-metadata`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-metadata) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmparser`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmparser) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmprinter`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmprinter) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wasmtime`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-environ`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-component-macro`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-component-util`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| `wasmtime-internal-core` | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-cranelift`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-fiber`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-jit-icache-coherence`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-unwinder`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-versioned-export-macros`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-internal-wit-bindgen`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-wasi`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`wasmtime-wasi-io`](https://github.com/bytecodealliance/wasmtime) | 49.0.3 | Apache-2.0 WITH LLVM-exception |
| [`winx`](https://github.com/sunfishcode/winx) | 0.36.4 | Apache-2.0 WITH LLVM-exception |
| [`wit-component`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-component) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [`wit-parser`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-parser) | 0.258.3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |

## Ported source (`rust/plugins/guard`)

The `guard` plugin component ports Pi's `protected-paths` extension example. Its [`NOTICE`](rust/plugins/guard/NOTICE) file reproduces the license text.

| Project | Ported revision | License |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi) `packages/coding-agent/examples/extensions/protected-paths.ts`, Copyright (c) 2025 Mario Zechner | v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`) | MIT |

## Adapted designs

Earlier terminal turn header revisions included a Braille port (`GLYPH_PULSE`) of the "Glyph Pulse" loader (`dotm-square-9`) from [dot-matrix](https://dotmatrix.zzzzshawn.cloud) ([source](https://github.com/zzzzshawn/matrix)), copyright (c) 2026 dot-matrix contributors, all rights reserved. That adaptation is covered by the project's custom license, which permits use in commercial and non-commercial products and prohibits, without written permission, publishing or distributing its components as standalone reusable components or as part of another component library, and selling or sublicensing them as a standalone offering. Bake's MIT license does not cover this adaptation; do not extract it into a reusable component or library.

## First-party native packages

`@deepseek-ai/node-addon-system` (and its platform packages) is built and released from this repository under BSD 3-Clause. It is listed here for completeness; it is first-party, not third-party.
