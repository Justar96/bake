import { afterEach, describe, expect, it } from 'bun:test'
import { createHash } from 'node:crypto'
import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { cargoProfile, parseArm, type Arm } from './rust-perf/arms.ts'
import { proposeBudgets, refuseDocsPath, type BudgetProposal } from './rust-perf/budgets.ts'
import { answerEvents, DELTA_COUNT, STREAM_DONE, STREAM_START } from './rust-perf/model.ts'
import { cpuTicksBetween, processTree, readProcStat, sampleTree, sessionMembers, survivors, TreeMonitor, type ProcStat } from './rust-perf/proc-tree.ts'
import { REPORT_SCHEMA, summarize, type Report } from './rust-perf/report.ts'
import { PREPARE_ITERATION, runSample, type SampleKey, type SampleRecord, type SampleSettings, type WarmState } from './rust-perf/sample.ts'
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

/** Poll `condition` every 5 ms for at most `ms`. */
async function until(condition: () => boolean, ms = 10_000): Promise<void> {
  const deadline = performance.now() + ms
  while (!condition() && performance.now() < deadline) await Bun.sleep(5)
}

/** Strip string chunks through `stripper`, as a PTY would deliver their bytes. */
function strip(stripper: AnsiStripper, chunks: readonly string[]): string {
  const decoder = new TextDecoder()
  let text = ''
  for (const chunk of chunks) {
    const bytes = new TextEncoder().encode(chunk)
    const out = new Uint8Array(bytes.length)
    text += decoder.decode(out.subarray(0, stripper.strip(bytes, out, 0)), { stream: true })
  }
  return text
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
    // The seed drives the resampling: the same seed repeats an interval, and another moves it.
    const mean = (sortedValues: readonly number[]): number => sortedValues.reduce((sum, value) => sum + value, 0) / sortedValues.length
    const options = { resamples: 200, confidence: 0.9, seed: 8 }
    const other = bootstrapInterval(values, mean, options)
    expect(bootstrapInterval(values, mean, options)).toEqual(other)
    expect(bootstrapInterval(values, mean, { ...options, seed: 9 })).not.toEqual(other)
    expect(other.low).toBeLessThan(other.high)
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

/** A fake `/proc` entry: its `stat` line, `VmRSS` and `VmHWM`, and per-task children. */
interface FakeProcess {
  pid: number
  ppid: number
  comm: string
  rssKiB?: number
  hwmKiB?: number
  ticks: number
  start: number
  session?: number
  children?: number[]
}

async function fakeProc(processes: FakeProcess[], childrenFiles: boolean): Promise<string> {
  const root = await temporary('rust-perf-proc-')
  for (const entry of processes) {
    const dir = join(root, String(entry.pid))
    await mkdir(join(dir, 'task', String(entry.pid)), { recursive: true })
    // Fields 3-22 of proc(5): state, ppid, pgrp, session, ..., utime (14), stime (15), ..., starttime (22).
    const fields = ['S', entry.ppid, 0, entry.session ?? 0, 0, 0, 0, 0, 0, 0, 0, entry.ticks, 1, 0, 0, 20, 0, 1, 0, entry.start]
    await writeFile(join(dir, 'stat'), `${entry.pid} (${entry.comm}) ${fields.join(' ')} 0 0\n`)
    const memory = entry.rssKiB === undefined ? '' : `VmHWM:\t  ${entry.hwmKiB ?? entry.rssKiB} kB\nVmRSS:\t  ${entry.rssKiB} kB\n`
    await writeFile(join(dir, 'status'), `Name:\t${entry.comm}\n${memory}Threads:\t1\n`)
    if (childrenFiles) await writeFile(join(dir, 'task', String(entry.pid), 'children'), (entry.children ?? []).map(child => `${child} `).join(''))
  }
  return root
}

const FAKE_TREE: FakeProcess[] = [
  { pid: 100, ppid: 1, comm: 'bake', rssKiB: 1000, hwmKiB: 4000, ticks: 10, start: 500, session: 100, children: [101, 102] },
  { pid: 101, ppid: 100, comm: 'sh -c (x) y', rssKiB: 200, ticks: 3, start: 510, session: 100, children: [103] },
  { pid: 102, ppid: 100, comm: 'kworker', ticks: 1, start: 520 },
  { pid: 103, ppid: 101, comm: 'sleep', rssKiB: 30, ticks: 0, start: 530, session: 100 },
  // A stale child listing whose pid now belongs to another parent.
  { pid: 104, ppid: 1, comm: 'other', rssKiB: 9999, ticks: 0, start: 540 },
  // A double-forked daemon that kept the session, and an older process in a session of a reused pid.
  { pid: 105, ppid: 1, comm: 'daemon', rssKiB: 10, ticks: 0, start: 550, session: 100 },
  { pid: 106, ppid: 1, comm: 'older', rssKiB: 10, ticks: 0, start: 400, session: 100 },
  { pid: 1, ppid: 0, comm: 'init', rssKiB: 5000, ticks: 0, start: 1, children: [100, 104, 105, 106] },
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
      expect(sample.hwmKiB).toEqual(new Map([['100:500', 4000], ['101:510', 200], ['102:520', 0], ['103:530', 30]]))
      expect(readProcStat(101, proc)).toEqual({ pid: 101, ppid: 100, cpuTicks: 4, startTime: 510, state: 'S', session: 100 })
      expect(processTree(999, proc, childrenFiles)).toEqual([])
      // The session holds the tree's members and the daemon, not a process older than the session leader.
      expect(sessionMembers({ pid: 100, startTime: 500 }, proc).map(member => member.pid).sort()).toEqual([101, 103, 105])
      // A live pid 100 with another start time is a reused pid, whose session is another program's.
      expect(sessionMembers({ pid: 100, startTime: 499 }, proc)).toEqual([])
    }
  })

  it('counts CPU ticks per process identity, a restarted pid in full', () => {
    const stat = (pid: number, cpuTicks: number, startTime = pid): ProcStat => ({ pid, ppid: 1, cpuTicks, startTime, state: 'S', session: 1 })
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
    expect(observed.sampledPeakRssKiB).toBeGreaterThan(0)
    // Each member's high-water mark is at least what any one reading of it showed.
    expect(observed.peakHwmKiB).toBeGreaterThanOrEqual(observed.sampledPeakRssKiB)
    expect(observed.members.map(member => member.pid).sort()).toEqual(members.map(member => member.pid).sort())
    // Killing only the shell leaves its two sleeps alive under the same identity.
    child.kill('SIGKILL')
    await child.exited
    left = survivors(members, child.pid)
    expect(left.map(member => member.pid).sort()).toEqual(members.slice(1).map(member => member.pid).sort())
  })
})

