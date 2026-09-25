# Bake direct download releases

English | [中文](README.zh.md)

The `bake-downloads` Railway service in project `28ff3000-7240-4c57-81d1-7bd05445c1ee` serves a release manifest, installers, and versioned platform archives at `https://bake.justar.dev`. The service exists, but a release is available only after its archives are built, checked, and deployed. Bake still runs on Node; Bun builds the workspace and installs the production dependency graph into each archive.

## Build and check one platform locally

Use a clean checkout with stable Bun 1.4.2 (pinned in `package.json`), Node 24 or newer, and the host native build prerequisites. Run `bun install --frozen-lockfile`, `bun run build`, then:

```sh
bun run release:pack
BAKE_RELEASE_SIGNING_KEY_FILE=~/.config/bake/release-signing-key.pem bun run release:assemble
bun run release:verify-local
```

`release:pack` creates `.artifacts/bake-release/<Bake version>/bake-v<Bake version>-<platform>.tar.gz`. It requires the root and CLI versions to match, copies the declared package payload roots, licenses, changelog, and built files, then runs a frozen production Bun install in a temporary workspace and boots that staged CLI. `release:assemble` hashes every archive for the current Bake version, writes `distribution/host/public/latest.json` plus the versioned archives, and signs the manifest's exact bytes into `latest.json.sig`. The local check verifies that signature, serves those exact files over HTTP, runs the platform installer into temporary directories, and boots the installed command. It then publishes a release one patch newer, built from the installed files and signed by a key of its own, and updates to it with `bake update`: it checks `bake update --check`, a refused update when the newer manifest's key is not trusted, the update itself, `bake --version`, where `current` points, and that the replaced release is still on disk. These generated paths are ignored by Git.

### The release signing key

Installers and `bake update` trust a manifest only when the Ed25519 key in `distribution/host/release-key.pub` signed it; the same public key is written into both installers and `RELEASE_PUBLIC_KEYS` in `packages/boot/updater/src/keys.ts`, and a release test holds every copy equal. The private half never enters the repository: keep it as a PKCS#8 PEM file readable only by the release owner, and name it with `BAKE_RELEASE_SIGNING_KEY_FILE`. `release:assemble` refuses to run without it, and refuses a key whose public half differs from `release-key.pub`. A check without the key, such as CI, runs `bun run release:assemble --ephemeral-key`: it signs with a throwaway key and leaves the public half as `ephemeral-release-key.pub` beside the archives, where `release:verify-local` passes it to the clients through `BAKE_RELEASE_PUBLIC_KEY`. The download service's image build sets no such variable, so a manifest signed with a throwaway key never deploys. To rotate the key, add the new public key to `RELEASE_PUBLIC_KEYS` in a release signed by the old one, then sign with the new one.

Build each target on its own host: `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, and `win32-x64`. Verified platforms may be published independently; the current `0.1.0` payload contains Windows x64 only, and the download page states that macOS and Linux are not available yet. Collect the archives to publish in the same `.artifacts/bake-release/<Bake version>/` directory before assembling. When adding a target to an existing version, include every already published archive to keep it available; download those archives from the current manifest and verify their SHA-256 hashes before assembling. A single host build proves only its own target; the Windows installer and native modules need a Windows run. For a complete release, run `bun run release:assemble --complete` and `node distribution/host/verify-manifest.mjs --complete`. Every release version must be new: an installed release is named by its version and archive hash, and `bake update` offers only a version newer than the running one.

## Release with GitHub Actions

The [release workflow](../.github/workflows/release.yml) builds, checks, signs, and publishes every platform from one tag:

```sh
bun run release:prepare 0.1.1        # both manifests, bun.lock, and a CHANGELOG.md section
# write the 0.1.1 section in CHANGELOG.md, then:
git commit -am "release: 0.1.1"
git tag v0.1.1
git push origin HEAD v0.1.1
```

`release:prepare` moves the `[Unreleased]` changelog entry under the new version, or leaves a placeholder the release refuses. The pushed tag starts the workflow:

1. **preflight** (`release:preflight`) fails before anything builds when the tag does not name the workspace version, the changelog section is missing, empty, or still the placeholder, or the version is not newer than the one the download service serves.
2. **build** runs on one runner per target — `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`, `macos-15-intel`, and `windows-latest` — because the native modules are compiled for the host. Each runner checks it builds its target, packs the archive, installs it with the real installer and updates through it with `bake update` (`release:verify-local`, signed with a throwaway key), and, outside Windows, runs the built-profile terminal scenarios. Every platform reports even when one fails, and publishing needs all five.
3. **publish** runs in the `release` environment, the only place the signing key and the deploy token exist. It signs the manifest (`release:assemble --complete`), runs the image's own signature gate, drafts the GitHub release with the archives, `latest.json`, `latest.json.sig`, and both installers, deploys the download service, waits until the public host serves the new version and verifies it as a client does (`release:verify-live`), and only then publishes the draft. A failed step leaves a draft to finish by re-running the job.

Run the workflow by hand from the Actions tab for a dry run: it builds and checks all five platforms and publishes nothing.

Before the first tagged release, create the `release` environment under the repository's settings, limit it to tags matching `v*`, and, if releases should wait for a person, add a required reviewer. Give it two secrets:

| Secret | Value |
|---|---|
| `BAKE_RELEASE_SIGNING_KEY` | The whole PEM text of the release signing key |
| `RAILWAY_TOKEN` | A Railway project token for the `production` environment of the download service's project |

The GitHub release is a release host of its own: it carries the signed manifest beside the archives, and the installers and `bake update` accept `BAKE_RELEASE_BASE_URL=https://github.com/Justar96/bake/releases/latest/download`, fetching each archive from its own tag. Its assets download without a login only once the repository is public, and from then on the workflow also verifies the published GitHub release as a client reads it. Set the repository variable `BAKE_DOWNLOAD_SERVICE` to `off` to publish to GitHub alone; clients then need that host, either through the variable or by making it the default in `packages/boot/updater/src/keys.ts` and both installers.

