/**
 * Unit tests for the Windows durable namespace helper with a mocked kernel32
 * binding. The real JSONL suite exercises the helper on native Windows; these
 * tests keep the Win32 error mapping and race handling covered on every host.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import { link, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const MOVEFILE_WRITE_THROUGH = 0x00000008
const ERROR_FILE_NOT_FOUND = 2
const ERROR_PATH_NOT_FOUND = 3
const ERROR_ACCESS_DENIED = 5
const ERROR_NOT_SAME_DEVICE = 17
const ERROR_FILE_EXISTS = 80
const ERROR_INVALID_NAME = 123
const ERROR_ALREADY_EXISTS = 183

type MoveFileExW = (existing: string, replacement: string, flags: number, setLastError: (code: number) => void) => number

const roots: string[] = []

function stripNamespace(path: string): string {
  if (path.startsWith('\\\\?\\UNC\\')) return `\\\\${path.slice('\\\\?\\UNC\\'.length)}`
  if (path.startsWith('\\\\?\\')) return path.slice('\\\\?\\'.length)
  return path
}

async function tempRoot(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'dsh-jsonl-win32-'))
  roots.push(dir)
  return dir
}

async function importWithMove(moveFileExW: MoveFileExW): Promise<typeof import('../src/win32.ts')> {
  vi.resetModules()
  vi.doMock('koffi', () => {
    let lastError = 0
    const setLastError = (code: number): void => { lastError = code }
    const move: MoveFileExW = (existing, replacement, flags, setError) => {
      const ok = moveFileExW(existing, replacement, flags, setError)
      lastError = ok === 0 ? lastError : 0
      return ok
    }
    return {
      default: {
        load: () => ({
          func: (_convention: string, name: string, result: string) => {
            if (name === 'MoveFileExW') return (existing: string, replacement: string, flags: number) => {
              expect(result).toBe('int')
              const ok = move(existing, replacement, flags, setLastError)
              return ok
            }
            return () => lastError
          },
        }),
      },
    }
  })
  return import('../src/win32.ts')
}

async function importWithError(code: number): Promise<typeof import('../src/win32.ts')> {
  vi.resetModules()
  vi.doMock('koffi', () => ({
    default: {
      load: () => ({
        func: (_convention: string, name: string) => {
          if (name === 'MoveFileExW') return () => 0
          return () => code
        },
      }),
    },
  }))
  return import('../src/win32.ts')
}

async function importWithFilesystemMove(): Promise<typeof import('../src/win32.ts')> {
  return importWithMove((existing, replacement, flags, setLastError) => {
    expect(flags).toBe(MOVEFILE_WRITE_THROUGH)
    const from = stripNamespace(existing)
    const to = stripNamespace(replacement)
    if (!existsSync(from)) { setLastError(ERROR_FILE_NOT_FOUND); return 0 }
    if (existsSync(to)) { setLastError(ERROR_ALREADY_EXISTS); return 0 }
    renameSync(from, to)
    return 1
  })
}

afterEach(async () => {
  vi.doUnmock('koffi')
  vi.doUnmock('node:fs/promises')
  vi.doUnmock('node:path')
  vi.resetModules()
  for (const root of roots.splice(0)) await rm(root, { recursive: true, force: true })
})

describe('Windows durable namespace helpers', () => {
  it('keeps drive-root probes native while namespacing descendants', async () => {
    const probes: string[] = []
    vi.resetModules()
    vi.doMock('node:fs/promises', async (importOriginal) => {
      const actual = await importOriginal<typeof import('node:fs/promises')>()
      return {
        ...actual,
        stat: async (path: string) => {
          probes.push(path)
          return { isDirectory: () => true }
        },
      }
    })
    vi.doMock('node:path', async (importOriginal) => {
      const actual = await importOriginal<typeof import('node:path')>()
      return {
        ...actual,
        join: (...paths: string[]) => actual.win32.join(...paths),
        parse: (path: string) => actual.win32.parse(path),
        resolve: (...paths: string[]) => actual.win32.resolve(...paths),
        toNamespacedPath: (path: string) => actual.win32.toNamespacedPath(path),
      }
    })
    const { ensureDurableDirectoryWin32 } = await import('../src/win32.ts')

    await ensureDurableDirectoryWin32('C:\\existing')

    expect(probes).toEqual(['C:\\', '\\\\?\\C:\\existing'])
  })

  it('publishes a new file with write-through MoveFileExW semantics', async () => {
    const { publishNewFileWin32 } = await importWithFilesystemMove()
    const root = await tempRoot()
    const tmp = join(root, 'log.tmp')
    const final = join(root, 'log.jsonl')
    await writeFile(tmp, 'content')

    await publishNewFileWin32(tmp, final)
    expect(existsSync(tmp)).toBe(false)
    expect(readFileSync(final, 'utf8')).toBe('content')
  })

  it('maps Win32 publish failures to Node-style errno codes', async () => {
    const cases = [
      [ERROR_FILE_NOT_FOUND, 'ENOENT'],
      [ERROR_PATH_NOT_FOUND, 'ENOENT'],
      [ERROR_ACCESS_DENIED, 'EACCES'],
      [ERROR_NOT_SAME_DEVICE, 'EXDEV'],
      [ERROR_FILE_EXISTS, 'EEXIST'],
      [ERROR_ALREADY_EXISTS, 'EEXIST'],
      [ERROR_INVALID_NAME, 'EINVAL'],
      [9999, 'EIO'],
    ] as const
    for (const [win32Code, code] of cases) {
      const { publishNewFileWin32 } = await importWithError(win32Code)
      await expect(publishNewFileWin32('from', 'to')).rejects.toMatchObject({ code, win32Code, path: 'from', dest: 'to' })
    }
  })

  it('creates missing directories through staging siblings and tolerates an already-created race', async () => {
    const root = await tempRoot()
    const raced = join(root, 'raced')
    const { ensureDurableDirectoryWin32 } = await importWithMove((existing, replacement, flags, setLastError) => {
      expect(flags).toBe(MOVEFILE_WRITE_THROUGH)
      const from = stripNamespace(existing)
      const to = stripNamespace(replacement)
      if (to === raced) {
        mkdirSync(to)
        setLastError(ERROR_ALREADY_EXISTS)
        return 0
      }
      if (!existsSync(from)) { setLastError(ERROR_FILE_NOT_FOUND); return 0 }
      if (existsSync(to)) { setLastError(ERROR_ALREADY_EXISTS); return 0 }
      renameSync(from, to)
      return 1
    })

    await ensureDurableDirectoryWin32(join(root, 'a', 'b'))
    expect(existsSync(join(root, 'a', 'b'))).toBe(true)
    await ensureDurableDirectoryWin32(join(root, 'a', 'b'))
    await ensureDurableDirectoryWin32(raced)
    expect(existsSync(raced)).toBe(true)
  })

  it('keeps staging names valid for a maximum-length target component', async () => {
    const { ensureDurableDirectoryWin32 } = await importWithFilesystemMove()
    const root = await tempRoot()
    const target = join(root, 'x'.repeat(255))

    await ensureDurableDirectoryWin32(target)
    expect(existsSync(target)).toBe(true)
  })

  it('surfaces directory publication failures other than an existing-target race', async () => {
    const { ensureDurableDirectoryWin32 } = await importWithError(ERROR_ACCESS_DENIED)
    const root = await tempRoot()

    await expect(ensureDurableDirectoryWin32(join(root, 'denied'))).rejects.toMatchObject({ code: 'EACCES' })
  })

  it('rejects a non-directory component instead of treating it as missing', async () => {
    const { ensureDurableDirectoryWin32 } = await importWithFilesystemMove()
    const root = await tempRoot()
    const blocked = join(root, 'blocked')
    writeFileSync(blocked, 'x')

    await expect(ensureDurableDirectoryWin32(join(blocked, 'child'))).rejects.toMatchObject({ code: 'ENOTDIR' })
  })
})

async function importWithLock(bindings: {
  createFileW?: (...args: unknown[]) => bigint | null
  lockFileEx?: (...args: unknown[]) => number
  unlockFileEx?: (...args: unknown[]) => number
  closeHandle?: (handle: bigint) => number
  lastError?: number
}): Promise<typeof import('../src/win32.ts')> {
  vi.resetModules()
  vi.doMock('koffi', () => ({
    default: {
      pointer: () => 'void*',
      load: () => ({
        func: (_convention: string, name: string) => {
          if (name === 'CreateFileW') return bindings.createFileW ?? (() => 7n)
          if (name === 'LockFileEx') return bindings.lockFileEx ?? (() => 1)
          if (name === 'UnlockFileEx') return bindings.unlockFileEx ?? (() => 1)
          if (name === 'CloseHandle') return bindings.closeHandle ?? (() => 1)
          if (name === 'MoveFileExW') return () => 1
          return () => bindings.lastError ?? 0 // GetLastError
        },
      }),
    },
  }))
  return import('../src/win32.ts')
}

describe('Windows session file lock', () => {
  it.skipIf(process.platform !== 'win32')('locks file identity across aliases and refuses deletion while held', async () => {
    vi.doUnmock('koffi')
    vi.resetModules()
    const { acquireLockHandleWin32, releaseLockHandleWin32 } = await import('../src/win32.ts')
    const root = await tempRoot()
    const path = join(root, 'session.lock')
    const alias = join(root, 'alias.lock')
    const held = await acquireLockHandleWin32(path)
    try {
      await link(path, alias)
      await expect(acquireLockHandleWin32(alias)).rejects.toMatchObject({ code: 'EBUSY' })
      await expect(rm(path)).rejects.toBeDefined()
    } finally {
      await releaseLockHandleWin32(held)
    }
    const successor = await acquireLockHandleWin32(alias)
    await releaseLockHandleWin32(successor)
  })

  it('opens session.lock without share-delete and locks byte zero immediately', async () => {
    const opened: unknown[][] = []
    const locked: unknown[][] = []
    const { acquireLockHandleWin32 } = await importWithLock({
      createFileW: (...args) => { opened.push(args); return 7n },
      lockFileEx: (...args) => { locked.push(args); return 1 },
    })
    const held = await acquireLockHandleWin32('C:\\s\\session.lock')
    expect(held.handle).toBe(7n)
    expect(opened).toHaveLength(1)
    expect(opened[0]?.[1]).toBe(0xC0000000) // GENERIC_READ | GENERIC_WRITE
    expect(opened[0]?.[2]).toBe(0x3) // share read/write, never delete
    expect(opened[0]?.[4]).toBe(4) // OPEN_ALWAYS
    expect(locked).toHaveLength(1)
    expect(locked[0]?.slice(0, 5)).toEqual([7n, 0x3, 0, 1, 0])
    expect(held.overlapped).toEqual(Buffer.alloc(32))
    expect(locked[0]?.[5]).toBe(held.overlapped)
  })

  it('maps byte-range contention to EBUSY and closes the probe handle', async () => {
    const closed: bigint[] = []
    const { acquireLockHandleWin32 } = await importWithLock({
      lockFileEx: () => 0,
      lastError: 33, // ERROR_LOCK_VIOLATION
      closeHandle: (handle) => { closed.push(handle); return 1 },
    })
    await expect(acquireLockHandleWin32('C:\\s\\session.lock')).rejects.toMatchObject({ code: 'EBUSY', win32Code: 33 })
    expect(closed).toEqual([7n])
  })

  it('surfaces open and lock failures with Win32 codes', async () => {
    for (const invalid of [null, 0n, -1n, 0xFFFFFFFFFFFFFFFFn]) {
      const failed = await importWithLock({ createFileW: () => invalid, lastError: 5 })
      await expect(failed.acquireLockHandleWin32('C:\\s\\session.lock')).rejects.toMatchObject({ code: 'EACCES', win32Code: 5, syscall: 'CreateFileW' })
    }
    const locked = await importWithLock({ lockFileEx: () => 0, lastError: 5 })
    await expect(locked.acquireLockHandleWin32('C:\\s\\session.lock')).rejects.toMatchObject({ code: 'EACCES', win32Code: 5, syscall: 'LockFileEx' })
  })

  it('unlocks the same byte range before closing, even when unlock fails', async () => {
    const order: string[] = []
    const working = await importWithLock({
      unlockFileEx: (...args) => { order.push(`unlock:${String(args[0])}`); return 1 },
      closeHandle: (handle) => { order.push(`close:${String(handle)}`); return 1 },
    })
    const held = await working.acquireLockHandleWin32('C:\\s\\session.lock')
    await working.releaseLockHandleWin32(held)
    expect(order).toEqual(['unlock:7', 'close:7'])
    const failed = await importWithLock({ unlockFileEx: () => 0, lastError: 5,
      closeHandle: (handle) => { order.push(`close-failed:${String(handle)}`); return 1 } })
    const other = await failed.acquireLockHandleWin32('C:\\s\\session.lock')
    await expect(failed.releaseLockHandleWin32(other)).rejects.toMatchObject({ code: 'EACCES', syscall: 'UnlockFileEx' })
    expect(order.at(-1)).toBe('close-failed:7')
  })
})
