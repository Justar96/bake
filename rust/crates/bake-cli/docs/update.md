# Updates

`bake update` installs the release Bake publishes for this platform and makes it current. It needs the install the Bake installer made: a native install keeps one unpacked release per version and a pointer at the one your `bake` command runs, so an update switches that pointer and nothing else. A `bake` built from source, or unpacked by hand, has no install to update and says so.

| Command | What it does |
|---|---|
| `bake update` | Installs the published release and makes it current |
| `bake update --check` | Reports a published release and installs nothing |
| `bake update --force <version>` | Installs the published version even when it is not newer than the installed one |
| `bake update --rollback` | Makes the release the last update replaced current again |

`bake update` exits 0 when it installed the release or the install is already up to date, and 1 with one line on standard error when it refused. `--check` exits 10 when a newer release is published, so a script can tell a waiting update from a failed check.

## On macOS and Windows

Bake 0.4.0 publishes native releases for Linux alone, x64 and arm64. On macOS and Windows every `bake update` run refuses with exit status 1 before it reaches the network or changes anything, including `--check`, `--force`, and `--rollback`, and names the release that brings those platforms:

```
update needs a native install, and Bake 0.4.0 publishes native updates for linux-arm64 and linux-x64 only; native updates for darwin-arm64 arrive in Bake 0.4.x
```

With `BAKE_NATIVE_ROOT` naming an install root, the line is the same without its first clause. The installers refuse those platforms too, so there is no native install there to update: wait for Bake 0.4.x, whose installer brings the macOS and Windows releases. A Bake 0.3 install keeps working with its own data and its own `dsh` command.

## What an update does

1. It fetches `stable.json` and its signature over HTTPS and refuses the manifest unless Bake's native release key signed exactly those bytes. Bake 0.3's release key cannot sign it, and its `latest.json` route is never read.
2. It installs only a version newer than the installed one. A manifest naming an older version is refused; `--force <version>` installs that published version anyway.
3. It streams the archive into a staging directory inside the install, hashing it as it arrives, and refuses it as soon as it is larger than, shorter than, or hashed differently from what the manifest states.
4. It unpacks the archive, refusing a member that is absolute, that leaves the release directory, that is a link or a device, or that would unpack to more than 512 MiB.
5. It runs the new release's own self-check, which must report the published version and this platform.
6. Only then does it move the pointer: first the one a rollback returns to, then the current one, each replaced by a rename, so a command that starts during an update runs the old release or the new one and never a half-installed one.
7. It keeps the current release and the one before it, and removes the rest. A release whose files are still open, as a running Windows `bake.exe` is, is left for a later update.

Anything that fails before step 6 leaves the install exactly as it was, and the next run clears what it left behind.

## While an update runs

Every install, update, and rollback holds the install's lock, so two of them never write the same files; a second one says so and exits instead of waiting. A command that was already running keeps running from the release it started: an update never replaces the files of a running release.

## Where it installs

The install root is `~/.local/share/bake-native`, or `%LOCALAPPDATA%\bake-native` on Windows; `BAKE_NATIVE_ROOT` names another one. It is separate from a Bake 0.3 install, which keeps working with its own data and its own `dsh` command. The commands the installer links live in `~/.local/bin`, or wherever `BAKE_BIN_DIR` names, and point at the current release, so an update does not touch them.
