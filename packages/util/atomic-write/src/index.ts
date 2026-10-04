/**
 * Atomic file replacement and writer coordination.
 * `writeFileAtomic` writes and syncs a random-suffix sibling with exclusive
 * create and the caller's permission bits, renames it over the target, and
 * syncs the directory, so readers observe either the old or the new complete
 * content, a crash never leaves a partial file, and a replaced file ends up
 * with exactly the stated mode. `withFileLock` serializes cross-process
 * writers of one file through a `wx`-created `<file>.lock` sibling, held
 * under a kernel `flock` where the host supports one, so a read-modify-write
 * cycle can never resurrect a state another writer just replaced and a writer
 * that dies holding the lock never blocks the next one; readers stay
 * lock-free because the rename commit is atomic.
 * @module @deepseek-ai/dsh-atomic-write
 */

import { randomBytes } from 'node:crypto'
import { constants, lstat, mkdir, open, rename, rm } from 'node:fs/promises'
import type { FileHandle } from 'node:fs/promises'
import { hostname } from 'node:os'
import { dirname } from 'node:path'
import { tryLockExclusive } from '@deepseek-ai/node-addon-system/flock'

const WINDOWS_TRANSIENT_RENAME_ERRORS: ReadonlySet<string> = new Set(['EACCES', 'EBUSY', 'EPERM'])
const WINDOWS_RENAME_RETRY_INITIAL_MS = 20
const WINDOWS_RENAME_RETRY_MAX_MS = 200
const WINDOWS_RENAME_RETRY_LIMIT = 8

/** Whether Windows reported temporary interference with an atomic replacement. */
function isTransientWindowsRenameError(error: unknown): boolean {
  if (process.platform !== 'win32') return false
  return WINDOWS_TRANSIENT_RENAME_ERRORS.has((error as NodeJS.ErrnoException | null)?.code ?? '')
}

/** Replace the target after bounded retries for transient Windows interference. */
async function renameAtomicTemp(temp: string, filename: string): Promise<void> {
  let delay = WINDOWS_RENAME_RETRY_INITIAL_MS
  for (let retries = 0;; retries += 1) {
    try {
      await rename(temp, filename)
      return
    } catch (error) {
      if (!isTransientWindowsRenameError(error)) throw error
      if (retries >= WINDOWS_RENAME_RETRY_LIMIT) throw error
    }
    await new Promise(resolve => setTimeout(resolve, delay))
    delay = Math.min(delay * 2, WINDOWS_RENAME_RETRY_MAX_MS)
  }
}

/**
 * Filesystem options for {@link writeFileAtomic}; `mode` is required so the
 * permission decision stays visible at every call site.
 */
export interface WriteFileAtomicOptions {
  /**
   * Permission bits stamped on the fresh temp inode and carried through the
   * rename (subject to the process umask, like every fresh inode).
   */
  mode: number
  /**
   * Permission bits for parent directories this call creates (subject to the
   * umask; existing directories keep their mode). Omission uses the mkdir
   * default — pass `0o700` when the tree holds user-private data.
   */
  dirMode?: number
}

/**
 * Directory-sync failures that mean the filesystem or the directory's
 * permissions cannot sync it, not that synced data was lost: a FUSE or
 * network mount may refuse `fsync` on a directory, and a directory the
 * process may write but not read cannot be opened.
 */
const UNSYNCABLE_DIRECTORY_ERRORS: ReadonlySet<string> = new Set([
  'EACCES', 'EINVAL', 'EISDIR', 'ENOSYS', 'ENOTSUP', 'EOPNOTSUPP', 'EPERM',
])

/**
 * Sync `path`'s directory entries so a rename into it survives a power loss.
 * Windows cannot open a directory for syncing and is skipped; a filesystem
 * that cannot sync the directory leaves the rename as durable as it allows.
 */
async function syncDirectory(path: string): Promise<void> {
  /* v8 ignore next -- Windows rejects directory opens; POSIX coverage exercises the sync. */
  if (process.platform === 'win32') return
  let handle: FileHandle
  try {
    handle = await open(path, 'r')
  } catch (error) {
    if (UNSYNCABLE_DIRECTORY_ERRORS.has(errorCode(error) ?? '')) return
    throw error
  }
  try {
    await handle.sync()
  } catch (error) {
    if (!UNSYNCABLE_DIRECTORY_ERRORS.has(errorCode(error) ?? '')) throw error
  } finally {
    await handle.close()
  }
}

