import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, symlinkSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished, vi } from 'vitest'
import { resolveExampleLaunch } from '@deepseek-ai/dsh-loader-smoke'
import { compileCacheDirectory, enableCompileCache, flushCompileCache } from '../src/compile-cache.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const { version } = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')) as { version: string }
const posix = process.getuid !== undefined

function temporary(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  onTestFinished(() => { rmSync(dir, { recursive: true, force: true }) })
  return dir
}

/** Every regular file below `dir`. */
function files(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath, entry.name))
}

describe('enableCompileCache', () => {
  it('enables the cache in the chosen directory', () => {
    const enable = vi.fn((directory?: string) => ({ directory }))
    expect(enableCompileCache({}, { enableCompileCache: enable }, () => '/cache')).toBe(true)
    expect(enable).toHaveBeenCalledExactlyOnceWith('/cache')
  })

  it.each(['1', ''])('leaves the cache and the disk alone when NODE_DISABLE_COMPILE_CACHE=%j', (value) => {
    const enable = vi.fn(() => ({}))
    const locate = vi.fn(() => '/cache')
    expect(enableCompileCache({ NODE_DISABLE_COMPILE_CACHE: value }, { enableCompileCache: enable }, locate)).toBe(false)
    expect(enable).not.toHaveBeenCalled()
    expect(locate).not.toHaveBeenCalled()
  })

  it('keeps the directory the caller chose through NODE_COMPILE_CACHE', () => {
    const enable = vi.fn(() => ({ directory: '/chosen' }))
    const locate = vi.fn(() => '/cache')
    expect(enableCompileCache({ NODE_COMPILE_CACHE: '/chosen' }, { enableCompileCache: enable }, locate)).toBe(true)
    expect(enable).toHaveBeenCalledExactlyOnceWith(undefined)
    expect(locate).not.toHaveBeenCalled()
  })

  it('starts without the cache when it is unavailable', () => {
    const enable = vi.fn(() => ({ directory: '/cache' }))
    // No usable directory.
    expect(enableCompileCache({}, { enableCompileCache: enable }, () => undefined)).toBe(false)
    expect(enable).not.toHaveBeenCalled()
    // A directory that cannot be inspected.
    expect(enableCompileCache({}, { enableCompileCache: enable }, () => { throw new Error('EACCES') })).toBe(false)
    // Node reports a failure, which carries no directory.
    expect(enableCompileCache({}, { enableCompileCache: () => ({}) }, () => '/cache')).toBe(false)
    // An unexpected throw.
    expect(enableCompileCache({}, { enableCompileCache: () => { throw new Error('boom') } }, () => '/cache')).toBe(false)
    // A Node without the API.
    expect(enableCompileCache({}, {}, () => '/cache')).toBe(false)
    expect(() => { flushCompileCache({}) }).not.toThrow()
    expect(() => { flushCompileCache({ flushCompileCache: () => { throw new Error('ENOSPC') } }) }).not.toThrow()
  })
})

describe.skipIf(!posix)('compileCacheDirectory', () => {
  const uid = process.getuid?.() ?? -1

  it('creates the directory owner-only when absent', () => {
    const base = temporary('dsh-compile-cache-base-')
    const directory = compileCacheDirectory(base, uid)
    expect(directory).toBe(join(base, 'node-compile-cache'))
    expect(statSync(directory!).mode & 0o777).toBe(0o700)
  })

  it.each([0o755, 0o775])('accepts an existing directory this user owns with mode %s', (mode) => {
    const base = temporary('dsh-compile-cache-base-')
    mkdirSync(join(base, 'node-compile-cache'))
    chmodSync(join(base, 'node-compile-cache'), mode)
    expect(compileCacheDirectory(base, uid)).toBe(join(base, 'node-compile-cache'))
  })

  it('refuses a directory other users can write, one another user owns, or a link', () => {
    const writable = temporary('dsh-compile-cache-base-')
    mkdirSync(join(writable, 'node-compile-cache'))
    chmodSync(join(writable, 'node-compile-cache'), 0o777)
    expect(compileCacheDirectory(writable, uid)).toBeUndefined()

    const foreign = temporary('dsh-compile-cache-base-')
    mkdirSync(join(foreign, 'node-compile-cache'))
    expect(compileCacheDirectory(foreign, uid + 1)).toBeUndefined()

    const linked = temporary('dsh-compile-cache-base-')
    mkdirSync(join(linked, 'elsewhere'))
    symlinkSync(join(linked, 'elsewhere'), join(linked, 'node-compile-cache'))
    expect(compileCacheDirectory(linked, uid)).toBeUndefined()
  })
})

describe('dsh entry', () => {
  async function runVersion(env: NodeJS.ProcessEnv): Promise<void> {
    const launch = resolveExampleLaunch({ srcBin: dshBinScript, configArgs: ['--version'], tsconfigPath, env })
    const result = await execa(launch.command, launch.args, {
      env: { ...process.env, ...launch.env, NODE_COMPILE_CACHE: undefined, NODE_DISABLE_COMPILE_CACHE: undefined, ...env },
      extendEnv: false,
      stdin: 'ignore',
      reject: false,
      timeout: 60_000,
    })
    expect(result.stderr).toBe('')
    expect(result.stdout).toBe(version)
    expect(result.exitCode).toBe(0)
  }

  it('writes the compile cache under the temporary directory it starts with', async () => {
    const temp = temporary('dsh-compile-cache-entry-')
    await runVersion({ TMPDIR: temp, TMP: temp, TEMP: temp })
    // Commander and the launcher's own modules load after the cache is on.
    expect(files(join(temp, 'node-compile-cache')).length).toBeGreaterThan(10)
  }, 90_000)

  it('starts normally and writes nothing when the cache is disabled', async () => {
    const temp = temporary('dsh-compile-cache-entry-')
    await runVersion({ TMPDIR: temp, TMP: temp, TEMP: temp, NODE_DISABLE_COMPILE_CACHE: '1' })
    expect(existsSync(join(temp, 'node-compile-cache'))).toBe(false)
  }, 90_000)
})
