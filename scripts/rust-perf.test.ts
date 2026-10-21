import { afterEach, describe, expect, it } from 'bun:test'
import { createHash } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { parseArm } from './rust-perf/arms.ts'
import { proposeBudgets, refuseDocsPath, type BudgetProposal } from './rust-perf/budgets.ts'
import { answerEvents, DELTA_COUNT, STREAM_DONE, STREAM_START } from './rust-perf/model.ts'
import { cpuTicksBetween, processTree, readProcStat, sampleTree, survivors, TreeMonitor, type ProcStat } from './rust-perf/proc-tree.ts'
import { REPORT_SCHEMA, summarize, type Report } from './rust-perf/report.ts'
import type { SampleRecord } from './rust-perf/sample.ts'
import { seedPi } from './rust-perf/seed-pi.ts'
import { projectKey03, seedSession03 } from './rust-perf/seed-session03.ts'
import { bootstrapInterval, distribution, mad, median, p95, seededRandom } from './rust-perf/stats.ts'
import { AnsiStripper, Terminal } from './rust-perf/terminal.ts'
import { expectedMarkers, history, marker, WORKLOADS, type WorkloadName } from './rust-perf/workloads.ts'

const repository = resolve(import.meta.dir, '..')
const sha256 = (bytes: Uint8Array): string => createHash('sha256').update(bytes).digest('hex')

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})
async function temporary(prefix: string): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), prefix))
  cleanups.push(() => rm(dir, { recursive: true, force: true }))
  return dir
}

describe('rust-perf statistics', () => {
  it('takes the median of odd and even counts and the nearest-rank p95', () => {
    expect(median([3, 1, 2])).toBe(2)
    expect(median([4, 1, 3, 2])).toBe(2.5)
    // Below 20 samples the nearest-rank p95 is the maximum; at 20 it is the 19th smallest.
    expect(p95([5, 1, 9, 3])).toBe(9)
    expect(p95(Array.from({ length: 20 }, (_, index) => index + 1))).toBe(19)
    expect(p95(Array.from({ length: 100 }, (_, index) => index + 1))).toBe(95)
    expect(mad([1, 2, 3, 4, 100])).toBe(1)
  })

  it('refuses empty and non-finite samples', () => {
    expect(() => median([])).toThrow(RangeError)
    expect(() => p95([1, Number.NaN])).toThrow(RangeError)
    expect(() => bootstrapInterval([], median)).toThrow(RangeError)
    expect(() => bootstrapInterval([1], median, { resamples: 0, confidence: 0.95, seed: 1 })).toThrow(RangeError)
  })

  it('draws a reproducible bootstrap that brackets the estimate', () => {
    const random = seededRandom(7)
    const values = Array.from({ length: 20 }, () => 100 + random() * 20)
    const first = distribution(values)
    expect(distribution([...values].reverse())).toEqual(first)
    expect(first.samples).toBe(20)
    expect(first.medianCi!.low).toBeLessThanOrEqual(first.median!)
    expect(first.medianCi!.high).toBeGreaterThanOrEqual(first.median!)
    expect(first.p95Ci!.high).toBeLessThanOrEqual(first.max!)
    expect(first.p95Ci!.low).toBeGreaterThanOrEqual(first.min!)
    // Another seed gives another, still valid, interval.
    const middle = (sortedValues: readonly number[]): number => sortedValues[sortedValues.length >> 1] ?? 0
    const other = bootstrapInterval(values, middle, { resamples: 2000, confidence: 0.95, seed: 8 })
    expect(other.low).toBeLessThanOrEqual(other.high)
    expect(distribution([5, 5, 5]).medianCi).toEqual({ low: 5, high: 5 })
  })

  it('keeps an empty distribution explicit', () => {
    expect(distribution([])).toEqual({ samples: 0, min: null, median: null, p95: null, max: null, mad: null, medianCi: null, p95Ci: null })
  })
})

/**
 * SHA-256 of the logs Bake 0.3's own fixture writer
 * (`apps/tui/packages/app/performance/seed.ts`, run through the JSONL
 * persistence backend with `compression: 'none'`) persisted for each workload
 * with the working directory `/tmp/perfseed/<workload>/workspace`.
 */