/**
 * Replace `filename` with `content` in one atomic, crash-durable step,
 * creating parent directories. The content is first written to a
 * random-suffix sibling opened with exclusive create (`wx`): the open refuses
 * to follow a symlink planted at the temp path, and the fresh inode carries
 * `options.mode` through the rename, so replacing a wider-permission file
 * narrows it without a chmod race. The sibling is synced to disk before the
 * rename, so a crash can never publish an empty or partly written file, and
 * the parent directory is synced after it, so the replacement survives a
 * power loss once this resolves. The rename also replaces a symlinked target
 * itself instead of writing through to its referent, and the same-directory
 * sibling keeps the rename on one filesystem. Windows replacement retries
 * transient `EACCES`, `EBUSY`, and `EPERM` failures for a bounded interval
 * while the complete temp file remains the rename source. On any failure
 * before the rename the temp file is removed, the target is untouched, and
 * the failure rethrown. A directory sync that fails with an I/O error is
 * rethrown although the replacement is already visible, because it may not
 * survive a crash.
 * @param filename - final path receiving the content.
 * @param content - complete next file content.
 * @param options - permission bits for the replacement inode.
 */
export async function writeFileAtomic(filename: string, content: string, options: WriteFileAtomicOptions): Promise<void> {
  await mkdir(dirname(filename), {
    recursive: true,
    ...options.dirMode === undefined ? {} : { mode: options.dirMode },
  })
  // TODO(settings-windows-owner-only): Preserve owner-only permissions on
  // Windows, where `mode` sets only the read-only attribute.
  const temp = `${filename}.${randomBytes(6).toString('hex')}.tmp`
  try {
    const handle = await open(temp, 'wx', options.mode)
    try {
      await handle.writeFile(content, 'utf8')
      await handle.sync()
    } finally {
      await handle.close()
    }
    await renameAtomicTemp(temp, filename)
  } catch (error) {
    await rm(temp, { force: true })
    throw error
  }
  await syncDirectory(dirname(filename))
}

/** Error code of a failed filesystem or lock call. */
function errorCode(error: unknown): string | undefined {
  return (error as NodeJS.ErrnoException | null)?.code
}

/** Whether an exclusive create found an existing lock. */
async function isLockContention(error: unknown, lockPath: string): Promise<boolean> {
  const code = errorCode(error)
  if (code === 'EEXIST') return true
  if (code !== 'EPERM') return false
  try {
    await lstat(lockPath)
    return true
  } catch {
    // Keep the original EPERM authoritative when lock existence is unproven.
    return false
  }
}

/**
 * Retry cadence for a contended lock. These stay robustness invariants of the
 * cross-process write protocol rather than deployment tunables: they govern how
 * often a contender asks, which no caller has a reason to vary.
 */
const LOCK_RETRY_INITIAL_MS = 20
const LOCK_RETRY_MAX_MS = 200

/**
 * How long a contender waits when the caller states no limit — sized for the
 * render-and-rename cycle every call site had when this package was written.
 * Expiry fails the contender; a lock is only ever removed when its owner is
 * proven gone, never for its age. How long is *worth* waiting is a property
 * of the operation the lock holder runs, which is why
 * {@link FileLockOptions.waitMs} exists; the value here is the floor for an
 * operation that does file work alone.
 */
const DEFAULT_LOCK_WAIT_MS = 2_000

/** Lock content of the created-file protocol: the holder's PID and a newline. */
const PID_RECORD = /^([1-9]\d*)\n$/

/**
 * Lock content of the kernel-held protocol. The holder writes it only while
 * holding the file's `flock` and keeps that `flock` until it unlinks the
 * file, so a same-host record whose `flock` a contender can take has no
 * living holder.
 */
interface KernelLockRecord {
  flock: true
  pid: number
  host: string
}

/** Render this process's kernel-held lock record. */
function kernelLockRecord(): string {
  const record: KernelLockRecord = { flock: true, pid: process.pid, host: hostname() }
  return `${JSON.stringify(record)}\n`
}

/** Whether a PID names no process on this host; a signal-permission refusal still proves one exists. */
function processGone(pid: number): boolean {
  if (!Number.isSafeInteger(pid)) return false
  try {
    process.kill(pid, 0)
    return false
  } catch (error) {
    return errorCode(error) === 'ESRCH'
  }
}

/**
 * Whether the content of a lock file whose `flock` the caller holds proves
 * its owner stopped. A kernel-held record from this host does; one from
 * another host does not, because that host's `flock` may not reach this one.
 * A PID record does only when no process has that PID. Any other content,
 * including an empty or partly written record, names no owner and never does.
 */
