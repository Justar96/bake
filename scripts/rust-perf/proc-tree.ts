/**
 * Linux `/proc` readings of a measured process and every descendant: resident
 * memory (`VmRSS`), CPU time (`utime` + `stime`), and identities that survive
 * pid reuse (`starttime`).
 *
 * Descendants are found through each task's `children` file when the kernel
 * provides it (`CONFIG_PROC_CHILDREN`), and otherwise by a parent scan of
 * every process. A process that daemonizes away from the tree is not counted.
 * The process-tree RSS is the sum of members' `VmRSS`, so pages that members
 * share are counted once per member.
 */
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

/** One process's identity and counters. */
export interface ProcStat {
  readonly pid: number
  readonly ppid: number
  /** User plus system CPU time, in clock ticks. */
  readonly cpuTicks: number
  /** Start time after boot, in clock ticks; with the pid, a stable identity. */
  readonly startTime: number
}

/** One reading of a tree. */
export interface TreeSample {
  readonly members: readonly ProcStat[]
  /** Sum of members' resident set sizes, in KiB. */
  readonly rssKiB: number
}

function readText(path: string): string | undefined {
  try {
    return readFileSync(path, 'utf8')
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code
    // The process exited between listing and reading, or hides its counters.
    if (code === 'ENOENT' || code === 'ESRCH' || code === 'EACCES' || code === 'EPERM') return undefined
    throw error
  }
}

/**
 * Parse `/proc/<pid>/stat`, whose command name may hold spaces and parentheses.
 * @returns undefined once the process is gone or the line is malformed.
 */
export function readProcStat(pid: number, procRoot = '/proc'): ProcStat | undefined {
  const text = readText(join(procRoot, String(pid), 'stat'))
  if (text === undefined) return undefined
  const close = text.lastIndexOf(')')
  if (close < 0) return undefined
  // Fields after the command, numbered from 3 (`state`) as proc(5) does.
  const fields = text.slice(close + 2).trim().split(/\s+/u)
  const field = (n: number): number => Number(fields[n - 3])
  const stat = { pid, ppid: field(4), cpuTicks: field(14) + field(15), startTime: field(22) }
  return Number.isSafeInteger(stat.ppid) && Number.isSafeInteger(stat.cpuTicks) && Number.isSafeInteger(stat.startTime) ? stat : undefined
}

/** `VmRSS` of one process in KiB: 0 for a kernel thread or a process already gone. */
export function readVmRssKiB(pid: number, procRoot = '/proc'): number {
  const text = readText(join(procRoot, String(pid), 'status'))
  const match = text === undefined ? null : /^VmRSS:\s+(\d+)\s+kB$/mu.exec(text)
  return match === null ? 0 : Number(match[1])
}

function childrenFromTasks(pid: number, procRoot: string): number[] | undefined {
  let tasks: string[]
  try {
    tasks = readdirSync(join(procRoot, String(pid), 'task'))
  } catch {
    return undefined
  }
  const children: number[] = []
  for (const task of tasks) {
    const text = readText(join(procRoot, String(pid), 'task', task, 'children'))
    if (text === undefined) continue
    for (const word of text.split(/\s+/u)) if (word !== '') children.push(Number(word))
  }
  return children
}

/** Whether the kernel lists children per task, judged by the main thread of `pid`. */
export function hasChildrenFiles(pid: number, procRoot = '/proc'): boolean {
  return existsSync(join(procRoot, String(pid), 'task', String(pid), 'children'))
}

/**
 * The root and every live descendant.
 * @param root - the measured process.
 * @param procRoot - the proc file system, replaceable in tests.
 * @param childrenFiles - whether to walk `task/<tid>/children` rather than scan every parent.
 * @returns members in breadth-first order; empty once the root is gone.
 */
export function processTree(root: number, procRoot = '/proc', childrenFiles = hasChildrenFiles(root, procRoot)): ProcStat[] {
  const rootStat = readProcStat(root, procRoot)
  if (rootStat === undefined) return []
  let childrenOf: (pid: number) => number[]
  if (childrenFiles) {
    childrenOf = pid => childrenFromTasks(pid, procRoot) ?? []
  } else {
    const byParent = new Map<number, number[]>()
    const pids = readdirSync(procRoot).filter(entry => /^\d+$/u.test(entry)).map(Number).sort((a, b) => a - b)
    for (const pid of pids) {
      const stat = readProcStat(pid, procRoot)
      if (stat === undefined) continue
      byParent.set(stat.ppid, [...byParent.get(stat.ppid) ?? [], stat.pid])
    }
    childrenOf = pid => byParent.get(pid) ?? []
  }
  const members: ProcStat[] = [rootStat]
  const seen = new Set([root])
  for (let index = 0; index < members.length; index++) {
    const parent = members[index]
    if (parent === undefined) break
    for (const child of childrenOf(parent.pid)) {
      if (seen.has(child)) continue
      seen.add(child)
      const stat = readProcStat(child, procRoot)
      // Only a process whose parent is still this member belongs to the tree.
      if (stat !== undefined && stat.ppid === parent.pid) members.push(stat)
    }
  }
  return members
}

