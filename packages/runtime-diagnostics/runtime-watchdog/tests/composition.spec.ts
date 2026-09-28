/**
 * The watchdog through a real Loader composition: the base bundle's row shape
 * with `dshHomePath`, the Cordis logger, process-global report settings, and
 * child processes that stall the event loop or run out of heap.
 */

import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { format } from 'node:util'
import { Context } from '@deepseek-ai/cordis'
import { boot } from '@deepseek-ai/dsh-app-boot'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import * as RuntimeWatchdog from '../src/index.ts'
import type { WatchdogRecord } from '../src/watchdog.ts'

const repoRoot = fileURLToPath(new URL('../../../../', import.meta.url))
const childScript = fileURLToPath(new URL('./fixtures/child.ts', import.meta.url))
const tsxLoader = fileURLToPath(import.meta.resolve('tsx'))
const CHILD_TIMEOUT_MS = 90_000

/** The base bundle row, with optional extra config lines. */
function watchdogRow(extra = ''): string {
  return `- id: runtime-watchdog
  name: cordis:runtime-watchdog
  config:
    directory: !!js dshHomePath('diagnostics')
${extra}`
}

let root: string
let home: string
let work: string
let config: string

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'runtime-watchdog-composition-'))
  home = join(root, 'home')
  work = join(root, 'work')
  config = join(root, 'cordis.yml')
  await mkdir(work)
})

afterEach(async () => {
  await rm(root, { recursive: true, force: true })
})

async function records(): Promise<WatchdogRecord[]> {
  const diagnostics = join(home, 'diagnostics')
  const files = (await readdir(diagnostics)).filter(file => file.endsWith('.jsonl'))
  const text = (await Promise.all(files.map(file => readFile(join(diagnostics, file), 'utf8')))).join('')
  return text.trim().split('\n').map(line => JSON.parse(line) as WatchdogRecord)
}

describe('Loader composition', () => {
  let savedHome: string | undefined

  beforeEach(() => { savedHome = process.env.DSH_HOME })

  afterEach(() => {
    if (savedHome === undefined) delete process.env.DSH_HOME
    else process.env.DSH_HOME = savedHome
  })

  it('records into the Harness-home diagnostics directory and logs through the Cordis logger', async () => {
    process.env.DSH_HOME = home
    await writeFile(config, watchdogRow(`    intervalMs: 100
    heapFraction: 0
`))
    const { directory, reportOnFatalError } = process.report
    const warnings: string[] = []
    const ctx = await boot('runtime-watchdog-test', config, undefined, (host) => {
      host.loader.builtins['runtime-watchdog'] = RuntimeWatchdog
      host.logger.exporter({
        levels: { default: 2 },
        export: ({ name, type, args }) => {
          if (name === 'runtime-watchdog' && type === 'warn') warnings.push(format(...args as [string, ...unknown[]]))
        },
      })
    })
    try {
      await vi.waitFor(async () => { expect(await records()).not.toEqual([]) })
      // This test process did not start Node with --report-exclude-env.
      expect({ directory: process.report.directory, reportOnFatalError: process.report.reportOnFatalError })
        .toEqual({ directory, reportOnFatalError })
    } finally {
      await ctx.fiber.dispose()
    }
    expect((await records())[0]).toMatchObject({ kind: 'heap-limit', pid: process.pid })
    expect(warnings[0]).toContain('fatal-error reports are not armed')
    expect(warnings).toContainEqual(expect.stringContaining(`recorded in ${join(home, 'diagnostics', 'watchdog.')}`))
  })

  it('rejects a relative directory, which would resolve inside the workspace', () => {
    expect(() => { RuntimeWatchdog.apply(new Context(), { directory: 'diagnostics' }) }).toThrow('absolute path')
  })
})

interface ChildResult {
  code: number | null
  signal: NodeJS.Signals | null
  stdout: string
  stderr: string
}

/**
 * Run the fixture under tsx. A deliberate abort would otherwise leave a core
 * dump: a pipe core handler ignores a zero RLIMIT_CORE, but not a 1-byte one.
 */
