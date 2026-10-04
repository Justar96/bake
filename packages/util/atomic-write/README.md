---
description: "Atomic file replacement and cross-process writer locking for packages that must never leave partial, symlink-hijacked, or wider-permission content on disk."
kind: "package-library"
---

# @deepseek-ai/dsh-atomic-write

## Summary

Use `dsh-atomic-write` to replace a file without exposing partial content or following a symlinked temporary path. Its writer lock serializes read-modify-write cycles across processes so concurrent writers cannot overwrite one another with stale state. Each replacement uses caller-selected permission bits on a fresh inode, which safely narrows an existing file's permissions. A writer that dies while holding the lock does not block later writers: on Linux and macOS the lock is held under a kernel `flock`, so the next writer recovers it without operator action. The replacement is synced to disk before it is published and its directory after, so a crash or power loss leaves either the old or the new complete file, never an empty or partial one. This library accepts strings; it does not provide a `cordis.yml` plugin.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Use `writeFileAtomic` when a file-backed store must replace one already-rendered string without ever exposing a partial, symlink-hijacked, or wider-permission state, and `withFileLock` when several processes read-modify-write the same file. The smallest path is one call with the final content and the replacement's permission bits.

### Writing a file atomically

```ts
import { writeFileAtomic } from '@deepseek-ai/dsh-atomic-write'

declare const text: string
await writeFileAtomic('/home/u/.dsh/settings.yaml', text, { mode: 0o600 })
```

Parent directories are created as needed, and readers observe either the old or the new complete content. Once the call resolves, the new content survives a crash or power loss. On Windows, transient replacement interference reported as `EACCES`, `EBUSY`, or `EPERM` is retried for a bounded interval; any remaining failure removes the temporary file and leaves the target untouched.

### Coordinating writers

For a read-render-commit cycle that a bare atomic commit cannot make safe on its own, hold the writer lock around the operation:

```text
import { withFileLock, writeFileAtomic } from '@deepseek-ai/dsh-atomic-write'

declare const render: (previous: string) => string
declare const readCurrent: () => Promise<string>

await withFileLock('/home/u/.dsh/settings.yaml', async () => {
  const previous = await readCurrent()
  await writeFileAtomic('/home/u/.dsh/settings.yaml', render(previous), { mode: 0o600 })
})
```

Only writers contend — readers never take the lock — and a contender backs off exponentially and fails with a timed-out error rather than blocking forever. How long a contender waits is stated per call through `waitMs`: the default is sized for file work alone, so a holder whose cycle includes a network round trip — a credential mutation that refreshes an expired token — states a longer one, because leaving the default would fail every other writer of that file for the duration. The retry cadence stays fixed. A live holder is never displaced, however long it runs: a contender removes an existing lock only when its owner is proven gone, never because of the lock's age.

### Failures to plan for

Windows retries one `EPERM` when the lock cannot be observed, because its holder can release between exclusive creation and the existence check. A repeated unconfirmed `EPERM` is rethrown without running the operation.

The lock's parent directory must already exist, so `withFileLock` rejects an invalid parent hierarchy before running the operation. On Linux and macOS, a process that exits or is killed while holding the lock leaves its lock sibling behind, and the next writer removes it and proceeds without waiting. A lock sibling whose owner cannot be proven gone stays in place, and writers time out as before. This covers a sibling written on another host, a PID record whose process still exists, and content that names no owner, such as the empty file a writer leaves when it dies between creating the lock and writing its PID. Windows, a missing native binding, and a filesystem without `flock` keep the earlier behavior, which never recovers a lock; there, an operator removes an orphaned sibling after verifying that no writer still owns it.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

The package is built on one separation: the atomic commit owns the swap, and the writer lock owns cross-process ordering.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | `writeFileAtomic` and `withFileLock`, the package's whole surface |
| [`tests/fixtures/lock-holder.ts`](tests/fixtures/lock-holder.ts) | Child process that holds the lock through either protocol, for the cross-process specs |
| — | No runtime invariant companion is published; this pure filesystem primitive owns no event stream or mutable runtime data; its replacement contract is enforced by unit tests. |

### Write path

`writeFileAtomic` writes a random-suffix sibling opened with exclusive create (`wx`), syncs it with `fsync`, then renames it over the target and syncs the parent directory. Syncing the content first means a crash can never publish a renamed file whose data never reached the disk; syncing the directory makes the rename itself survive a power loss. A failure before the rename removes the sibling and leaves the target untouched. A directory sync that fails with an I/O error is reported even though the replacement is already visible, because it may not survive a crash; a filesystem or directory permission that cannot sync the directory at all, such as some FUSE and network mounts, leaves the rename as durable as that filesystem allows. The exclusive open refuses to follow a symlink planted at a guessable temp path; the same-directory sibling keeps the rename on one filesystem; and the rename replaces a symlinked target itself instead of writing through to its referent. A Windows retry keeps the same complete sibling and uses bounded exponential backoff, so temporary use of the target by software outside the cooperative writer lock cannot turn a safe replacement into an immediate failure; the archived [retry decision record](../../../.agents/notes/archived/bug-fix/2026-08-29-windows-atomic-replace-retry.md) documents the original rationale and rejected alternatives.