/** Read the tree's members and the sum of their resident sizes. */
export function sampleTree(root: number, procRoot = '/proc', childrenFiles = hasChildrenFiles(root, procRoot)): TreeSample {
  const members = processTree(root, procRoot, childrenFiles)
  let rssKiB = 0
  for (const member of members) rssKiB += readVmRssKiB(member.pid, procRoot)
  return { members, rssKiB }
}

/**
 * CPU ticks the tree spent between two readings: each member's growth, and
 * the whole count of a member that started in between.
 */
export function cpuTicksBetween(before: readonly ProcStat[], after: readonly ProcStat[]): number {
  const earlier = new Map(before.map(stat => [`${stat.pid}:${stat.startTime}`, stat.cpuTicks]))
  let ticks = 0
  for (const stat of after) ticks += stat.cpuTicks - (earlier.get(`${stat.pid}:${stat.startTime}`) ?? 0)
  return ticks
}

/** Members of `members` still alive under the same identity, excluding `except`. */
export function survivors(members: readonly ProcStat[], except: number, procRoot = '/proc'): ProcStat[] {
  return members.filter(member => member.pid !== except && readProcStat(member.pid, procRoot)?.startTime === member.startTime)
}

/** The kernel's `USER_HZ`, the unit of `/proc` CPU times. */
export function clockTicksPerSecond(): number {
  const result = Bun.spawnSync(['getconf', 'CLK_TCK'], { stdout: 'pipe', stderr: 'ignore' })
  const value = Number(result.stdout.toString().trim())
  if (!result.success || !Number.isSafeInteger(value) || value <= 0) throw new Error('getconf CLK_TCK did not report the clock tick rate')
  return value
}

async function withDeadline<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => { reject(new Error(message)) }, timeoutMs)
    })
    return await Promise.race([promise, deadline])
  } finally {
    clearTimeout(timer)
  }
}

/** What a monitor observed between its start and stop. */
export interface MonitorResult {
  /** The largest tree RSS read, in KiB. */
  readonly peakRssKiB: number
  /** Readings taken. */
  readonly ticks: number
  /** The longest interval between two readings, in milliseconds. */
  readonly maxGapMs: number
  /** Every pid seen in the tree. */
  readonly pids: readonly number[]
}

/** Messages a monitor worker accepts. */
export type MonitorRequest = { readonly kind: 'start'; readonly root: number; readonly intervalMs: number; readonly procRoot: string } | { readonly kind: 'stop' }

/** A running tree-RSS monitor in its own worker thread, so harness work never delays a reading. */
export class TreeMonitor {
  private readonly worker: Worker
  private readonly result: Promise<MonitorResult>
  private readonly started: Promise<void>

  /**
   * Start reading the tree of `root` every `intervalMs`.
   * @param root - the measured process.
   * @param intervalMs - the reading period.
   * @param procRoot - the proc file system, replaceable in tests.
   */
  constructor(root: number, intervalMs = 10, procRoot = '/proc') {
    this.worker = new Worker(new URL('./proc-monitor-worker.ts', import.meta.url).href)
    let ready: () => void = () => {}
    this.started = new Promise<void>((resolve) => { ready = resolve })
    this.result = new Promise<MonitorResult>((resolve, reject) => {
      this.worker.addEventListener('message', (event: MessageEvent<{ kind: 'started' } | { kind: 'result'; result: MonitorResult } | { kind: 'error'; message: string }>) => {
        if (event.data.kind === 'started') ready()
        else if (event.data.kind === 'result') resolve(event.data.result)
        else reject(new Error(`tree monitor failed: ${event.data.message}`))
      })
      this.worker.addEventListener('error', (event) => { reject(new Error(`tree monitor failed: ${event.message}`)) })
    })
    void this.result.catch(() => {})
    const start: MonitorRequest = { kind: 'start', root, intervalMs, procRoot }
    this.worker.postMessage(start)
  }

  /** Settles once the worker has taken its first reading; rejects after `timeoutMs`. */
  ready(timeoutMs = 10_000): Promise<void> {
    return withDeadline(Promise.race([this.started, this.result.then(() => {})]), timeoutMs, 'the tree monitor did not start')
  }

  /** Stop reading and return what was observed; the worker then exits. */
  async stop(timeoutMs = 10_000): Promise<MonitorResult> {
    const stop: MonitorRequest = { kind: 'stop' }
    this.worker.postMessage(stop)
    try {
      return await withDeadline(this.result, timeoutMs, 'the tree monitor did not report')
    } finally {
      this.worker.terminate()
    }
  }
}