const SESSION03_SHA256: Record<Exclude<WorkloadName, 'fresh'>, string> = {
  small: 'f1babf1ff01d71caffa8ea9d66a1db098c0d65b6eab625192dfd819fcf39a23b',
  typical: '06303d9a5b24b20e1ee86bfba423f63cda45aa11c5ef7b18b372f9cfd17d8aa0',
  tail: '9782e4339cbd2e45344a828ae48d24a6615c816218d8e7c3314b218e69ea1911',
  tools: '0b38d757bbf5745939c5adda29513f6eeabd0a3e20bc382705c8d165534ba917',
  'large-output': '7df9d05f6f2202ded5cc8e385c5985b25362455730a4a8a9af2379fbae9839e6',
}

/** SHA-256 of the Pi sessions this generator writes for the same working directories. */
const PI_SHA256: Record<Exclude<WorkloadName, 'fresh'>, string> = {
  small: 'db707031a5ddfc6de0a1feb006bff1331d9e9d71209a3e4ac7e40c7bdcc76794',
  typical: 'db483be8432bddcc25b0e63142b36fb03d1fafa429a7213f33a6b45220c3ba71',
  tail: '5c5452e45e6a97b178ef1dd49f218b4e62613857248333c8dbe0162fdc61d0bd',
  tools: 'ecf02c1fa8f7cea2ed4daa8309545875c540fa81da0be3ff958767cdd342e11f',
  'large-output': '18218317c99a7632bf89bdbbdd41b1ed33392adc8e97bb72c459259f831afa75',
}

describe('rust-perf seeds', () => {
  it('writes the 0.3 fixture writer\'s bytes and dimensions for every workload', () => {
    for (const name of Object.keys(SESSION03_SHA256) as (keyof typeof SESSION03_SHA256)[]) {
      const cwd = `/tmp/perfseed/${name}/workspace`
      const seed = seedSession03(WORKLOADS[name], cwd)
      expect(sha256(seed.bytes)).toBe(SESSION03_SHA256[name])
      expect(seed.relativePath).toBe(`--tmp-perfseed-${name}-workspace--/tui-perf-session/session.v3.jsonl`)
    }
    // The 0.3 driver's README records these sizes for 500 and 2000 turns.
    expect(seedSession03(WORKLOADS.typical, '/w').dimensions).toMatchObject({ events: 3251, deltaCount: 20500, toolCount: 125, historyMarkerCount: 501 })
    expect(seedSession03(WORKLOADS.tail, '/w').dimensions).toMatchObject({ events: 13001, deltaCount: 82000, toolCount: 500, historyMarkerCount: 2001 })
    expect(seedSession03(WORKLOADS['large-output'], '/w').dimensions).toMatchObject({ events: 79, deltaCount: 32928, compactRecordCount: 90, historyMarkerCount: 193, maxAssistantTextBytes: 65536 })
    expect(seedSession03(WORKLOADS.fresh, '/w').bytes.length).toBe(0)
  })

  it('encodes project directories as the 0.3 backend does', () => {
    expect(projectKey03('/home/a b/ü')).toBe('--home-a~0020b-~00FC--')
    expect(projectKey03('C:\\\\work//x')).toBe('--C-work-x--')
    expect(projectKey03('/')).toBe('--root--')
    expect(() => projectKey03('')).toThrow()
  })

  it('writes the same logical history as a deterministic Pi session', () => {
    for (const name of Object.keys(PI_SHA256) as (keyof typeof PI_SHA256)[]) {
      const cwd = `/tmp/perfseed/${name}/workspace`
      const seed = seedPi(WORKLOADS[name], cwd)
      expect(sha256(seed.bytes)).toBe(PI_SHA256[name])
      expect(sha256(seedPi(WORKLOADS[name], cwd).bytes)).toBe(PI_SHA256[name])
    }
    const spec = WORKLOADS.small
    interface Entry {
      id: string
      parentId: string | null
      message: { role: string; toolCallId?: string; content: { type: string; id?: string }[] }
    }
    const lines = new TextDecoder().decode(seedPi(spec, '/w').bytes).trimEnd().split('\n').map(line => JSON.parse(line) as unknown)
    expect(lines[0]).toMatchObject({ type: 'session', version: 3, cwd: '/w' })
    const entries = lines.slice(1) as Entry[]
    // Every entry is the child of the one before it.
    entries.forEach((entry, index) => expect(entry.parentId).toBe(index === 0 ? null : entries[index - 1]!.id))
    const { dimensions } = history(spec)
    expect(entries.filter(entry => entry.message.role === 'user')).toHaveLength(dimensions.turns)
    expect(entries.filter(entry => entry.message.role === 'toolResult')).toHaveLength(dimensions.toolCount)
    const calls = entries.flatMap(entry => entry.message.role === 'assistant' ? entry.message.content.filter(block => block.type === 'toolCall').map(block => block.id) : [])
    expect(calls).toEqual(entries.filter(entry => entry.message.role === 'toolResult').map(entry => entry.message.toolCallId))
    const text = JSON.stringify(entries)
    for (const expected of expectedMarkers(dimensions)) expect(text.split(expected)).toHaveLength(2)
    expect(seedPi(WORKLOADS.fresh, '/w').bytes.length).toBe(0)
  })

  it('ends a history with tools on the final result\'s marker', () => {
    const { turns, dimensions } = history(WORKLOADS.tools)
    expect(dimensions.historyMarkerCount).toBe(dimensions.assistantMarkerCount + 1)
    expect(turns.at(-1)!.calls.at(-1)!.output.endsWith(`${marker(501)}\n`)).toBe(true)
    expect(history(WORKLOADS.small).dimensions.historyMarkerCount).toBe(50)
    expect(() => history({ turns: 1, assistantTextBytes: 100 })).toThrow()
  })
})

