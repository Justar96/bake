---
description: "Bake's self-updater: verify the signed release manifest, install a newer release beside the running one once it passes its launch check, roll back to an earlier one, and answer the update notice from an hourly cache."
kind: "package-library"
---

# bake-updater

## Summary

`bake-updater` is the code behind `bake update`, the terminal's `/update`, and its update notice. It trusts a release manifest only when a known Ed25519 key signed its exact bytes, installs a newer release into its own version directory beside the running one, runs that release's launch check, and moves the install's `current` pointer as its last step, so an interrupted or failed update leaves the old release in place. `bake update --rollback` moves `current` back to an earlier release still installed, after the same check. It updates only an install the installers laid out; a source checkout is left alone. Use it as a direct library dependency, not through `cordis.yml`.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

`detectInstall(release)` says whether the release directory this process runs from is managed: it sits directly in `<root>/versions/` under the name `<version>-<first 12 hex digits of its archive SHA-256>`. `fetchRelease(source)` fetches `latest.json` and `latest.json.sig` and returns the manifest only when a trusted key verifies the signature; `releaseSource(env)` gives the host and keys for this environment. `statusOf(manifest, running, target)` compares the result with the running version for this host's `hostTarget()`, reporting `newer`, `current`, or `unavailable` for a release published without an archive for this platform. `installRelease(options)` installs a `newer` result, reporting each step through `onProgress`: the download's received and total bytes, then `unpack` and `verify`. `selfUpdate(options)` is the whole run both commands share: it refuses an unmanaged install before asking the host (a `check` run may still ask), checks, records the answer, and installs, returning an `UpdateOutcome` each surface words itself. `currentVersion(root)` names the release the next launch runs, so a notice can tell an installed update from an available one.

`launchProblem(check)` runs a release directory's launch check and returns the first line of what went wrong, or `undefined` when it passed; `installRelease` runs it on every release before installing it. `rollbackRelease(options)` makes the newest installed release older than the current one current again, returning `none` when there is none, `unmanaged` for an install the updater does not manage, or `rolled-back` with the version directories it moved `current` from and to; `rollbackTarget(root, current)` names that release without changing anything.

`cachedUpdate(home, running)` reads the last answer from `<Bake home>/update-check.json` synchronously, for a notice on the first frame. `refreshCheck(options)` asks the host again only when that answer is an hour old (`CHECK_INTERVAL_MS`), and records a failed check too, retried after ten minutes (`FAILED_CHECK_RETRY_MS`). The short retry matters most just after a release: while it publishes, a new `latest.json` can sit beside the old signature, and that check fails. `recordCheck(home, status)` writes an answer another command found, so `bake update` and the notices agree. Only a verified manifest with an archive for this platform records a version. `checksDisabled(env)` is true when `BAKE_NO_UPDATE_CHECK` is set to anything but empty, `0`, or `false`.

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
3. It unpacks the archive with the system `tar` and runs the unpacked release's [launch check](#launch-check). A failure stops the install with `The downloaded Bake <version> did not start; the current install is unchanged:` and the first line of what the check reported.
4. It renames the unpacked tree into `versions/`, on the same filesystem. A directory already there that passes the check is reused without a download; one that fails it is set aside and replaced.
5. It moves `current`. Unix renames a fresh absolute link over `<root>/current`; Windows renames a fresh file over `<root>/current.txt`. A reader sees the old target or the new one, never neither.

The running process resolved its own version directory when it started, so it keeps running from it. The next launch starts the new release.

<a id="launch-check"></a>
### Launch check

`launchProblem` starts the release's `apps/cli/lib/bin.js` with the running Node, as the installed launchers do: with `--report-exclude-env --report-exclude-network --diagnostic-dir=…`, so the command runs directly instead of restarting itself. It runs `bake --self-check`, which composes the shipped `tui` and `headless` profiles and imports every plugin their trees name, the agent presets' plugins, and the terminal runner's modules, then exits; see [the launcher](../../../apps/cli/README.md). That is what a launch loads, so a package missing from the platform's installed layout fails the check rather than the next `bake`. The check passes only when the command exits 0 and prints the release's version.

The command runs in a scratch directory the updater makes under the system temporary directory: it holds the check's `BAKE_HOME` and its `TMPDIR`, `TMP`, and `TEMP`, and the updater removes it afterwards, whether the check passed, failed, or was stopped. The caller's Bake home is never read or written, and Node's compile cache is off for the check. The check needs no TTY, network, or model key. It is stopped and fails after 60 seconds (`LAUNCH_CHECK_TIMEOUT_MS`), and an abort signal stops it and rejects. A complete release passes in about two seconds.

