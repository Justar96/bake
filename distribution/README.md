# Bake releases and the download service

Bake is published as native archives on a signed channel that https://bake.justar.dev serves. The [installers](#installers) install from it and `bake update` updates and rolls back through it. Releases are built, signed, and published by the [release workflow](#release-with-github-actions), whose steps are `bake-release` ([`crates/bake-release`](../crates/bake-release/)), a build tool that is never shipped. Every artifact it writes is checked by `bake-update`, the code a user's `bake update` runs. Bake 0.4 publishes `linux-x64` and `linux-arm64`; macOS and Windows follow in 0.4.x ([D47](../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)).

## Contents

- [The native route](#the-native-route)
- [The native signing key](#the-native-signing-key)
- [The archive](#the-archive)
- [Dependency licenses](#dependency-licenses)
- [The glibc floor](#the-glibc-floor)
- [Installers](#installers)
- [Release with GitHub Actions](#release-with-github-actions)
- [The download service](#the-download-service)
- [Release steps by hand](#release-steps-by-hand)

## The native route

| Route | What it serves |
|---|---|
| `/native/stable.json` | the native release manifest |
| `/native/stable.json.sig` | base64 Ed25519 signature over that file's exact bytes |
| `/native/releases/<version>/<file>` | the archives, redirected to the version's GitHub release |
| `/install.sh`, `/install.ps1` | the installers |
| `/latest.json`, `/latest.json.sig` | the frozen 0.3 route, unchanged since the final 0.3 release, which 0.3 installs keep reading |

```json
{"schema":"bake-native-release/1","channel":"stable","version":"0.4.2","published":"2026-10-25T00:00:00Z","artifacts":{"linux-arm64":{"file":"bake-v0.4.2-linux-arm64.tar.gz","sha256":"<64 lowercase hex>","size":123},"linux-x64":{"file":"bake-v0.4.2-linux-x64.tar.gz","sha256":"<64 lowercase hex>","size":123}}}
```

The manifest has exactly those members.
- `version` is canonical SemVer with no build metadata, ordered by SemVer precedence.
- `published` is UTC with second precision.
- `artifacts` names release platforms from the five the grammar keeps: `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, `win32-x64`. A 0.4.x release can add one without a new schema.
- Each artifact's `file` is `bake-v<version>-<platform>.tar.gz`, with a lowercase hex `sha256` and a positive `size`.

Any unknown member is refused. The canonical bytes are those members in that order, the artifacts in platform order, on one line with a trailing newline. The signature covers those exact bytes, so a reformatted manifest no longer verifies. `bake_update::manifest` parses them; `bake-release`'s `manifest` module writes them and parses them back before signing.

A channel never moves backward and a version is never republished with other bytes. `bake-release assemble` refuses a version that is not newer than both the staged `public/native/stable.json` and the live channel; the live check is skipped with `--offline`, and a 404 there means nothing is published yet.

## The native signing key

`crates/bake-update/native-release-key.pub` holds the native public key, base64 DER SubjectPublicKeyInfo, which every `bake` embeds. These copies repeat it, and a `bake-release` test holds all of them equal:
- `distribution/host/native-release-key.pub`;
- the `native_release_key` line of `install.sh`;
- the `$nativeReleaseKey` line of `install.ps1`.

No 0.3 updater trusts this key. The private half is the release owner's PKCS#8 PEM file. It never enters the repository: the workflow reads it from the `release` environment's `BAKE_NATIVE_RELEASE_SIGNING_KEY` secret. Keep an offline copy, because installs trust this key alone. Rotating it needs a native release, signed with the old key, that trusts the new one.

`bake-release assemble` signs with `--key-file <pem>` or `BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE`, and refuses a key whose public half is not the committed one. `--ephemeral-key` signs a check with a throwaway key and leaves its public half beside the archives as `ephemeral-native-release-key.pub`. The deploy's gate accepts only the committed key, so a throwaway signature never deploys.

## The archive

`bake-release pack` archives the release `bake` this checkout built (`cargo build --release --locked -p bake-cli`). It writes `.artifacts/bake-native-release/<version>/bake-v<version>-<platform>.tar.gz` for this host, and refuses a host outside the release platforms. `--binary` names another build, and `--out` another directory, which must be outside the repository or under `.artifacts/bake-native-release/`.

The archive holds:
- `bin/bake`, mode 0755;
- `LICENSE` and `THIRD_PARTY_NOTICES.md`;
- `notices/<crate>/NOTICE` for each Bake crate `bake-cli` links, which keep Pi's MIT notice for the ported crates;
- `licenses/inventory.json` and `licenses/<platform>/THIRD-PARTY-LICENSES.txt` from the [dependency licenses](#dependency-licenses), generated for this run.

Directories are 0755 and other files 0644, all owned by 0 and dated at the commit (`SOURCE_DATE_EPOCH`, else the commit time), so a commit's archive has the same bytes on a rerun.

The pack checks, in this order:
1. The binary runs `--version` and `--self-check` in a private empty home and `PATH`, which must stay empty, for at most 30 seconds each. The record must name the workspace version and this platform, and embed exactly the documentation in `crates/bake-cli/docs/*.md`.
2. The archive is read back: exactly the staged files, no links, nothing writable by group or others, and an executable binary.
3. It is unpacked by `bake_update::archive::extract`, the installer's own reader, compared byte for byte, and its binary checked again.

Only then is it renamed into place.

## Dependency licenses

`bake-release licenses [--out <dir>]` generates the third-party license bundle. `pack` generates it for each archive itself. It needs the toolchain `rust-toolchain.toml` pins and cargo-about 0.9.2, installed with `cargo install cargo-about --version =0.9.2 --locked --features cli` or named with `BAKE_CARGO_ABOUT`; any other version is refused. It runs `cargo fetch --locked`, the only step that uses the network, then works offline.

Selection uses Cargo's own resolver.
- **Runtime crates** of a platform are those `cargo tree -p bake-cli --target <triple> -e normal,no-proc-macro` lists, the crates linked into the binary.
- **Build crates** are build scripts, proc macros, and their dependencies below the linked crates in `cargo tree --target all -e normal,build --no-dedupe`, for every build host.

Dev-dependencies are excluded. Bake's own crates are listed with their NOTICE files and the `LICENSE-*` files of the third-party artifacts they embed, such as `bake-codemode`'s `engine/LICENSE-QuickJS-NG`.

[`about.toml`](../about.toml) is the license policy: cargo-about chooses each crate's license from its accepted list and fails a crate none satisfies. The generator adds no policy. It checks provenance:
- cargo-about must cover every selected crate.
- Every text must appear verbatim in that crate's own source package.
- Each clarification must name a selected crate, keep its declared license, and match its files' hashes.
- NOTICE, COPYRIGHT, and COPYING files at a crate's root are reproduced verbatim.

A crate published without the license file its repository keeps, as Wasmtime's internal crates are, takes a reviewed copy vendored under `licenses/` and pinned in [`licenses/upstream.toml`](../licenses/upstream.toml). The entry pins:
- the file's SHA-256;
- the repository commit, the path, and its raw URL;
- each inheriting crate's `name@version`, `Cargo.lock` checksum, and `.cargo_vcs_info.json` path.

That copy is used only where cargo-about found no text, and only when every pin matches. An entry no selected crate needs fails the run.

The pack checks that the bundle describes this build:
- its recorded inputs (`Cargo.lock`, `about.toml`, `licenses/upstream.toml`, and the vendored texts) are unchanged;
- each platform text matches its inventory hash and this platform's text is headed with its own triple;
- it describes `bake-cli` at the packed version;
- it reproduces the same NOTICE files as `notices/`.

The generator is a port of the Bun script that generated 0.4.0's bundle. On the same dependency graph it reproduces that bundle's text, differing only in the generator's name, input hashes, and paths.

## The glibc floor

`bake-release glibc-floor <binary> [--floor GLIBC_2.34]` reports the oldest glibc a built Linux `bake` starts on, and fails above the floor the owner chose ([D50](../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)): glibc 2.34, which RHEL 9, Amazon Linux 2023, Ubuntu 22.04, and Debian 12 meet.

It reads the version needs with `readelf` from binutils, in the C locale. The floor is the highest non-weak `GLIBC_` need; weak references, which Rust's standard library uses for newer functions, are reported without raising it. It fails closed on output it cannot read. A failure names the symbols bound above the floor, or a link that left a weak need unmarked. The workflow links every Linux build with the pinned toolchain's rust-lld, which marks those needs weak.

## Installers

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

`install.sh` and `install.ps1` read `native/stable.json` and its signature, and verify the signature over the exact bytes with the key they carry. They then check the manifest grammar, the archive's size and hash, and every member: a link, a special file, an absolute or `..` path, or more than 512 MiB unpacked is refused before anything is unpacked.

The unpacked release's own `bin/bake --self-check` must report the manifest's version and this platform before the tree is renamed into place and `current` moves. A release older than the installed one is refused, by the SemVer precedence `bake-update` applies. Both installers are held to the same case table, [`crates/bake-update/tests/fixtures/native-version-order.txt`](../crates/bake-update/tests/fixtures/native-version-order.txt).

The install root, `$BAKE_NATIVE_ROOT` (default `~/.local/share/bake-native`), holds:
- `releases/<version>/`;
- the `current` and `previous` links;
- `install.lock`, which installs, updates, rollbacks, and prunes all hold;
- `links.json`, which records the commands the installer created or replaced.

`<bin>/bake` links to `current/bin/bake`, in `$BAKE_BIN_DIR` (default `~/.local/bin`). A Bake-managed 0.3 `bake` is taken over and restorable with `--uninstall-links`. A `bake` no Bake installer made is left alone unless `--replace-command` is given.

Unix needs `curl` or `wget`, `tar`, and `sha256sum` or `shasum`. The signature is verified by an OpenSSL with Ed25519, proven on RFC 8032's vector first. Where no OpenSSL passes, as on a stock Mac, it is verified by the JavaScript verifier `install.sh` embeds, run with `osascript`. Bake 0.4 publishes Linux alone, so the installers stop on macOS and Windows before they download anything.

`bake-release`'s tests run the shipped `install.sh` against a loopback host: it installs a release the trusted key signed and refuses one another key signed. Its `version_newer` is checked against `compare_versions` on every pair of the table. `bake-release rehearse` installs the real archive this way, as the workflow does on both Linux platforms.

## Release with GitHub Actions

```sh
# on an up-to-date develop, once the release's changes are pushed:
NEXT=0.4.2
# set [workspace.package] version in Cargo.toml, then refresh both lockfiles:
cargo update --workspace --offline && (cd fuzz && cargo update --workspace --offline)
# turn CHANGELOG.md's [Unreleased] entries into a "## [$NEXT] - <date>" section, then:
cargo run --locked -p bake-release -- preflight --offline --tag "v$NEXT"
git commit -am "release: $NEXT" && git push origin HEAD:develop
gh pr create --base main --head develop     # CI runs here; fix failures on develop
# once that pull request merges with a merge commit:
git fetch origin && git tag -a "v$NEXT" origin/main -m "Bake $NEXT" && git push origin "v$NEXT"
```

The pushed tag starts the [release workflow](../.github/workflows/release.yml):

1. **preflight** checks the tag and records the version and its notes. The tag must point to a commit on the default branch and name the workspace version, which must be stable with a written changelog section. The version must be newer than the release the download service publishes, verified with the committed key.
2. **build** runs on `ubuntu-24.04` for `linux-x64` and `ubuntu-24.04-arm` for `linux-arm64`. Each runner:
   - links with the toolchain's rust-lld and builds cargo-about 0.9.2;
   - builds the release `bake` and `bake-release`;
   - packs the archive and checks its glibc floor;
   - rehearses its install with the shipped `install.sh`.
3. **assemble** puts both archives through `assemble --complete` with a throwaway key, so their names, layouts, and platforms are checked together before anything is signed.
4. **publish** runs in the `release` environment, which holds the secrets and is limited to `v*` tags. In order, it:
   - signs the manifest with `assemble --complete`;
   - runs the deploy's gate, `verify-native-manifest.mjs --complete --required`;
   - carries the served 0.3 route forward unchanged, checked with the 0.3 key;
   - uploads the archives, the manifest, its signature, and both installers to a draft GitHub release;
   - downloads them again to compare every byte, and publishes the draft as the latest release;
   - deploys the download service;
   - runs `verify-live <version> --complete`. It waits for the service to serve the version and checks it as `bake update` does, through `bake-update`'s download client: the signature, both archives' sizes and hashes through the redirect, the 0.3 route, the served installers against the committed ones, and `/health`.

A manual run from the Actions tab is a dry run. It builds, packs, rehearses, and assembles both platforms at whatever version the workspace holds, reports what would refuse a tag, and publishes nothing.

If a tagged run fails, rerun it. Before the GitHub release is public nothing has been served. After, the publish job stages that release's own signed assets instead of signing again.

| `release` environment secret | Value |
|---|---|
| `BAKE_NATIVE_RELEASE_SIGNING_KEY` | the whole PEM text of the native signing key |
| `CLOUDFLARE_API_TOKEN` | a token from the **Edit Cloudflare Workers** template, limited to the account in `host/wrangler.jsonc` and the `justar.dev` zone |

## The download service

[`host/`](host/) is the `bake-downloads` Cloudflare Worker: [`worker.mjs`](host/worker.mjs) runs the routes and headers in [`handler.mjs`](host/handler.mjs) over the files [`stage-site.mjs`](host/stage-site.mjs) stages, and [`server.mjs`](host/server.mjs) runs the same handler as a local Node server. It needs Node 24 or newer, and wrangler, pinned in `package.json` and run with `npx`. It has no other dependency.

`npm run deploy` from `host/` runs wrangler, whose build step, `stage-site.mjs`, runs both gates with the environment's extra keys removed:
- `verify-manifest.mjs` refuses a `latest.json` the committed 0.3 key (`release-key.pub`) did not sign.
- `verify-native-manifest.mjs` refuses a native manifest the committed native key did not sign, or one that breaks the rules above.

It then stages the landing page, both installers, and the four manifest files into `site/`. The Worker redirects every archive to `https://github.com/Justar96/bake/releases/download/v<version>/`, so a version's GitHub release must be public before the service names it.

The Worker reports active installs to gissx.org when it has the `GISSX_ACTIVITY_URL`, `GISSX_ACTIVITY_TOKEN`, and `BAKE_ACTIVITY_SALT` secrets, set with `npx wrangler secret put <name>`. Each report is a daily salted hash of the requesting address, sent after the response. The service serves release bytes only and stores no keys.

## Release steps by hand

The workflow's steps run locally the same way. On each Linux platform:

```sh
cargo build --release --locked -p bake-cli -p bake-release
target/release/bake-release pack
target/release/bake-release glibc-floor target/release/bake
target/release/bake-release rehearse --archive .artifacts/bake-native-release/<version>/bake-v<version>-<platform>.tar.gz
```

With both archives in `.artifacts/bake-native-release/<version>/` and published on the version's GitHub release:

```sh
target/release/bake-release assemble <version> --complete --key-file ~/.config/bake/native-release-signing-key.pem
for file in latest.json latest.json.sig; do curl -fsSL "https://bake.justar.dev/$file" -o "distribution/host/public/$file"; done
node distribution/host/verify-native-manifest.mjs --complete --required
(cd distribution/host && npm run deploy -- --message "Bake <version>")
target/release/bake-release verify-live <version> --complete
```

Generated paths (`.artifacts/`, `distribution/host/public/`, `site/`, `.wrangler/`) are ignored by Git. A deploy that reports success is not a release until `verify-live` passes.