describe('rust-perf faux model', () => {
  it('streams 100 deltas that open and close with the stream markers', () => {
    const { head, deltas, tail } = answerEvents(1, 'synthetic')
    expect(deltas).toHaveLength(DELTA_COUNT)
    const data = [...head, ...deltas, ...tail].map(event => JSON.parse(event.split('\ndata: ')[1]!) as { type: string; sequence_number: number; delta?: string })
    expect(data.map(event => event.sequence_number)).toEqual(data.map((_, index) => index))
    const text = data.filter(event => event.type === 'response.output_text.delta').map(event => event.delta).join('')
    expect(text.startsWith(STREAM_START)).toBe(true)
    expect(text.endsWith(STREAM_DONE)).toBe(true)
    expect(data.at(-1)!.type).toBe('response.completed')
  })
})

/** A fake `/proc` entry: its `stat` line, `VmRSS`, and per-task children. */
interface FakeProcess { pid: number; ppid: number; comm: string; rssKiB?: number; ticks: number; start: number; children?: number[] }

async function fakeProc(processes: FakeProcess[], childrenFiles: boolean): Promise<string> {
  const root = await temporary('rust-perf-proc-')
  for (const entry of processes) {
    const dir = join(root, String(entry.pid))
    await mkdir(join(dir, 'task', String(entry.pid)), { recursive: true })
    // Fields 3-22 of proc(5): state, ppid, ..., utime (14), stime (15), ..., starttime (22).
    const fields = ['S', entry.ppid, 0, 0, 0, 0, 0, 0, 0, 0, 0, entry.ticks, 1, 0, 0, 20, 0, 1, 0, entry.start]
    await writeFile(join(dir, 'stat'), `${entry.pid} (${entry.comm}) ${fields.join(' ')} 0 0\n`)
    await writeFile(join(dir, 'status'), `Name:\t${entry.comm}\n${entry.rssKiB === undefined ? '' : `VmRSS:\t  ${entry.rssKiB} kB\n`}Threads:\t1\n`)
    if (childrenFiles) await writeFile(join(dir, 'task', String(entry.pid), 'children'), (entry.children ?? []).map(child => `${child} `).join(''))
  }
  return root
}

const FAKE_TREE: FakeProcess[] = [
  { pid: 100, ppid: 1, comm: 'bake', rssKiB: 1000, ticks: 10, start: 500, children: [101, 102] },
  { pid: 101, ppid: 100, comm: 'sh -c (x) y', rssKiB: 200, ticks: 3, start: 510, children: [103] },
  { pid: 102, ppid: 100, comm: 'kworker', ticks: 1, start: 520 },
  { pid: 103, ppid: 101, comm: 'sleep', rssKiB: 30, ticks: 0, start: 530 },
  // A stale child listing whose pid now belongs to another parent.
  { pid: 104, ppid: 1, comm: 'other', rssKiB: 9999, ticks: 0, start: 540 },
  { pid: 1, ppid: 0, comm: 'init', rssKiB: 5000, ticks: 0, start: 1, children: [100, 104] },
]