function isAbandoned(content: string): boolean {
  const pid = PID_RECORD.exec(content)?.[1]
  if (pid !== undefined) return processGone(Number(pid))
  let record: unknown
  try {
    record = JSON.parse(content)
  } catch {
    // Unparsable content names no owner, so the lock stays in place.
    return false
  }
  const { flock, host } = (record ?? {}) as Partial<KernelLockRecord>
  return flock === true && host === hostname()
}

/** Whether a descriptor's file is still the one `path` names. */
async function isAtPath(handle: FileHandle, path: string): Promise<boolean> {
  const held = await handle.stat({ bigint: true })
  const current = await lstat(path, { bigint: true }).catch((error: unknown) => {
    if (errorCode(error) === 'ENOENT') return undefined
    throw error
  })
  return current !== undefined && current.ino === held.ino && current.dev === held.dev
}

/** The failure every protocol reports when the deadline passes under contention. */
function lockTimeout(lockPath: string): Error {
  return new Error(`atomic-write: timed out waiting for the writer lock at ${lockPath}`)
}

/** Whether a flock refusal means another descriptor holds the lock. */
function isFlockContention(error: unknown): boolean {
  const code = errorCode(error)
  // flock(2) reports EAGAIN; some libcs spell it EWOULDBLOCK.
  return code === 'EAGAIN' || code === 'EWOULDBLOCK'
}

/** A held writer lock: kernel-held through its open descriptor, or the created file alone. */
type HeldLock =
  | { readonly kind: 'kernel'; readonly handle: FileHandle }
  | { readonly kind: 'created' }

/**
 * Take the `flock` of a lock file this process just created. Only a
 * contender inspecting the PID record can hold it meanwhile, and only while
 * it reads, so contention is retried at a short interval.
 * @returns whether the flock is held; false when no kernel lock can serve or the deadline passed.
 */
async function lockCreatedFile(handle: FileHandle, deadline: number): Promise<boolean> {
  let delay = 1
  for (;;) {
    try {
      await tryLockExclusive(handle.fd)
      return true
    } catch (error) {
      // A missing or unloadable binding, or a filesystem without flock, leaves the created file as the lock.
      if (!isFlockContention(error) || Date.now() >= deadline) return false
    }
    await new Promise(resolve => setTimeout(resolve, delay))
    delay = Math.min(delay * 2, LOCK_RETRY_INITIAL_MS)
  }
}

/**
 * Unlink the lock path while it still names this descriptor's file, then
 * close the descriptor, which drops any `flock` on it. Unlinking first keeps
 * a contender from finding the path unlocked while this holder lives, and
 * the identity check never removes a lock another writer published.
 */
async function removeOwnLock(lockPath: string, handle: FileHandle): Promise<void> {
  try {
    if (await isAtPath(handle, lockPath)) await rm(lockPath, { force: true })
  } finally {
    await handle.close()
  }
}

/**
 * Fill a lock file this process just created. The PID record goes in first,
 * so a contender that inspects the file before the `flock` is taken judges
 * it by PID; the kernel-held record then replaces it under the `flock`,
 * where no contender reads. A failure removes the file again.
 * @returns the held lock, or undefined when a contender removed the file before it was locked.
 */
async function claimCreatedFile(handle: FileHandle, lockPath: string, deadline: number): Promise<HeldLock | undefined> {
  let locked = false
  try {
    await handle.writeFile(`${process.pid}\n`)
    // The native binding has no Windows build, so Windows keeps the created-file protocol.
    locked = process.platform !== 'win32' && await lockCreatedFile(handle, deadline)
    if (locked && await isAtPath(handle, lockPath)) {
      // Longer than the PID record, so this positional write replaces it whole.
      await handle.write(kernelLockRecord(), 0)
      return { kind: 'kernel', handle }
    }
  } catch (error) {
    await removeOwnLock(lockPath, handle)
    throw error
  }
  await handle.close()
  return locked ? undefined : { kind: 'created' }
}

/**
 * Inspect the lock file at `lockPath` and remove it when its owner is proven
 * gone. The inspector holds the file's `flock` while it confirms the path
 * still names that file, reads the record, and unlinks it, so two inspectors
 * never both remove one lock and none removes a file that replaced the one
 * it locked. A symlink is never followed.
 * @returns whether the lock path is now free, because its holder released it or the inspector removed it.
 */
