# Bake direct download releases

English | [中文](README.zh.md)

The `bake-downloads` Railway service in project `28ff3000-7240-4c57-81d1-7bd05445c1ee` serves a release manifest, installers, and versioned platform archives at `https://bake.justar.dev`. The service exists, but a release is available only after its archives are built, checked, and deployed. Bake still runs on Node; Bun builds the workspace and installs the production dependency graph into each archive.

## Build and check one platform locally

Use a clean checkout with stable Bun 1.4.2 (pinned in `package.json`), Node 24 or newer, and the host native build prerequisites. Run `bun install --frozen-lockfile`, `bun run build`, then:

```sh
bun run release:pack
bun run release:assemble
bun run release:verify-local
```

`release:pack` creates `.artifacts/bake-release/<Bake version>/bake-v<Bake version>-<platform>.tar.gz`. It requires the root and CLI versions to match, copies the declared package payload roots, licenses, changelog, and built files, then runs a frozen production Bun install in a temporary workspace and boots that staged CLI. `release:assemble` hashes every archive for the current Bake version and writes `distribution/host/public/latest.json` plus the versioned archives. The local check serves those exact files over HTTP, runs the platform installer into temporary directories, and boots the installed command. These generated paths are ignored by Git.

Build every target on its own host: `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, and `win32-x64`. Collect the archives in the same `.artifacts/bake-release/<Bake version>/` directory before assembling a public release, then run `bun run release:assemble --complete` to reject a missing target. A single host build proves only its own target; the Windows installer and native modules need a Windows run. The current Bake version is `0.1.0`.

## Publish the checked release

After the manifest contains every target and each archive has passed its native local check, deploy the download service from the repository root:

```sh
railway up ./distribution/host --path-as-root --no-gitignore \
  --project 28ff3000-7240-4c57-81d1-7bd05445c1ee \
  --environment production --service bake-downloads \
  --detach --json -m "Bake CLI direct download release"
```

The `--no-gitignore` flag includes the generated `public/` payload. The Docker build verifies all five archives and their hashes, so a partial local manifest cannot publish. Record the deployment ID, wait for that deployment to reach `SUCCESS`, then check `/health`, `/latest.json`, both installer routes, and a platform archive through the public domain. An upload response alone does not qualify the release.

Once published, users with Node 24 or newer can install with:

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

The installers select the host platform, verify the archive's SHA-256 against the manifest, and install `bake` under the user's home. They never read or migrate `~/.dsh`; the command uses Bake's `~/.bake` home unless `DSH_HOME` is set. Unix needs `curl`, `tar`, and `shasum` or `sha256sum`; Windows needs `tar.exe`. The service serves release bytes only and stores no API keys.