describe('rust-perf /proc sampler', () => {
  it('reads a synthetic tree by children files and by a parent scan alike', async () => {
    const tree = FAKE_TREE.map(entry => entry.pid === 101 ? { ...entry, children: [103, 104] } : entry)
    for (const childrenFiles of [true, false]) {
      const proc = await fakeProc(tree, childrenFiles)
      const sample = sampleTree(100, proc, childrenFiles)
      expect(sample.members.map(member => member.pid)).toEqual([100, 101, 102, 103])
      // The kernel thread has no VmRSS and counts 0; the stale 104 is not a member.
      expect(sample.rssKiB).toBe(1230)
      expect(readProcStat(101, proc)).toEqual({ pid: 101, ppid: 100, cpuTicks: 4, startTime: 510 })
      expect(processTree(999, proc, childrenFiles)).toEqual([])
    }
  })

  it('counts CPU ticks per process identity, a restarted pid in full', () => {
    const stat = (pid: number, cpuTicks: number, startTime = pid): ProcStat => ({ pid, ppid: 1, cpuTicks, startTime })
    expect(cpuTicksBetween([stat(1, 10), stat(2, 5)], [stat(1, 14), stat(2, 5), stat(3, 2)])).toBe(6)
    expect(cpuTicksBetween([stat(2, 5)], [stat(2, 7, 99)])).toBe(7)
  })

  it('follows a real process tree, its peak RSS, and the members left behind', async () => {
    const child = Bun.spawn(['sh', '-c', 'sleep 30 & sleep 30 & wait'], { stdout: 'ignore', stderr: 'ignore' })
    let left: ProcStat[] = []
    cleanups.push(async () => {
      for (const member of [...left, ...processTree(child.pid)]) {
        try { process.kill(member.pid, 'SIGKILL') } catch { /* gone */ }
      }
      child.kill('SIGKILL')
      await child.exited
    })
    const monitor = new TreeMonitor(child.pid, 10)
    await monitor.ready()
    const deadline = performance.now() + 5000
    while (processTree(child.pid).length < 3 && performance.now() < deadline) await Bun.sleep(5)
    const members = processTree(child.pid)
    expect(members.map(member => member.pid)[0]).toBe(child.pid)
    expect(members).toHaveLength(3)
    const sample = sampleTree(child.pid)
    expect(sample.rssKiB).toBeGreaterThan(0)
    await Bun.sleep(50)
    const observed = await monitor.stop()
    expect(observed.ticks).toBeGreaterThan(2)
    expect(observed.peakRssKiB).toBeGreaterThan(0)
    expect([...observed.pids].sort()).toEqual(members.map(member => member.pid).sort())
    // Killing only the shell leaves its two sleeps alive under the same identity.
    child.kill('SIGKILL')
    await child.exited
    left = survivors(members, child.pid)
    expect(left.map(member => member.pid).sort()).toEqual(members.slice(1).map(member => member.pid).sort())
  })
})

describe('rust-perf terminal observation', () => {
  it('strips control sequences split across chunks', () => {
    const stripper = new AnsiStripper()
    const chunks = ['a\x1b[3', '8;5;8mb\x1b]0;ti', 'tle\x07c\x1b]8;;x\x1b', '\\d\x1b(Be\r\nf\tg\x1b', 'Mh']
    expect(chunks.map(chunk => stripper.push(chunk)).join('')).toBe('abcde\n\nf gh')
  })

  it('sees bracketed paste, raw mode, split markers once, and a watched echo in a real PTY', async () => {
    const script = 'saved=$(stty -g); stty raw -echo; printf "\\033[?2004h"; printf "H000"; sleep 0.1; printf "01_E"; sleep 0.1; printf "ND H00002_END\\n"; read -r line; printf "got:%s" "$line"; stty "$saved"; exit 0'
    const terminal = new Terminal(['sh', '-c', script], tmpdir(), { PATH: process.env.PATH ?? '/usr/bin:/bin' })
    cleanups.push(() => terminal.close())
    await terminal.wait('raw', () => terminal.bracketedPasteAt !== undefined && terminal.modesChanged, 10_000)
    await terminal.wait('markers', () => terminal.markers.size === 2, 10_000)
    expect([...terminal.markers.keys()]).toEqual(['H00001_END', 'H00002_END'])
    expect(terminal.markerOccurrences).toBe(2)
    const echo = terminal.watch('got:probe')
    terminal.send('probe\n')
    const at = await terminal.seen(echo, 'echo', 10_000)
    expect(at).toBeGreaterThan(terminal.startedAt)
    const exit = await terminal.exited
    expect(exit).toMatchObject({ code: 0, signal: null })
    expect(terminal.restored).toBe(true)
  })
})

