/** The profile root `cordis.yml` is replaced atomically, and only when it changed. */

import { spawn } from 'node:child_process'
import { closeSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it, onTestFinished } from 'vitest'
import { PROFILE_ROOT_FILENAME, writeProfileRootConfig } from '../src/profile-boot.ts'

function profileDir(): string {
  const dir = mkdtempSync(join(tmpdir(), 'dsh-profile-root-'))
  onTestFinished(() => { rmSync(dir, { recursive: true, force: true }) })
  return dir
}

/** The empty root config the launcher writes, read back from a fresh profile. */
function rootConfig(): string {
  const dir = profileDir()
  writeProfileRootConfig(dir)
  return readFileSync(join(dir, PROFILE_ROOT_FILENAME), 'utf8')
}

const BAKED = '- id: baked-by-loader-write-back\n  name: some-plugin\n'

describe('writeProfileRootConfig', () => {
  it('creates the empty root config', () => {
    const content = rootConfig()
    expect(content).toMatch(/^# dsh profile root/)
    expect(content.endsWith('\n[]\n')).toBe(true)
  })

  it('leaves an unchanged file alone', () => {
    const dir = profileDir()
    const path = join(dir, PROFILE_ROOT_FILENAME)
    writeProfileRootConfig(dir)
    const before = statSync(path, { bigint: true })

    writeProfileRootConfig(dir)

    const after = statSync(path, { bigint: true })
    expect(after.ino).toBe(before.ino)
    expect(after.mtimeNs).toBe(before.mtimeNs)
    expect(readdirSync(dir)).toEqual([PROFILE_ROOT_FILENAME])
  })

  it('replaces changed content with a new file instead of rewriting it in place', () => {
    const dir = profileDir()
    const path = join(dir, PROFILE_ROOT_FILENAME)
    writeFileSync(path, BAKED)
    const before = statSync(path).ino
    // A reader that opened the old file keeps reading it whole.
    const reader = openSync(path, 'r')
    try {
      writeProfileRootConfig(dir)
      expect(readFileSync(reader, 'utf8')).toBe(BAKED)
    } finally {
      closeSync(reader)
    }
    expect(readFileSync(path, 'utf8')).toBe(rootConfig())
    expect(statSync(path).ino).not.toBe(before)
    expect(readdirSync(dir)).toEqual([PROFILE_ROOT_FILENAME])
  })

  it('removes its temporary file when the replacement fails', () => {
    const dir = profileDir()
    mkdirSync(join(dir, PROFILE_ROOT_FILENAME, 'occupied'), { recursive: true })

    expect(() => { writeProfileRootConfig(dir) }).toThrow()
    expect(readdirSync(dir)).toEqual([PROFILE_ROOT_FILENAME])
  })

  it('never shows a concurrent reader an empty or partial file', async () => {
    const dir = profileDir()
    const path = join(dir, PROFILE_ROOT_FILENAME)
    const expected = rootConfig()
    writeFileSync(path, BAKED)
    const stop = join(dir, 'stop')
    // A separate process, so its reads interleave with the synchronous writes below.
    const reader = spawn(process.execPath, ['-e', `
      const { existsSync, readFileSync } = require('node:fs')
      const [path, stop, ...valid] = process.argv.slice(1)
      let reads = 0, torn = 0
      process.stdout.write('reading\\n')
      while (!existsSync(stop)) {
        const text = readFileSync(path, 'utf8')
        reads++
        if (!valid.includes(text)) torn++
      }
      process.stdout.write(JSON.stringify({ reads, torn }) + '\\n')
    `, path, stop, BAKED, expected], { stdio: ['ignore', 'pipe', 'inherit'] })
    onTestFinished(() => { reader.kill('SIGKILL') })
    let output = ''
    reader.stdout.setEncoding('utf8')
    const started = new Promise<void>((resolve) => {
      reader.stdout.on('data', (chunk: string) => {
        output += chunk
        if (output.includes('reading\n')) resolve()
      })
    })
    const exited = new Promise<void>((resolve) => { reader.once('exit', () => { resolve() }) })
    await started

    const deadline = Date.now() + 1_000
    for (let round = 0; Date.now() < deadline; round++) {
      // Stand in for the Loader's write-back, itself atomic so that only this writer is on trial.
      writeFileSync(`${path}.baked`, BAKED)
      renameSync(`${path}.baked`, path)
      writeProfileRootConfig(dir)
    }
    writeFileSync(stop, '')
    await exited

    const { reads, torn } = JSON.parse(output.split('\n').filter(Boolean).at(-1)!) as { reads: number; torn: number }
    expect(reads).toBeGreaterThan(0)
    expect(torn).toBe(0)
  })
})