Releases up to `LAST_RELEASE_WITHOUT_SELF_CHECK` (0.3.6) have no `--self-check`, and their launcher would read the flag as a profile boot. For those the check runs `--version` instead, which must report the release's version, as every update did before the check existed; that keeps a rollback to such a release possible. A newer release must pass `--self-check`: one that lacks it fails closed, with the launcher's complaint as the reason.

<a id="rollback"></a>
### Rollback

The updater records no history of `current`; the version directories are the record. `rollbackRelease` takes the install lock, reads `current` under it, and picks the newest release under `versions/` whose version is older than the current one and whose command is on disk; among copies of one version, the one whose launch marker is newest wins. Right after an update that is the release the update replaced, which pruning always keeps. A second rollback goes one release further back, for as long as an older one remains. The target must pass the same [launch check](#launch-check); a failure stops with `The earlier Bake <version> did not start; the current install is unchanged:` and the reason. Then `current` moves by the same atomic rename an install uses. A rollback removes nothing, so `bake update` returns to the newest release without downloading it again.

Each launch touches `.last-launch` in its version directory (`markLaunched`). After an install, a version directory is removed only when it is not the new current, not the one it replaced, not the one the updater runs from, and nobody has started it for a week. That keeps a release another terminal still runs from, whose next lazy import would otherwise fail.

On Windows, `cmd.exe` reads a batch file a line at a time, by offset, while it runs, so rewriting the running `bake.cmd` would make it execute the new file from the old file's offset. `windowsLauncher(root)` therefore writes a launcher that reads `current.txt` on every run and never needs rewriting; the release archive ships the same text as `bin/bake-launcher.cmd.template` for `install.ps1`. An older `bake.cmd` that named one release directly is replaced by a detached helper once the `cmd.exe` running it has exited.

This launcher, like the release's `bin/bake.cmd` and Unix `bin/bake`, creates `<Bake home>/diagnostics` and starts Node with `--report-exclude-env --report-exclude-network --diagnostic-dir=<Bake home>/diagnostics`. The runtime watchdog arms fatal-error reports only when environment variables are excluded on Node's command line; `NODE_OPTIONS` would pass the flags to the agent's subprocesses as well. An installed `bake.cmd` whose text differs is replaced by that same helper, but only when the updater running is one that already writes the new text, so a changed launcher reaches Windows installs one update later.

Bake Desktop starts Bake as `dsh --profile desktop`, so an update also gives an install made before the `dsh` alias its second command, best effort. `addDshAlias` writes a missing `bake.cmd` or `dsh.cmd` beside the Windows launcher and never overwrites one. On Unix it links `<bin dir>/dsh` (`BAKE_BIN_DIR`, or `~/.local/bin`) to `<install root>/current/bin/bake` only when `<bin dir>/bake` is a link to that same launcher and no `dsh` exists, so a `dsh` from another install stays in place.

The release signing key's private half never enters the repository. `release:assemble` reads it from `BAKE_RELEASE_SIGNING_KEY_FILE` and signs the manifest's bytes as written. `RELEASE_PUBLIC_KEYS` may hold more than one key, so a new key can be trusted before the old one stops signing; `distribution/host/release-key.pub` and both installers carry the first, and a release test holds every copy to it.

No runtime invariant companion is published because the package is a library that never mounts in a Cordis tree: it emits no events and keeps no in-process relation a companion could observe. What it guards lives on disk and across processes, and each install step checks its own result as it goes: the manifest signature, the archive's size and SHA-256, and the unpacked release's launch check.

<a id="known-limitations-and-deferred-work"></a>
## Known Limitations and Deferred Work

- The notice names an update; nothing installs one without `bake update` or `/update`.
- There is no release channel: every install follows `latest.json`.
- Pruning cannot see a release another process runs from beyond its launch marker; a session left open for more than a week from a release two updates old can lose its files on Unix.
- A rolled-back install is not pinned: the notice still names the newer release, and `bake update` installs it again.
- The launch check imports every plugin a launch loads but applies none, so a plugin that fails only while it starts passes the check.

<a id="dev-note"></a>
## Dev Note

```sh
bun run test:runtime packages/boot/updater
```

The tests pack real `.tar.gz` releases and serve them from memory, signed by throwaway keys. `bun run release:verify-local` covers the whole path: it installs the staged release with the real installer, publishes a newer one built from it, and runs `bake update --check`, a refused update against an untrusted key, `bake update`, and `bake --version`, then `bake update --rollback` back to the staged release and a second rollback with nothing left to return to. `bun run test:integration apps/cli/tests/self-check.e2e.ts` runs the launch check on the built launcher in release layouts that each lack one package.
