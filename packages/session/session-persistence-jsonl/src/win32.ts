/**
 * Windows durable namespace helpers for the JSONL backend.
 *
 * POSIX publishes a newly-created log by creating a directory entry and then
 * fsyncing the parent directory. Windows does not expose that parent-directory
 * fsync contract through Node, so the Windows path uses the native durable
 * namespace primitive instead: create a staging object in the target directory
 * and publish it with `MoveFileExW(..., MOVEFILE_WRITE_THROUGH)` without
 * replacement or cross-volume copy fallback.
 *
 * @module dsh-session-persistence-jsonl/win32
 */

import { mkdtemp, rm, stat } from 'node:fs/promises'
import { join, parse, resolve, toNamespacedPath } from 'node:path'
import { isENOENT } from 'bake-util-values'

type MoveFileExW = (existing: string, replacement: string, flags: number) => number
type NativeHandle = bigint
type CreateFileW = (
  path: string, access: number, share: number, security: null,
  disposition: number, flags: number, template: null,
) => NativeHandle | null
type LockFileEx = (
  handle: NativeHandle, flags: number, reserved: number,
  bytesLow: number, bytesHigh: number, overlapped: Buffer,
) => number
type UnlockFileEx = (
  handle: NativeHandle, reserved: number,
  bytesLow: number, bytesHigh: number, overlapped: Buffer,
) => number
type CloseHandle = (handle: NativeHandle) => number
type GetLastError = () => number

interface Win32Bindings {
  moveFileExW: MoveFileExW
  createFileW: CreateFileW
  lockFileEx: LockFileEx
  unlockFileEx: UnlockFileEx
  closeHandle: CloseHandle
  getLastError: GetLastError
}

interface Win32ErrnoException extends NodeJS.ErrnoException {
  win32Code: number
  dest: string
}

const MOVEFILE_WRITE_THROUGH = 0x00000008
const GENERIC_READ = 0x80000000
const GENERIC_WRITE = 0x40000000
const FILE_SHARE_READ = 0x00000001
const FILE_SHARE_WRITE = 0x00000002
const OPEN_ALWAYS = 4
const LOCKFILE_FAIL_IMMEDIATELY = 0x1
const LOCKFILE_EXCLUSIVE_LOCK = 0x2
const ERROR_FILE_NOT_FOUND = 2
const ERROR_PATH_NOT_FOUND = 3
const ERROR_ACCESS_DENIED = 5
const ERROR_NOT_SAME_DEVICE = 17
const ERROR_SHARING_VIOLATION = 32
const ERROR_LOCK_VIOLATION = 33
const ERROR_FILE_EXISTS = 80
const ERROR_INVALID_NAME = 123
const ERROR_ALREADY_EXISTS = 183

let bindings: Win32Bindings | undefined

/** Open lock file and zeroed byte-range record retained until release. */
export interface Win32LockHandle {
  readonly handle: NativeHandle
  readonly overlapped: Buffer
  readonly path: string
}

/** Load the small Win32 API lazily so non-Windows processes never load Koffi. */
async function win32(): Promise<Win32Bindings> {
  if (bindings !== undefined) return bindings
  const koffi = (await import('koffi')).default
  const kernel32 = koffi.load('kernel32.dll')
  const pointer = 'void*'
  bindings = {
    moveFileExW: kernel32.func('__stdcall', 'MoveFileExW', 'int', ['str16', 'str16', 'uint']) as MoveFileExW,
    createFileW: kernel32.func('__stdcall', 'CreateFileW', pointer, ['str16', 'uint32', 'uint32', pointer, 'uint32', 'uint32', pointer]) as CreateFileW,
    lockFileEx: kernel32.func('__stdcall', 'LockFileEx', 'int', [pointer, 'uint32', 'uint32', 'uint32', 'uint32', pointer]) as LockFileEx,
    unlockFileEx: kernel32.func('__stdcall', 'UnlockFileEx', 'int', [pointer, 'uint32', 'uint32', 'uint32', pointer]) as UnlockFileEx,
    closeHandle: kernel32.func('__stdcall', 'CloseHandle', 'int', [pointer]) as CloseHandle,
    getLastError: kernel32.func('__stdcall', 'GetLastError', 'uint', []) as GetLastError,
  }
  return bindings
}

function errnoCode(win32Code: number): string {
  switch (win32Code) {
    case ERROR_FILE_NOT_FOUND:
    case ERROR_PATH_NOT_FOUND:
      return 'ENOENT'
    case ERROR_ACCESS_DENIED:
      return 'EACCES'
    case ERROR_NOT_SAME_DEVICE:
      return 'EXDEV'
    case ERROR_SHARING_VIOLATION:
    case ERROR_LOCK_VIOLATION:
      return 'EBUSY'
    case ERROR_FILE_EXISTS:
    case ERROR_ALREADY_EXISTS:
      return 'EEXIST'
    case ERROR_INVALID_NAME:
      return 'EINVAL'
    default:
      return 'EIO'
  }
}

