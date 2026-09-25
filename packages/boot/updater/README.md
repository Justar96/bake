---
description: "Bake's self-updater: verify the signed release manifest, install a newer release beside the running one, and answer the update notice from a daily cache."
kind: "package-library"
---

# @deepseek-ai/dsh-updater

English | [中文](README.zh.md)

## Summary

`@deepseek-ai/dsh-updater` is the code behind `bake update` and the terminal's update notice. It trusts a release manifest only when a known Ed25519 key signed its exact bytes, installs a newer release into its own version directory beside the running one, and moves the install's `current` pointer as its last step, so an interrupted update leaves the old release in place. It updates only an install the installers laid out; a source checkout is left alone. Use it as a direct library dependency, not through `cordis.yml`.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

`detectInstall(release)` says whether the release directory this process runs from is managed: it sits directly in `<root>/versions/` under the name `<version>-<first 12 hex digits of its archive SHA-256>`. `fetchRelease(source)` fetches `latest.json` and `latest.json.sig` and returns the manifest only when a trusted key verifies the signature; `releaseSource(env)` gives the host and keys for this environment. `statusOf(manifest, running, target)` compares the result with the running version for this host's `hostTarget()`, reporting `newer`, `current`, or `unavailable` for a release published without an archive for this platform. `installRelease(options)` installs a `newer` result.

`cachedUpdate(home, running)` reads the last answer from `<Bake home>/update-check.json` synchronously, for a notice on the first frame. `refreshCheck(options)` asks the host again only when that answer is a day old, and records a failed check too, so a host that is down is asked once a day. Only a verified manifest with an archive for this platform records a version. `checksDisabled(env)` is true when `BAKE_NO_UPDATE_CHECK` is set to anything but empty, `0`, or `false`.

| Variable | Effect |
|---|---|
| `BAKE_RELEASE_BASE_URL` | Release host; `https://bake.justar.dev` by default, as for the installers. `https://github.com/<owner>/<repo>/releases/latest/download` names a repository's GitHub releases, and `archiveUrl` then fetches each archive from its own tag |
| `BAKE_RELEASE_PUBLIC_KEY` | One more trusted key, base64 DER SPKI, for a release signed with a throwaway key in a local or CI check |
| `BAKE_NO_UPDATE_CHECK` | Turns the background check off; `bake update` still works |

<a id="understand-the-implementation"></a>
## Understand the implementation

An install moves through five steps, and only the last changes what `bake` starts:

1. It takes `<root>/update.lock`, which records the holder's process id and start time. A contender takes over a lock whose process has exited or that is older than 30 minutes, so a killed update never blocks the next.
2. Holding the lock, it clears `<root>/.staging/`, where an interrupted run may have left files, and streams the archive there. It stops reading once the archive outgrows its stated size, and rejects it when the size or SHA-256 differs from the manifest.
3. It unpacks the archive with the system `tar` and starts the unpacked command's `--version`, which must report the manifest's version.
4. It renames the unpacked tree into `versions/`, on the same filesystem. A directory already there that starts is reused without a download; one that does not start is set aside and replaced.
5. It moves `current`. Unix renames a fresh absolute link over `<root>/current`; Windows renames a fresh file over `<root>/current.txt`. A reader sees the old target or the new one, never neither.

The running process resolved its own version directory when it started, so it keeps running from it. The next launch starts the new release.

Each launch touches `.last-launch` in its version directory (`markLaunched`). After an install, a version directory is removed only when it is not the new current, not the one it replaced, not the one the updater runs from, and nobody has started it for a week. That keeps a release another terminal still runs from, whose next lazy import would otherwise fail.

On Windows, `cmd.exe` reads a batch file a line at a time, by offset, while it runs, so rewriting the running `bake.cmd` would make it execute the new file from the old file's offset. `windowsLauncher(root)` therefore writes a launcher that reads `current.txt` on every run and never needs rewriting; the release archive ships the same text as `bin/bake-launcher.cmd.template` for `install.ps1`. An older `bake.cmd` that named one release directly is replaced by a detached helper once the `cmd.exe` running it has exited.

The release signing key's private half never enters the repository. `release:assemble` reads it from `BAKE_RELEASE_SIGNING_KEY_FILE` and signs the manifest's bytes as written. `RELEASE_PUBLIC_KEYS` may hold more than one key, so a new key can be trusted before the old one stops signing; `distribution/host/release-key.pub` and both installers carry the first, and a release test holds every copy to it.

<a id="known-limitations-and-deferred-work"></a>
## Known Limitations and Deferred Work

- The notice names an update; nothing installs one in the background.
- There is no release channel: every install follows `latest.json`.
- Pruning cannot see a release another process runs from beyond its launch marker; a session left open for more than a week from a release two updates old can lose its files on Unix.

<a id="dev-note"></a>
## Dev Note

```sh
bun run test:runtime packages/boot/updater
```

The tests pack real `.tar.gz` releases and serve them from memory, signed by throwaway keys. `bun run release:verify-local` covers the whole path: it installs the staged release with the real installer, publishes a newer one built from it, and runs `bake update --check`, a refused update against an untrusted key, `bake update`, and `bake --version`.