describe('rust-perf terminal observation', () => {
  it('strips control sequences split across chunks', () => {
    expect(strip(new AnsiStripper(), ['a\x1b[3', '8;5;8mb\x1b]0;ti', 'tle\x07c\x1b]8;;x\x1b', '\\d\x1b(Be\r\nf\tg\x1b', 'Mh ✓'])).toBe('abcde\n\nf gh ✓')
  })

  it('tracks DEC private modes across chunks and parameter lists', () => {
    const changes: string[] = []
    const stripper = new AnsiStripper((mode, set) => changes.push(`${mode}${set ? 'h' : 'l'}`))
    strip(stripper, ['\x1b[?10', '00;1006h\x1b[?7l\x1b', '[?2004h\x1b[?25l'])
    expect(changes).toEqual(['1000h', '1006h', '7l', '2004h', '25l'])
    expect(stripper.changedModes()).toEqual(['?7l', '?25l', '?1000h', '?1006h', '?2004h'])
    // Untracked modes, ANSI (non-private) modes, subparameters, and intermediates change nothing.
    strip(stripper, ['\x1b[?1h\x1b[4h\x1b[?1000:1l\x1b[?1006$l'])
    expect(stripper.changedModes()).toEqual(['?7l', '?25l', '?1000h', '?1006h', '?2004h'])
    strip(stripper, ['\x1b[?1006;1000l\x1b[?7;25h\x1b[?2004l'])
    expect(stripper.changedModes()).toEqual([])
    // A full reset returns every mode named so far to its initial state.
    strip(stripper, ['\x1b[?1049h\x1b[?7l', '\x1bc'])
    expect(stripper.changedModes()).toEqual([])
    expect(stripper.modes.get(1049)).toBe(false)
  })

  it('sees bracketed paste, raw mode, split markers once, and a watched echo in a real PTY', async () => {
    const script = 'saved=$(stty -g); stty raw -echo; printf "\\033[?2004h"; printf "H000"; sleep 0.1; printf "01_E"; sleep 0.1; printf "ND H00002_END\\n"; read -r line; printf "got:%s\\033[?2004l" "$line"; stty "$saved"; exit 0'
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
    // The markers still in the search carry are not counted again with later output.
    expect(terminal.markerOccurrences).toBe(2)
    const exit = await terminal.exited
    expect(exit).toMatchObject({ code: 0, signal: null })
    await until(() => terminal.restored)
    expect(terminal.restored).toBe(true)
    // Stamped at the arrival of the output that enabled bracketed paste, after raw mode was set.
    expect(terminal.inputReadyAt).toBe(terminal.bracketedPasteAt)
    expect(terminal.termiosChangedAt).toBeLessThanOrEqual(terminal.bracketedPasteAt!)
  })

  it('reports a DEC private mode the program left changed as not restored', async () => {
    const script = 'printf "\\033[?2004h\\033[?10"; sleep 0.05; printf "00;1006h\\033[?25l"; printf "\\033[?2004l\\033[?25h"; exit 0'
    const terminal = new Terminal(['sh', '-c', script], tmpdir(), { PATH: process.env.PATH ?? '/usr/bin:/bin' })
    cleanups.push(() => terminal.close())
    await terminal.exited
    // The last output may be read after the exit is observed.
    await until(() => terminal.unrestored().length === 2)
    expect(terminal.unrestored()).toEqual(['?1000h left in effect', '?1006h left in effect'])
    expect(terminal.restored).toBe(false)
  })
})