function syntheticRecord(arm: string, workload: WorkloadName, iteration: number, startupMs: number, peakRssMiB: number): SampleRecord {
  return {
    arm, workload, mode: 'cold', iteration, warmup: false, loadAverage1m: 0.1, dimensions: undefined, ok: true,
    metrics: {
      inputReadyMs: 5, startupMs, markersAtFirstInput: 0, replayMs: null, readyMs: startupMs,
      firstDeltaMs: 20, streamInputMs: 2, streamMs: 1000,
      idleCpuMs: 10, idleCpuPercent: 0.2, shutdownMs: 40, peakRssMiB,
    },
    exit: { code: 0, signal: null }, terminalRestored: true, leakedProcesses: [], markerOccurrences: 0, outputBytes: 1, modelRequests: 1,
    rssSampling: { ticks: 10, maxGapMs: 10, pids: 1 },
  }
}

function syntheticReport(samples: SampleRecord[]): Report {
  const arms = [...new Set(samples.map(sample => sample.arm))]
  return {
    schema: REPORT_SCHEMA, createdAt: '2026-10-21T00:00:00.000Z', command: [], interrupted: false, host: {}, harness: {},
    method: { samplesPerCell: 20, order: '', viewport: { columns: 120, rows: 40 }, paceMs: 10, idleMs: 5000, rssIntervalMs: 10, timeoutMs: 1, maxLoad: 1, cache: { cold: '', warm: '' }, endpoints: '' },
    arms: arms.map(label => ({ label, kind: 'ts', build: {} })), workloads: ['fresh'], modes: ['cold'], summary: summarize(samples), samples,
  }
}

describe('rust-perf budgets', () => {
  it('proposes 10% over the baseline median and p95 and no leaks or failures', () => {
    const samples = Array.from({ length: 20 }, (_, index) => syntheticRecord('ts:/b', 'fresh', index, 100 + index, 200 + index))
    samples.push({ arm: 'ts:/b', workload: 'fresh', mode: 'cold', iteration: 20, warmup: false, loadAverage1m: 0, dimensions: undefined, ok: false, error: 'timed out' })
    samples.push({ ...syntheticRecord('ts:/b', 'fresh', -1, 9999, 9999), warmup: true })
    const report = syntheticReport(samples)
    expect(report.summary[0]).toMatchObject({ completed: 20, failed: 1 })
    const proposal = proposeBudgets(report, new TextEncoder().encode('{}'), '/r.json')
    expect(proposal.status).toBe('proposed')
    expect(proposal.approval.approvedBy).toBeNull()
    const cell = proposal.cells[0]!
    expect(cell.metrics.startupMs).toMatchObject({ samples: 20, baselineMedian: 109.5, baselineP95: 118, maxMedian: 120.45, maxP95: 129.8 })
    expect(cell.metrics.peakRssMiB).toMatchObject({ maxMedian: 230.45, maxP95: 239.8 })
    expect(cell.metrics.replayMs).toBeUndefined()
    expect(cell.observed.idleCpuMs?.median).toBe(10)
    expect(cell).toMatchObject({ maxLeakedProcesses: 0, maxFailedSamples: 0 })
    expect(proposal.warnings).toEqual(['fresh/cold: 1 baseline sample(s) failed; investigate before approving'])
  })

  it('needs the baseline arm named when a report has several, and never writes docs', () => {
    const report = syntheticReport([syntheticRecord('ts:/b', 'fresh', 0, 1, 1), syntheticRecord('native:/n', 'fresh', 0, 1, 1)])
    expect(() => proposeBudgets(report, new Uint8Array(), '/r.json')).toThrow('name the baseline with --arm')
    expect(proposeBudgets(report, new Uint8Array(), '/r.json', 'native:/n').warnings).toContain('fresh/cold: 1 completed sample(s), fewer than the proposed 20')
    expect(() => proposeBudgets({ ...report, schema: 'other' } as unknown as Report, new Uint8Array(), '/r.json')).toThrow('not a rust-perf report')
    expect(() => refuseDocsPath(join(repository, 'docs/roadmap/rust-0.4/budgets.json'), repository)).toThrow('never writes under docs/')
    expect(() => refuseDocsPath(join(repository, 'docs'), repository)).toThrow()
    expect(() => refuseDocsPath(join(repository, '.preflight/perf/b.json'), repository)).not.toThrow()
    expect(() => refuseDocsPath(join(repository, 'docs-other/b.json'), repository)).not.toThrow()
  })

  it('parses arms', () => {
    expect(parseArm('native:rust/target/debug/bake', '/r')).toEqual({ label: 'native:rust/target/debug/bake', kind: 'native', path: '/r/rust/target/debug/bake' })
    expect(parseArm('ts:/w', '/r')).toMatchObject({ kind: 'ts', path: '/w' })
    expect(() => parseArm('node:/x')).toThrow()
  })
})