async function releasedOrRecovered(lockPath: string): Promise<boolean> {
  let handle: FileHandle
  try {
    handle = await open(lockPath, constants.O_RDWR | constants.O_NOFOLLOW)
  } catch (error) {
    // Only absence frees the path; a symlink, directory, or unreadable file proves no owner gone.
    return errorCode(error) === 'ENOENT'
  }
  try {
    try {
      await tryLockExclusive(handle.fd)
    } catch {
      // Contention means a live kernel holder; any other refusal proves no owner gone.
      return false
    }
    if (!await isAtPath(handle, lockPath) || !isAbandoned(await handle.readFile('utf8'))) return false
    await rm(lockPath, { force: true })
    return true
  } finally {
    await handle.close()
  }
}

/**
 * Acquire the writer lock at `lockPath` by exclusive create. A contender
 * inspects the existing lock between backoff intervals and retries at once
 * when the path frees; the inspection needs the native `flock`, so it never
 * runs on Windows.
 * @throws when the deadline passes while a live or unproven holder keeps the lock, or on a non-contention failure.
 */
async function acquireLock(lockPath: string, deadline: number): Promise<HeldLock> {
  let delay = LOCK_RETRY_INITIAL_MS
  let retriedUnconfirmedPermissionError = false
  for (;;) {
    let handle: FileHandle | undefined
    try {
      handle = await open(lockPath, 'wx', 0o600)
    } catch (error) {
      if (!await isLockContention(error, lockPath)) {
        // Windows can release the competing lock between exclusive create and lstat.
        if (process.platform !== 'win32'
          || errorCode(error) !== 'EPERM'
          || retriedUnconfirmedPermissionError) throw error
        retriedUnconfirmedPermissionError = true
      }
    }
    if (handle !== undefined) {
      const held = await claimCreatedFile(handle, lockPath, deadline)
      if (held !== undefined) return held
      continue
    }
    if (process.platform !== 'win32' && await releasedOrRecovered(lockPath)) continue
    if (Date.now() >= deadline) throw lockTimeout(lockPath)
    await new Promise(resolve => setTimeout(resolve, delay))
    delay = Math.min(delay * 2, LOCK_RETRY_MAX_MS)
  }
}

/** Options for one {@link withFileLock} acquisition. */
export interface FileLockOptions {
  /**
   * Maximum time to wait for the lock, in milliseconds. State one when the
   * holder's operation legitimately runs longer than file work — a credential
   * mutation that refreshes a token performs a network round trip while
   * holding the lock, and leaving the default in place would fail every other
   * writer of the same file for the duration. Waiting is productive: a
   * contender that acquires the lock afterwards re-reads the committed state.
   */
  waitMs?: number
}

/**
 * Hold the cross-process writer lock for `filename` around one operation. The
 * lock is a `wx`-created `<filename>.lock` sibling; paired with the
 * rename-based commit of {@link writeFileAtomic}, readers stay lock-free and
 * only writers contend. `EEXIST` is contention directly; an `EPERM` is
 * contention only when a fresh `lstat` confirms the lock path exists, covering
 * Windows exclusive-create behavior. Windows retries one unconfirmed EPERM
 * because the holder can release before the probe; a repeated unconfirmed
 * permission error is rethrown. Contention backs off exponentially and fails
 * once the deadline passes; a live holder is never displaced, however long it
 * runs. The parent directory must exist.
 *
 * The holder writes its `<pid>\n` record into the created file, as earlier
 * releases did. Where the native `flock` binding is available (Linux and
 * macOS), it then takes the file's `flock` and replaces the record with one
 * naming its PID and host, keeping the `flock` until it unlinks the file on
 * release. A contender that finds the lock takes its `flock`: contention
 * means a live holder. Otherwise the kernel has released any holder's
 * `flock`, so the contender removes a same-host kernel-held record and
 * retries at once. It removes a `<pid>\n` record only when no process has
 * that PID, and it never removes other content or a record from another host.
 * Windows, an unavailable binding, and a filesystem without `flock` keep the
 * `<pid>\n` record and never remove another writer's lock. Holders of either
 * form exclude each other because each keeps the lock path occupied.
 * @param filename - the file whose writers this lock serializes.
 * @param operation - the read-render-commit cycle to run while holding the lock.
 * @param options - acquisition options; omitted waits {@link DEFAULT_LOCK_WAIT_MS}.
 * @returns the operation's result; the lock releases on both outcomes.
 */
export async function withFileLock<T>(
  filename: string,
  operation: () => Promise<T>,
  options?: FileLockOptions,
): Promise<T> {
  const lockPath = `${filename}.lock`
  const held = await acquireLock(lockPath, Date.now() + (options?.waitMs ?? DEFAULT_LOCK_WAIT_MS))
  try {
    return await operation()
  } finally {
    if (held.kind === 'kernel') await removeOwnLock(lockPath, held.handle)
    else await rm(lockPath, { force: true })
  }
}