function syntheticRecord(arm: string, workload: WorkloadName, iteration: number, startupMs: number, peakRssMiB: number): SampleRecord {
  return {
    arm, workload, mode: 'cold', iteration, warmup: false, loadAverage1m: 0.1, dimensions: undefined, ok: true,
    metrics: {
      inputReadyMs: 5, startupMs, markersAtFirstInput: 0, replayMs: null, readyMs: startupMs,
      firstDeltaMs: 20, streamInputMs: 2, streamMs: 1000,
      idleCpuMs: 10, idleCpuPercent: 0.2, shutdownMs: 40, peakRssMiB, sampledPeakRssMiB: peakRssMiB,
    },
    exit: { code: 0, signal: null }, terminalRestored: true, leakedProcesses: [], markerOccurrences: 0, outputBytes: 1, modelRequests: 1,
    rssSampling: { ticks: 10, maxGapMs: 10, processes: 1 },
  }
}

function syntheticReport(samples: SampleRecord[]): Report {
  const arms = [...new Set(samples.map(sample => sample.arm))]
  return {
    schema: REPORT_SCHEMA, createdAt: '2026-10-21T00:00:00.000Z', command: [], interrupted: false, host: {}, harness: {},
    method: { samplesPerCell: 20, order: '', viewport: { columns: 120, rows: 40 }, paceMs: 10, idleMs: 5000, rssIntervalMs: 10, timeoutMs: 1, shutdownTimeoutMs: 1, maxLoad: 1, cache: { cold: '', warm: '' }, endpoints: '' },
    arms: arms.map(label => ({ label, kind: 'ts', build: {} })), workloads: ['fresh'], modes: ['cold'], summary: summarize(samples), samples,
  }
}