const nativeBinary = join(repository, 'rust/target/debug/bake')

describe('rust-perf native smoke', () => {
  it.skipIf(!existsSync(nativeBinary))('measures one fresh sample of rust/target/debug/bake and proposes budgets from it', async () => {
    const dir = await temporary('rust-perf-smoke-')
    const out = join(dir, 'report.json')
    const run = Bun.spawn(['bun', join(repository, 'scripts/rust-perf.ts'), '--arm', `native:${nativeBinary}`, '--workload', 'fresh', '--samples', '1', '--mode', 'cold',
      '--idle-ms', '200', '--timeout-ms', '20000', '--max-load', '100000', '--out', out], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    const [code, stdout, stderr] = await Promise.all([run.exited, new Response(run.stdout).text(), new Response(run.stderr).text()])
    expect({ code, stderr }).toEqual({ code: 0, stderr: '' })
    expect(stdout).toContain('1 sample(s), 0 failed')
    const report = JSON.parse(readFileSync(out, 'utf8')) as Report
    expect(report.schema).toBe(REPORT_SCHEMA)
    expect(report.interrupted).toBe(false)
    expect(report.arms[0]!.build).toMatchObject({ sha256: sha256(readFileSync(nativeBinary)) })
    const [sample] = report.samples
    if (sample === undefined || !sample.ok) throw new Error(`the smoke sample failed: ${JSON.stringify(sample)}`)
    expect(sample.terminalRestored).toBe(true)
    expect(sample.leakedProcesses).toEqual([])
    expect(sample.modelRequests).toBe(1)
    for (const metric of ['inputReadyMs', 'startupMs', 'readyMs', 'firstDeltaMs', 'streamInputMs', 'streamMs', 'shutdownMs', 'peakRssMiB'] as const) expect(sample.metrics[metric]).toBeGreaterThan(0)
    expect(sample.metrics.replayMs).toBeNull()
    expect(report.summary[0]!.metrics.startupMs.samples).toBe(1)
    const proposalPath = join(dir, 'budgets.json')
    const budgets = Bun.spawnSync(['bun', join(repository, 'scripts/rust-perf.ts'), 'budgets', '--baseline', out, '--out', proposalPath], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    expect(budgets.exitCode).toBe(0)
    const proposal = JSON.parse(await readFile(proposalPath, 'utf8')) as BudgetProposal
    expect(proposal.cells[0]!.metrics.startupMs!.maxMedian).toBeGreaterThan(sample.metrics.startupMs)
    const refused = Bun.spawnSync(['bun', join(repository, 'scripts/rust-perf.ts'), 'budgets', '--baseline', out, '--out', join(repository, 'docs/rust-perf-budgets.json')], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    expect(refused.exitCode).toBe(2)
    expect(existsSync(join(repository, 'docs/rust-perf-budgets.json'))).toBe(false)
  }, 60_000)
})