`withFileLock` serializes writers through a `<filename>.lock` sibling created with `wx`. `EEXIST` identifies contention directly; `EPERM` does so only when a fresh `lstat` confirms the lock path exists, covering Windows exclusive-create behavior without hiding an unrelated permission failure. Contention backs off exponentially and fails when the per-call `waitMs` deadline (default two seconds) passes, with the same error in every configuration.

The holder first writes its `<pid>\n` record into the created file, as earlier releases did. Where the [`node-addon-system`](../../../native/system/README.md) `flock` binding loads (Linux and macOS), the holder then takes a non-blocking `flock` on its descriptor and confirms that the lock path still names its file. It then overwrites the record in place with a JSON record of its PID and host, and keeps the descriptor open until release. The holder writes the JSON record only while holding the `flock`, and no contender reads a record without holding it, so no contender ever reads the JSON record in an unlocked or partly written state. A contender that finds the lock opens it without following symlinks and tries its `flock`. Contention means a live holder, so the contender backs off. Otherwise it confirms that the path still names the file it locked, then reads the record. A same-host JSON record proves its holder gone, because the kernel releases a `flock` when its process dies. A `<pid>\n` record proves its holder gone only when signalling that PID reports that no such process exists. When the holder is proven gone, the contender unlinks the lock while still holding its `flock` and retries at once. Holding the `flock` means two contenders never both remove one lock, and neither removes a file that replaced the one it locked. A holder whose file was removed before it took the `flock` acquires again. The holder releases by unlinking the lock while the path still names its file, then closing the descriptor. The protocol creates no file other than the lock itself, so a directory watcher sees only the lock sibling, as before.

Windows, a binding that fails to load, and a filesystem that refuses `flock` keep the `<pid>\n` record: the holder closes the file and removes it in a `finally`, and a contender never removes another writer's lock. Both forms keep the lock path occupied while held, so they exclude each other, and writers of different releases can run side by side.

### Why the swap stays safe

- **Fresh inode, caller-stated mode** — the temp carries `mode` through the rename, so narrowing a wider-permission file has no chmod race. `mode` is required so the permission decision stays visible at every call site.
- **Readers never contend** — the rename commit is atomic, so a reader needs no lock.
- **A contender deletes only a proven-dead lock** — a released kernel `flock` or a vanished PID proves the owner stopped; the lock's age never counts, because age cannot distinguish a crashed owner from a paused live writer.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when you need the consuming stores or the family this primitive belongs to.

- [User-settings file store](../../settings/settings-file/README.md) — the settings document every write replaces through this package.
- [Credentials store](../../credentials/credentials-local/README.md) — the credentials file this package locks and replaces.
- [Native `flock` behavior](../../../native/system/docs/flock-contract.md) — the kernel lock semantics the writer lock's crash recovery relies on.
- [util group map](../README.md) — the utility family this package belongs to.

-----

<a id="model-experience"></a>
## Model Experience

None, as this is a pure filesystem write primitive that registers nothing model-facing.

#### KV Cache effect

Nothing here enters a request prefix, so provider cache reuse is unaffected.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define where the package is not the right tool. They are current package constraints, not a task backlog.

- **Windows syncs the content but not the rename** — Node cannot open a directory there, so a power loss just after a write may still find the previous complete file, never a partial one. A filesystem that refuses to sync directories behaves the same way.
- **String content only** — no `Buffer` or stream form until a consumer needs one.
- **Automatic lock recovery needs the native `flock`** — on Windows, without the native binding, or on a filesystem without `flock`, an orphaned lock sibling blocks writers until an operator removes it. So does an empty sibling left by a writer that died between creating the lock and writing its PID.
- **Recovery assumes one host per lock directory** — a PID record from the earlier protocol carries no host, so a contender judges it by this host's process table. A live earlier-release writer on another host or in another PID namespace that shares the directory could lose its lock. A kernel-held record from another host is never recovered, including after the local hostname changes.
- **The lock belongs to the process, not its children** — the lock descriptor closes on exec, so a subprocess that the holder started, such as the package manager a plugin operation runs, keeps running unlocked if the holder dies.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

Preserving owner-only permissions on Windows, where `mode` sets only the read-only attribute, remains open (tracked as `settings-windows-owner-only` in source).

</details>