## Publish a checked release by hand

After every archive listed in the manifest has passed its native local check, run `node distribution/host/verify-manifest.mjs` and deploy the download service from the repository root:

```sh
railway up ./distribution/host --path-as-root --no-gitignore \
  --project 28ff3000-7240-4c57-81d1-7bd05445c1ee \
  --environment production --service bake-downloads \
  --detach --json -m "Bake CLI direct download release"
```

The `--no-gitignore` flag includes the generated `public/` payload. The Docker build rejects an unsigned manifest or one the committed key did not sign, empty manifests, unsupported targets, invalid names, empty archives, and size or SHA-256 mismatches for every included archive. Use `--complete` for the optional five-platform gate. Record the deployment ID, wait for that deployment to reach `SUCCESS`, then check `/health`, `/latest.json`, `/latest.json.sig`, both installer routes, and each newly published platform archive through the public domain. An upload response alone does not qualify the release. The manifest lists the platforms currently available; the installers fail clearly on an unavailable platform.

Users with Node 24 or newer can install on a platform listed in the published manifest. Windows x64 is currently available; the Unix command requires the corresponding macOS or Linux archive to be published first:

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

The installers verify the manifest's signature, select the host platform, verify the archive's SHA-256 against the manifest, and install `bake` under the user's home. They never read or migrate `~/.dsh`; the command uses Bake's `~/.bake` home unless `DSH_HOME` is set. Unix needs `curl`, `tar`, and `shasum` or `sha256sum`; Windows needs `tar.exe`. The service serves release bytes only and stores no API keys.

Interactive installs and `bake update` show an amber ASCII oven while work runs, then leave a freshly baked loaf on success. Redirected output, CI, and `TERM=dumb` stay plain. Set `BAKE_NO_ANIMATION=1` to disable motion or `NO_COLOR=1` to omit color. The installers embed the CLI renderer; after changing `apps/cli/src/bakery.ts` or `scripts/release/installer-animation.ts`, run `bun scripts/release/embed-animation.ts` and verify with `bun scripts/release/embed-animation.ts --check`.

## Updating an install

`bake update` replaces an install the installers made with the newest signed release, and `bake update --check` only reports, exiting 0 when up to date, 10 when a newer release is available, and 1 on failure. The update downloads into the install root, checks the archive's size, SHA-256, and that its command starts as the version it claims, and moves `current` last, so any failure before that leaves the install as it was; see [the updater](../packages/boot/updater/README.md). Sessions already open keep running their own release; new ones start the new release. A source checkout is refused with the command that updates it.

On Unix, `<install root>/current` is a link that `~/.local/bin/bake` follows. On Windows, `bake.cmd` reads the release name from `<install root>\current.txt` on every run, so an update never rewrites the batch file a running `cmd.exe` is reading; an older `bake.cmd` that named one release directly is replaced once the `cmd.exe` running it exits.

The terminal names a newer release in its status line, from an answer cached for a day in `<Bake home>/update-check.json`, and refreshes that answer in the background without delaying the first frame. It never installs anything. `BAKE_NO_UPDATE_CHECK=1` turns the check off. Installs from a release before the updater have no `bake update`; running the installer again brings them onto it.