function win32Error(syscall: string, win32Code: number, path: string, dest: string): Win32ErrnoException {
  const code = errnoCode(win32Code)
  const error = new Error(`${syscall} ${code} (Win32 ${win32Code}): ${path} -> ${dest}`) as Win32ErrnoException
  error.code = code
  error.errno = win32Code
  error.syscall = syscall
  error.path = path
  error.dest = dest
  error.win32Code = win32Code
  return error
}

function isEEXIST(error: unknown): boolean {
  return (error as NodeJS.ErrnoException | null)?.code === 'EEXIST'
}

async function assertDirectory(path: string): Promise<boolean> {
  try {
    // A bare drive root is already short, and Node rejects its extended-length
    // spelling as EISDIR. Descendants retain the namespace for long-path probes.
    const probe = path === parse(path).root ? path : toNamespacedPath(path)
    const info = await stat(probe)
    if (info.isDirectory()) return true
    const error = new Error(`path exists but is not a directory: ${path}`) as NodeJS.ErrnoException
    error.code = 'ENOTDIR'
    error.path = path
    throw error
  } catch (error) {
    if (isENOENT(error)) return false
    throw error
  }
}

/**
 * Publish `existing` at `replacement` with Windows write-through rename
 * semantics. The destination must not already exist; the move must stay within
 * the volume (no copy fallback flag is set).
 * @param existing - the synced staging path to move.
 * @param replacement - the final path, which must not already exist.
 */
export async function publishNewFileWin32(existing: string, replacement: string): Promise<void> {
  const api = await win32()
  const ok = api.moveFileExW(toNamespacedPath(existing), toNamespacedPath(replacement), MOVEFILE_WRITE_THROUGH)
  if (ok === 0) throw win32Error('MoveFileExW', api.getLastError(), existing, replacement)
}

/**
 * Acquire an immediate one-byte exclusive file lock on `session.lock`.
 * CreateFileW allows readers and writers but not deletion, so another process
 * cannot replace the lock path while the handle is open. The file lock spans
 * Windows login sessions and the kernel drops it if the process dies.
 * @param path - the session's lock file path.
 * @returns the held file handle and OVERLAPPED record for release.
 */
export async function acquireLockHandleWin32(path: string): Promise<Win32LockHandle> {
  const api = await win32()
  const nativePath = toNamespacedPath(path)
  const handle = api.createFileW(nativePath, (GENERIC_READ | GENERIC_WRITE) >>> 0,
    FILE_SHARE_READ | FILE_SHARE_WRITE, null, OPEN_ALWAYS, 0, null)
  if (handle === null || handle === 0n || handle === -1n || handle === 0xFFFFFFFFFFFFFFFFn) {
    throw win32Error('CreateFileW', api.getLastError(), path, path)
  }
  // Koffi's pointer argument needs a real, zeroed OVERLAPPED record even for
  // a synchronous handle. Offset 0 and a one-byte range select one lock.
  const overlapped = Buffer.alloc(32)
  if (api.lockFileEx(handle, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
    0, 1, 0, overlapped) === 0) {
    const code = api.getLastError()
    api.closeHandle(handle)
    throw win32Error('LockFileEx', code, path, path)
  }
  return { handle, overlapped, path }
}

/**
 * Release the byte-range lock and close its file handle. The lock file stays
 * on disk so later writers address the same file.
 * @param held - the lock returned by {@link acquireLockHandleWin32}.
 */
export async function releaseLockHandleWin32(held: Win32LockHandle): Promise<void> {
  const api = await win32()
  const unlocked = api.unlockFileEx(held.handle, 0, 1, 0, held.overlapped)
  const unlockCode = unlocked === 0 ? api.getLastError() : 0
  const closed = api.closeHandle(held.handle)
  if (unlocked === 0) throw win32Error('UnlockFileEx', unlockCode, held.path, held.path)
  if (closed === 0) throw win32Error('CloseHandle', api.getLastError(), held.path, held.path)
}

/**
 * Create `target` and its missing ancestors with durable Windows namespace
 * publication. Each missing directory is first created as a random staging
 * sibling, then moved to its final name with `MOVEFILE_WRITE_THROUGH`; races
 * with another creator are accepted only after verifying the winner is a
 * directory.
 * @param target - the absolute directory path to create durably when absent.
 */
export async function ensureDurableDirectoryWin32(target: string): Promise<void> {
  const absolute = resolve(target)
  const root = parse(absolute).root
  await assertDirectory(root)

  const segments = absolute.slice(root.length).split(/[\\/]+/).filter(part => part.length > 0)
  let current = root
  for (const segment of segments) {
    const next = join(current, segment)
    if (!await assertDirectory(next)) await createLeafDirectoryWin32(current, next)
    current = next
  }
}

async function createLeafDirectoryWin32(parent: string, target: string): Promise<void> {
  // Keep the staging component independent of the target basename so a legal
  // 255-byte target component does not make mkdtemp's sibling name too long.
  const staging = await mkdtemp(toNamespacedPath(join(parent, '.dsh-mkdir-')))
  try {
    await publishNewFileWin32(staging, target)
  } catch (error) {
    await rm(staging, { recursive: true, force: true })
    if (isEEXIST(error) && await assertDirectory(target)) return
    throw error
  }
}
