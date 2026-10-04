/** The launcher's handling of unhandled rejections once startup has committed. */
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { AppRejection } from '@deepseek-ai/dsh-cmdline'
import { afterAll, describe, expect, it } from 'vitest'
import {
  createLateRejectionReporter, LATE_REJECTION_BURST, LATE_REJECTION_WINDOW_MS, warningLine,
  type LateRejectionOptions, type LateRejectionRecord,
} from '../src/late-rejections.ts'

const roots: string[] = []
afterAll(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

/** A reporter over a fresh directory, a controlled clock, and captured channels. */
function harness(overrides: Partial<LateRejectionOptions> = {}) {
  const root = mkdtempSync(join(tmpdir(), 'dsh-late-rejections-'))
  roots.push(root)
  const directory = join(root, 'diagnostics')
  const clock = { now: 1_000 }
  const presented: AppRejection[] = []
  const warnings: string[] = []
  const stderr: string[] = []
  let surface = false
  const report = createLateRejectionReporter({
    directory,
    present: (rejection) => { presented.push(rejection); return surface },
    warn: (message) => { warnings.push(message) },
    stderr: { write: (chunk: string) => { stderr.push(chunk) } },
    now: () => clock.now,
    pid: 4242,
    ...overrides,
  })
  const records = (): LateRejectionRecord[] => {
    let files: string[]
    try { files = readdirSync(directory) } catch { return [] }
    return files.flatMap(file => readFileSync(join(directory, file), 'utf8').trim().split('\n')
      .map(line => JSON.parse(line) as LateRejectionRecord))
  }
  return {
    root, directory, clock, presented, warnings, stderr, report, records,
    showOnSurface: () => { surface = true },
  }
}

describe('createLateRejectionReporter', () => {
  it('records the error\'s name, message, and stack alone in an owner-only file, logs it, and writes one warning line', () => {
    const run = harness()
    const error = Object.assign(new Error('socket hang up'), {
      headers: { authorization: 'Bearer secret-token' }, code: 'ECONNRESET',
    })
    run.report(error)
    const files = readdirSync(run.directory)
    expect(files).toHaveLength(1)
    expect(files[0]).toMatch(/^rejections\.\d{8}\.\d{6}\.4242\.jsonl$/u)
    const path = join(run.directory, files[0]!)
    if (process.platform !== 'win32') {
      expect(statSync(path).mode & 0o777).toBe(0o600)
      expect(statSync(run.directory).mode & 0o777).toBe(0o700)
    }
    const text = readFileSync(path, 'utf8')
    expect(text).not.toContain('secret-token')
    expect(text).not.toContain('ECONNRESET')
    expect(run.records()).toEqual([{
      time: expect.any(String), kind: 'unhandled-rejection', pid: 4242, uptimeMs: 1_000,
      error: { name: 'Error', message: 'socket hang up', stack: error.stack }, suppressed: 0,
    }])
    expect(run.presented).toEqual([{ summary: 'Error: socket hang up', record: path }])
    expect(run.warnings).toEqual([`unhandled rejection after startup: Error: socket hang up; the session continues; recorded in ${path}`])
    expect(run.stderr).toEqual([
      `dsh: warning: unhandled rejection after startup: Error: socket hang up; the session continues; details in ${path}\n`,
    ])
  })

  it('leaves the warning line out when a surface showed the rejection', () => {
    const run = harness()
    run.showOnSurface()
    run.report(new TypeError('cannot read properties of undefined'))
    expect(run.presented).toHaveLength(1)
    expect(run.presented[0]!.summary).toBe('TypeError: cannot read properties of undefined')
    expect(run.stderr).toEqual([])
  })

  it('shows each distinct error once and counts its repeats into the next record', () => {
    const run = harness()
    const throwSite = (): Error => new Error('listener failed')
    const first = throwSite()
    run.report(first)
    run.report(throwSite())
    run.report(first)
    expect(run.records()).toHaveLength(1)
    expect(run.presented).toHaveLength(1)
    expect(run.stderr).toHaveLength(1)
    // The same message from another throw site is a different error.
    run.report(new Error('listener failed'))
    expect(run.presented).toHaveLength(2)
    expect(run.records().map(record => record.suppressed)).toEqual([0, 2])
  })

  it('handles at most a burst of distinct errors per window and counts the rest', () => {
    const run = harness()
    for (let index = 0; index <= LATE_REJECTION_BURST; index++) run.report(new Error(`distinct ${index}`))
    expect(run.presented).toHaveLength(LATE_REJECTION_BURST)
    expect(run.stderr).toHaveLength(LATE_REJECTION_BURST)
    expect(run.records()).toHaveLength(LATE_REJECTION_BURST)
    // The one beyond the burst was not remembered as shown, so it is reported once the window moves on.
    run.clock.now += LATE_REJECTION_WINDOW_MS
    run.report(new Error(`distinct ${LATE_REJECTION_BURST}`))
    expect(run.presented).toHaveLength(LATE_REJECTION_BURST + 1)
    expect(run.records().at(-1)).toMatchObject({ error: { message: `distinct ${LATE_REJECTION_BURST}` }, suppressed: 1 })
  })

  it('keeps one line of a summary, without control characters, and bounds it', () => {
    const run = harness()
    run.report(new Error(`\u001b[2Jcleared\u009b31m\nsecond line ${'x'.repeat(500)}`))
    run.report(new Error('y'.repeat(500)))
    // The escape characters go; the printable rest of each sequence is harmless text.
    expect(run.presented[0]!.summary).toBe('Error:  [2Jcleared 31m')
    expect(run.presented[1]!.summary).toHaveLength(200)
    expect(run.presented[1]!.summary.endsWith('…')).toBe(true)
    // The record keeps the whole message, up to its own bound, as escaped JSON.
    expect(run.records()[0]!.error.message).toContain('second line')
  })

  it('keeps only the value of a primitive reason and the type tag of any other object', () => {
    const run = harness()
    run.report('timeout')
    run.report({ token: 'secret-token', toString: () => 'secret-token' })
    run.report(undefined)
    expect(run.presented.map(rejection => rejection.summary)).toEqual([
      'Non-error rejection: timeout', 'Non-error rejection: [object Object]', 'Non-error rejection: undefined',
    ])
    expect(JSON.stringify(run.records())).not.toContain('secret-token')
  })

  it('still logs and shows a rejection whose record cannot be written', () => {
    const run = harness()
    // A file where the directory should be: neither mkdir nor append can succeed.
    writeFileSync(run.directory, '')
    run.report(new Error('late failure'))
    expect(run.presented).toEqual([{ summary: 'Error: late failure' }])
    expect(run.warnings).toHaveLength(2)
    expect(run.warnings[0]).toMatch(/^could not write /u)
    expect(run.warnings[1]).toBe('unhandled rejection after startup: Error: late failure; the session continues')
    expect(run.stderr).toEqual(['dsh: warning: unhandled rejection after startup: Error: late failure; the session continues\n'])
  })

  it('falls back to the warning line when presenting or logging throws, and never throws itself', () => {
    const stderr: string[] = []
    const run = harness({
      present: () => { throw new Error('the app is disposed') },
      warn: () => { throw new Error('the logger is gone') },
      stderr: { write: (chunk: string) => { stderr.push(chunk) } },
    })
    expect(() => { run.report(new Error('during disposal')) }).not.toThrow()
    expect(stderr).toHaveLength(1)
    expect(stderr[0]).toContain('Error: during disposal; the session continues; details in ')
    expect(run.records()).toHaveLength(1)
    const failingStderr = harness({ stderr: { write: () => { throw new Error('EPIPE') } } })
    expect(() => { failingStderr.report(new Error('pipe closed')) }).not.toThrow()
    expect(failingStderr.records()).toHaveLength(1)
  })

  it('appends to one file for the life of the process', () => {
    const run = harness()
    run.report(new Error('first'))
    run.report(new Error('second'))
    expect(readdirSync(run.directory)).toHaveLength(1)
    expect(run.records().map(record => record.error.message)).toEqual(['first', 'second'])
  })
})

describe('warningLine', () => {
  it('names the record when there is one', () => {
    expect(warningLine({ summary: 'Error: x', record: '/home/u/.bake/diagnostics/r.jsonl' }))
      .toBe('dsh: warning: unhandled rejection after startup: Error: x; the session continues; details in /home/u/.bake/diagnostics/r.jsonl\n')
    expect(warningLine({ summary: 'Error: x' })).toBe('dsh: warning: unhandled rejection after startup: Error: x; the session continues\n')
  })
})