describe('rust-perf budgets', () => {
  it('proposes 10% over the baseline median and p95 and no leaks or failures', () => {
    const samples = Array.from({ length: 20 }, (_, index) => syntheticRecord('ts:/b', 'fresh', index, 100 + index, 200 + index))
    samples.push({ arm: 'ts:/b', workload: 'fresh', mode: 'cold', iteration: 20, warmup: false, loadAverage1m: 0, dimensions: undefined, leakedProcesses: ['sleep 9'], ok: false, error: 'timed out' })
    samples.push({ ...syntheticRecord('ts:/b', 'fresh', -1, 9999, 9999), warmup: true })
    const report = syntheticReport(samples)
    // A failed sample's leak still counts.
    expect(report.summary[0]).toMatchObject({ completed: 20, failed: 1, leakedProcesses: 1 })
    const proposal = proposeBudgets(report, new TextEncoder().encode('{}'), '/r.json')
    expect(proposal.status).toBe('proposed')
    expect(proposal.approval.approvedBy).toBeNull()
    const cell = proposal.cells[0]!
    expect(cell.metrics.startupMs).toMatchObject({ samples: 20, baselineMedian: 109.5, baselineP95: 118, maxMedian: 120.45, maxP95: 129.8 })
    expect(cell.metrics.peakRssMiB).toMatchObject({ maxMedian: 230.45, maxP95: 239.8 })
    expect(cell.metrics.replayMs).toBeUndefined()
    expect(cell.observed.idleCpuMs?.median).toBe(10)
    expect(cell).toMatchObject({ maxLeakedProcesses: 0, maxFailedSamples: 0 })
    expect(proposal.warnings).toEqual(['fresh/cold: 1 baseline sample(s) failed; investigate before approving', 'fresh/cold: the baseline itself leaked 1 process(es)'])
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

  it('reads a native binary\'s Cargo profile from its path and warns about a non-release baseline', () => {
    expect(cargoProfile('/r/rust/target/release/bake')).toBe('release')
    expect(cargoProfile('/r/rust/target/debug/bake')).toBe('debug')
    expect(cargoProfile('/r/rust/target/x86_64-unknown-linux-gnu/release/bake')).toBe('release')
    expect(cargoProfile('/r/rust/target/debug/deps/bake-0123abcd')).toBe('debug')
    expect(cargoProfile('/usr/local/bin/bake')).toBeNull()
    expect(cargoProfile('/target/a/b/c/bake')).toBeNull()
    const report = syntheticReport([syntheticRecord('native:/n', 'fresh', 0, 1, 1)])
    const native = { ...report, arms: [{ label: 'native:/n', kind: 'native', build: { profile: 'debug' } }] }
    expect(proposeBudgets(native, new Uint8Array(), '/r.json').warnings).toContain('the baseline is a native debug build; D18 budgets compare release builds')
    const release = { ...report, arms: [{ label: 'native:/n', kind: 'native', build: { profile: 'release' } }] }
    expect(proposeBudgets(release, new Uint8Array(), '/r.json').warnings.join('\n')).not.toContain('release builds')
  })
})

/** Live processes whose command line holds `token`, a sleep duration unique to one test. */
function processesWith(token: string): number[] {
  return readdirSync('/proc').filter(entry => /^\d+$/u.test(entry)).flatMap((entry) => {
    try {
      return readFileSync(`/proc/${entry}/cmdline`, 'utf8').split('\0').includes(token) && readProcStat(Number(entry))?.state !== 'Z' ? [Number(entry)] : []
    } catch {
      return []
    }
  })
}

/**
 * A bash program that plays one sample: raw mode and bracketed paste, the
 * echo of each probe, a streamed answer, then `ending`.
 */
function fakeProgram(ending: string, opening = ''): string {
  return [
    'saved=$(stty -g); stty raw -echo', opening,
    'printf "\\033[?2004h"',
    'x=$(head -c 10); printf "%s" "$x"',
    'head -c 1 >/dev/null',
    'printf "PERF_STREAM_START\\n"',
    'y=$(head -c 15); printf "%s" "$y"',
    'printf "\\nPERF_STREAM_DONE\\nSETTLED\\n"',
    ending,
  ].join('\n')
}

/** Ends on SIGTERM as a well-behaved arm does: restores the terminal and exits 143. */
const CLEAN_ENDING = 'sleep 1000 </dev/null >/dev/null 2>&1 & idle=$!\ntrap \'kill -9 $idle; wait $idle 2>/dev/null; printf "\\033[?2004l"; stty "$saved"; exit 143\' TERM\nwait $idle'

function fakeArm(script: string, log?: string): Arm {
  return {
    spec: { label: 'fake', kind: 'native', path: '/bin/bash' },
    check: () => {},
    build: () => ({}),
    prepare: async (_workload, dirs) => {
      await mkdir(dirs.home, { recursive: true })
      return {
        argv: ['bash', '-c', script], cwd: dirs.root, dimensions: { ...history(WORKLOADS.fresh).dimensions, fileBytes: 0 },
        env: { PATH: process.env.PATH ?? '/usr/bin:/bin', BAKE_HOME: dirs.home, ...log === undefined ? {} : { PERF_LOG: log } },
      }
    },
    settled: text => text.includes('SETTLED'),
  }
}

const KEY: SampleKey = { arm: 'fake', workload: 'fresh', mode: 'cold', iteration: 0, warmup: false }

describe('rust-perf samples', () => {
  async function setup(): Promise<{ settings: SampleSettings; warm: WarmState; samples: string }> {
    const dir = await temporary('rust-perf-sample-')
    const samples = join(dir, 'samples')
    await mkdir(samples)
    const warm = { cache: join(dir, 'cache'), home: join(dir, 'warm.home'), template: join(dir, 'warm.template') }
    return { settings: { idleMs: 100, timeoutMs: 10_000, shutdownTimeoutMs: 400, clockTicks: 100, tempRoot: samples }, warm, samples }
  }
  const model = { baseUrl: 'http://127.0.0.1:9/v1', requests: 0 }
  /** Kill what a failed assertion would otherwise leave behind. */
  function reap(...tokens: string[]): void {
    cleanups.push(async () => {
      for (const token of tokens) for (const pid of processesWith(token)) process.kill(pid, 'SIGKILL')
    })
  }

  it('measures a well-behaved program and removes its temporary root', async () => {
    const { settings, warm, samples } = await setup()
    const record = await runSample(fakeArm(fakeProgram(CLEAN_ENDING)), KEY, model, warm, settings)
    if (!record.ok) throw new Error(record.error)
    expect(record.exit).toEqual({ code: 143, signal: null })
    expect(record.leakedProcesses).toEqual([])
    for (const value of Object.values(record.metrics)) if (value !== null) expect(value).toBeGreaterThanOrEqual(0)
    expect(record.metrics.peakRssMiB).toBeGreaterThan(0)
    expect(await readdir(samples)).toEqual([])
  }, 20_000)

  it('fails a program that ignores SIGTERM at the shutdown deadline and kills its tree', async () => {
    const { settings, warm, samples } = await setup()
    reap('1001.5')
    const started = performance.now()
    const record = await runSample(fakeArm(fakeProgram('trap "" TERM\nnohup sleep 1001.5 </dev/null >/dev/null 2>&1 &\nwait')), KEY, model, warm, settings)
    expect(record.ok).toBe(false)
    if (record.ok) return
    expect(record.error).toContain('did not exit within 400 ms of SIGTERM')
    expect(performance.now() - started).toBeLessThan(8000)
    // The program was killed first, so its child outlived it: reported, then killed.
    expect(record.leakedProcesses).toEqual(['sleep 1001.5'])
    expect(processesWith('1001.5')).toEqual([])
    expect(await readdir(samples)).toEqual([])
  }, 20_000)

  it('fails a program that exits on its own before SIGTERM', async () => {
    const { settings, warm, samples } = await setup()
    const record = await runSample(fakeArm(fakeProgram('sleep 0.02; printf "\\033[?2004l"; stty "$saved"; exit 0')), KEY, model, warm, { ...settings, idleMs: 1500 })
    expect(record.ok).toBe(false)
    if (record.ok) return
    expect(record.error).toContain('exited before SIGTERM')
    expect(await readdir(samples)).toEqual([])
  }, 20_000)

  it('reports and kills every process left alive after the program exits, wherever it went', async () => {
    const { settings, warm, samples } = await setup()
    reap('1002.5', '1003.5', '1004.5')
    // A child, a double-forked one that leaves the tree at once, and one started during shutdown;
    // each ignores the SIGHUP that ends the terminal's foreground jobs when the program exits.
    const opening = 'nohup sleep 1002.5 </dev/null >/dev/null 2>&1 &\n(nohup sleep 1003.5 </dev/null >/dev/null 2>&1 &)'
    const ending = CLEAN_ENDING.replace('kill -9 $idle;', 'kill -9 $idle; nohup sleep 1004.5 </dev/null >/dev/null 2>&1 &')
    const record = await runSample(fakeArm(fakeProgram(ending, opening)), KEY, model, warm, settings)
    if (!record.ok) throw new Error(record.error)
    expect([...record.leakedProcesses].sort()).toEqual(['sleep 1002.5', 'sleep 1003.5', 'sleep 1004.5'])
    for (const token of ['1002.5', '1003.5', '1004.5']) expect(processesWith(token)).toEqual([])
    expect(await readdir(samples)).toEqual([])
  }, 20_000)

  it('kills a failed sample\'s orphans even when the program is already gone', async () => {
    const { settings, warm } = await setup()
    reap('1005.5')
    const record = await runSample(fakeArm(fakeProgram('', 'nohup sleep 1005.5 </dev/null >/dev/null 2>&1 &\nsleep 0.2; exit 3')), KEY, model, warm, settings)
    expect(record.ok).toBe(false)
    expect(record.leakedProcesses).toEqual(['sleep 1005.5'])
    expect(processesWith('1005.5')).toEqual([])
  }, 20_000)

  it('fails a program that leaves a DEC private mode set', async () => {
    const { settings, warm } = await setup()
    const record = await runSample(fakeArm(fakeProgram(CLEAN_ENDING, 'printf "\\033[?1000;1006h"')), KEY, model, warm, settings)
    expect(record.ok).toBe(false)
    if (record.ok) return
    expect(record.error).toBe('the terminal was not restored at exit: ?1000h left in effect, ?1006h left in effect')
  }, 20_000)

  it('stops at once when interrupted, after killing the program and removing its root', async () => {
    const { settings, warm, samples } = await setup()
    reap('1006.5')
    const aborter = new AbortController()
    const pending = runSample(fakeArm(fakeProgram('trap "" TERM\nnohup sleep 1006.5 </dev/null >/dev/null 2>&1 &\nwait')), KEY, model, warm, { ...settings, shutdownTimeoutMs: 60_000, signal: aborter.signal })
    await until(() => processesWith('1006.5').length > 0)
    await Bun.sleep(300)
    const abortedAt = performance.now()
    aborter.abort(new Error('interrupted'))
    expect(pending).rejects.toThrow('interrupted')
    await pending.catch(() => undefined)
    expect(performance.now() - abortedAt).toBeLessThan(5000)
    expect(processesWith('1006.5')).toEqual([])
    expect(await readdir(samples)).toEqual([])
  }, 20_000)

  it('starts warm samples from a prepared home without its sessions, at one path', async () => {
    const { settings, warm, samples } = await setup()
    const log = join(await temporary('rust-perf-log-'), 'log')
    // Report what the home held at start, then leave a preparation and a session behind.
    const opening = 'printf "%s|%s\\n" "$(ls -A "$BAKE_HOME" | tr "\\n" " ")" "$BAKE_HOME" >> "$PERF_LOG"; touch "$BAKE_HOME/prepared"; mkdir -p "$BAKE_HOME/sessions"; touch "$BAKE_HOME/sessions/s"'
    const arm = fakeArm(fakeProgram(CLEAN_ENDING, opening), log)
    const warmKey = { ...KEY, mode: 'warm' as const }
    const prepared = await runSample(arm, { ...warmKey, iteration: PREPARE_ITERATION, warmup: true }, model, warm, settings, true)
    if (!prepared.ok) throw new Error(prepared.error)
    expect(readdirSync(warm.template).sort()).toEqual(['prepared'])
    expect(existsSync(warm.home)).toBe(false)
    for (let index = 0; index < 2; index++) {
      const record = await runSample(arm, warmKey, model, warm, settings)
      if (!record.ok) throw new Error(record.error)
    }
    const cold = await runSample(arm, KEY, model, warm, settings)
    if (!cold.ok) throw new Error(cold.error)
    const lines = readFileSync(log, 'utf8').trimEnd().split('\n')
    expect(lines.slice(0, 3)).toEqual([`|${warm.home}`, `prepared |${warm.home}`, `prepared |${warm.home}`])
    // A cold sample starts from an empty home of its own.
    expect(lines[3]).toStartWith(`|${realpathSync(samples)}/`)
    expect(readdirSync(warm.template)).toEqual(['prepared'])
    expect(await readdir(samples)).toEqual([])
  }, 30_000)
})

/**
 * The native smoke measures `BAKE_PERF_NATIVE`, a `bake` binary the caller
 * names, because the scripts-unit gate runs before any build and a binary
 * already under rust/target may be older than the sources. Unset, it is
 * skipped and says so.
 */
const nativeBinary = process.env.BAKE_PERF_NATIVE === undefined || process.env.BAKE_PERF_NATIVE === '' ? undefined : resolve(process.env.BAKE_PERF_NATIVE)
if (nativeBinary === undefined) console.log('rust-perf native smoke skipped: set BAKE_PERF_NATIVE=<bake binary> to measure one, such as rust/target/release/bake after cargo build --locked --release -p bake-cli')

describe('rust-perf native smoke', () => {
  it.skipIf(nativeBinary === undefined)('measures fresh and Pi-seeded samples of BAKE_PERF_NATIVE cold and warm, and proposes budgets from them', async () => {
    if (nativeBinary === undefined || !statSync(nativeBinary).isFile()) throw new Error(`BAKE_PERF_NATIVE names no file: ${nativeBinary}`)
    const dir = await temporary('rust-perf-smoke-')
    const out = join(dir, 'report.json')
    const run = Bun.spawn(['bun', join(repository, 'scripts/rust-perf.ts'), '--arm', `native:${nativeBinary}`, '--workload', 'fresh', '--workload', 'small', '--samples', '1', '--mode', 'both',
      '--idle-ms', '200', '--timeout-ms', '20000', '--max-load', '100000', '--allow-non-release', '--out', out], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    const [code, stdout, stderr] = await Promise.all([run.exited, new Response(run.stdout).text(), new Response(run.stderr).text()])
    expect({ code, stderr }).toEqual({ code: 0, stderr: '' })
    // One preparation, two warm-ups, and four measured samples.
    expect(stdout).toContain('7 sample(s), 0 failed')
    const report = JSON.parse(readFileSync(out, 'utf8')) as Report
    expect(report.schema).toBe(REPORT_SCHEMA)
    expect(report.interrupted).toBe(false)
    const build = { sha256: sha256(readFileSync(nativeBinary)), profile: cargoProfile(realpathSync(nativeBinary)) }
    expect(report.arms[0]!.build).toMatchObject(build)
    const measured = report.samples.filter(sample => !sample.warmup)
    expect(measured.map(sample => `${sample.workload}/${sample.mode}`).sort()).toEqual(['fresh/cold', 'fresh/warm', 'small/cold', 'small/warm'])
    for (const sample of measured) {
      if (!sample.ok) throw new Error(`the smoke sample failed: ${JSON.stringify(sample)}`)
      expect(sample.terminalRestored).toBe(true)
      expect(sample.leakedProcesses).toEqual([])
      expect(sample.modelRequests).toBe(1)
      for (const metric of ['inputReadyMs', 'startupMs', 'readyMs', 'firstDeltaMs', 'streamInputMs', 'streamMs', 'shutdownMs', 'peakRssMiB', 'sampledPeakRssMiB'] as const) expect(sample.metrics[metric]).toBeGreaterThan(0)
      expect(sample.metrics.peakRssMiB).toBeGreaterThanOrEqual(sample.metrics.sampledPeakRssMiB)
      if (sample.workload === 'fresh') {
        expect(sample.metrics.replayMs).toBeNull()
      } else {
        // The Pi seed's 50 answers were each shown once.
        expect(sample.markerOccurrences).toBe(50)
        expect(sample.metrics.replayMs).toBeGreaterThan(0)
      }
    }
    expect(report.summary.find(row => row.workload === 'small' && row.mode === 'cold')!.metrics.startupMs.samples).toBe(1)
    const proposalPath = join(dir, 'budgets.json')
    const budgets = Bun.spawnSync(['bun', join(repository, 'scripts/rust-perf.ts'), 'budgets', '--baseline', out, '--out', proposalPath], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    expect(budgets.exitCode).toBe(0)
    const proposal = JSON.parse(await readFile(proposalPath, 'utf8')) as BudgetProposal
    const fresh = measured.find(sample => sample.workload === 'fresh' && sample.mode === 'cold')
    if (fresh === undefined || !fresh.ok) throw new Error('no fresh cold sample')
    expect(proposal.cells.find(cell => cell.workload === 'fresh' && cell.mode === 'cold')!.metrics.startupMs!.maxMedian).toBeGreaterThan(fresh.metrics.startupMs)
    const refused = Bun.spawnSync(['bun', join(repository, 'scripts/rust-perf.ts'), 'budgets', '--baseline', out, '--out', join(repository, 'docs/rust-perf-budgets.json')], { cwd: repository, stdout: 'pipe', stderr: 'pipe' })
    expect(refused.exitCode).toBe(2)
    expect(existsSync(join(repository, 'docs/rust-perf-budgets.json'))).toBe(false)
  }, 120_000)
})