function runChild(scenario: 'stall' | 'oom', nodeArgs: string[] = []): Promise<ChildResult> {
  const env: NodeJS.ProcessEnv = { ...process.env, DSH_HOME: home, TSX_TSCONFIG_PATH: join(repoRoot, 'tsconfig.json') }
  delete env.NODE_OPTIONS
  const argv = [process.execPath, ...nodeArgs, '--import', tsxLoader, childScript, config, scenario]
  const prlimit = ['/usr/bin/prlimit', '/bin/prlimit'].find(path => existsSync(path))
  const [command, args] = prlimit === undefined
    ? ['/bin/sh', ['-c', 'ulimit -c 0; exec "$@"', 'sh', ...argv]]
    : [prlimit, ['--core=1:1', '--', ...argv]]
  const child = spawn(command, args, { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], timeout: CHILD_TIMEOUT_MS })
  let stdout = ''
  let stderr = ''
  child.stdout.setEncoding('utf8').on('data', (chunk: string) => { stdout += chunk })
  child.stderr.setEncoding('utf8').on('data', (chunk: string) => { stderr += chunk })
  return new Promise((resolve, reject) => {
    child.once('error', reject)
    child.once('close', (code, signal) => { resolve({ code, signal, stdout, stderr }) })
  })
}

/** Report names only: a failure message must not print a report's contents. */
async function reportFiles(directory: string): Promise<string[]> {
  return (await readdir(directory).catch(() => [])).filter(file => /^report\..*\.json$/.test(file))
}

describe.skipIf(process.platform === 'win32')('booted process', () => {
  it('records a real stall and a heap crossing without writing to stdout', async () => {
    await writeFile(config, watchdogRow(`    intervalMs: 100
    eventLoopDelayMs: 100
    sustainedMs: 400
    heapFraction: 0
`))
    const result = await runChild('stall')
    expect(result.stdout).toBe('')
    expect(result.code, result.stderr).toBe(0)
    expect(result.stderr).not.toContain('recorded in')

    expect(await readdir(join(home, 'diagnostics'))).toEqual([expect.stringMatching(/^watchdog\.\d{8}\.\d{6}\.\d+\.jsonl$/)])
    const written = await records()
    expect(written.map(record => record.kind).sort()).toEqual(['event-loop-delay', 'heap-limit'])
    const stall = written.find(record => record.kind === 'event-loop-delay')!
    expect(stall.eventLoop.maxMs).toBeGreaterThanOrEqual(400)
    expect(stall.memory.rss).toBeGreaterThan(0)
  }, CHILD_TIMEOUT_MS)

  it('writes an out-of-memory report without environment or network data to the diagnostics directory', async () => {
    await writeFile(config, watchdogRow())
    const result = await runChild('oom', ['--max-old-space-size=128', '--report-exclude-env', '--report-exclude-network'])
    expect(result.stdout).toBe('')
    expect(result.stderr).toContain('heap out of memory')

    const diagnostics = join(home, 'diagnostics')
    const reports = await reportFiles(diagnostics)
    expect(reports).toHaveLength(1)
    const report = JSON.parse(await readFile(join(diagnostics, reports[0]!), 'utf8')) as Record<string, unknown>
    expect(Object.keys(report)).toContain('javascriptHeap')
    expect(Object.keys(report)).not.toContain('environmentVariables')
    expect(Object.keys(report.header as object)).not.toContain('networkInterfaces')
    expect(await readdir(work)).toEqual([])
  }, CHILD_TIMEOUT_MS)

  it('writes no out-of-memory report when Node could include environment variables', async () => {
    await writeFile(config, watchdogRow())
    const result = await runChild('oom', ['--max-old-space-size=128'])
    expect(result.stderr).toContain('heap out of memory')
    expect(await reportFiles(join(home, 'diagnostics'))).toEqual([])
    expect(await readdir(work)).toEqual([])
  }, CHILD_TIMEOUT_MS)
})
