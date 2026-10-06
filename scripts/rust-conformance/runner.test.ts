import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { launch } from './driver.ts'

const RUNNER = join(import.meta.dirname, 'runner.ts')
const CONFORMANCE = join(import.meta.dirname, '..', '..', 'conformance')
// Process-bound cases take the scripts-unit lane budget.
const BUDGET = 30_000

let root: string
let work: string
beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'bake-conformance-runner-'))
  work = join(root, 'work')
  await mkdir(work)
})
afterEach(async () => {
  await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
})

async function runRunner(input: string | Uint8Array) {
  const result = await launch([process.execPath, RUNNER], work, { PATH: process.env.PATH ?? '', HOME: root },
    typeof input === 'string' ? Buffer.from(input) : input, BUDGET - 5_000, undefined)
  // A stopped child would otherwise read as an exit-status mismatch.
  expect({ timedOut: result.timedOut, signal: result.signal, overflow: result.stdoutOverflow })
    .toEqual({ timedOut: false, signal: null, overflow: false })
  return { ...result, stdout: result.stdout.toString('utf8'), stderr: result.stderr.toString('utf8') }
}

const listFiles = async (): Promise<string[]> =>
  (await readdir(work, { recursive: true, withFileTypes: true })).filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath, entry.name).slice(work.length + 1).replaceAll('\\', '/')).sort()

describe('synthetic Bun runner', () => {
  test('relays the allow fixture exactly and writes its nested file', async () => {
    const fixture = JSON.parse(await readFile(join(CONFORMANCE, 'fixtures', 'allow-write.json'), 'utf8'))
    const result = await runRunner(JSON.stringify(fixture.input))
    expect(result.exitCode).toBe(0)
    expect(result.stdout.endsWith('}\n')).toBe(true)
    expect(result.stdout.indexOf('\n')).toBe(result.stdout.length - 1)
    const { prompts, events, permissions } = fixture.input
    expect(JSON.parse(result.stdout)).toEqual({ schema: 'bake/synthetic-conformance/observation', version: 1, prompts, events, permissions })
    expect(await listFiles()).toEqual(['src/result.txt'])
    expect(await readFile(join(work, 'src', 'result.txt'), 'utf8')).toBe('ready\n')
  }, BUDGET)

  test('denied writes change nothing', async () => {
    const fixture = JSON.parse(await readFile(join(CONFORMANCE, 'fixtures', 'deny-write.json'), 'utf8'))
    const result = await runRunner(JSON.stringify(fixture.input))
    expect(result.exitCode).toBe(0)
    expect(await listFiles()).toEqual([])
  }, BUDGET)

  test('every shared invalid input exits 2 with one diagnostic line', async () => {
    const names = (await readdir(join(CONFORMANCE, 'invalid'))).sort()
    expect(names.length).toBeGreaterThan(0)
    for (const name of names) {
      const result = await runRunner(await readFile(join(CONFORMANCE, 'invalid', name)))
      expect({ name, exitCode: result.exitCode, stdout: result.stdout }).toEqual({ name, exitCode: 2, stdout: '' })
      expect(result.stderr).toMatch(/^invalid input: [^\n]+\n$/)
    }
  }, BUDGET)

  test('validates the whole input before the first write', async () => {
    const input = {
      schema: 'bake/synthetic-conformance/input', version: 1, prompts: [], events: [],
      permissions: [{ id: 'ok', path: 'first.txt', decision: 'allow' }],
      writes: [{ path: 'first.txt', text: 'x', permission: 'ok' }, { path: 'second.txt', text: 'x', permission: 'missing' }],
    }
    const result = await runRunner(JSON.stringify(input))
    expect(result.exitCode).toBe(2)
    expect(await listFiles()).toEqual([])
  }, BUDGET)

  test('an I/O failure exits 1', async () => {
    await writeFile(join(work, 'blocker'), '')
    const input = {
      schema: 'bake/synthetic-conformance/input', version: 1, prompts: [], events: [],
      permissions: [{ id: 'nested', path: 'blocker/child.txt', decision: 'allow' }],
      writes: [{ path: 'blocker/child.txt', text: 'x', permission: 'nested' }],
    }
    const result = await runRunner(JSON.stringify(input))
    expect(result.exitCode).toBe(1)
    expect(result.stdout).toBe('')
    expect(result.stderr).toMatch(/^I\/O failure: /)
  }, BUDGET)

  test('oversized stdin exits 2', async () => {
    const result = await runRunner(Buffer.alloc(256 * 1024 + 1, 0x20))
    expect(result.exitCode).toBe(2)
    expect(result.stderr).toContain('exceeds')
  }, BUDGET)
})
