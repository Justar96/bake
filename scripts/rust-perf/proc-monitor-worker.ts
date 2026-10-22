/** The worker behind `TreeMonitor`: reads a process tree's memory on a fixed period until told to stop. */
import { hasChildrenFiles, identity, sampleTree, type MonitorRequest, type MonitorResult, type ProcStat } from './proc-tree.ts'

declare const self: Worker

let timer: ReturnType<typeof setInterval> | undefined
let sampledPeakRssKiB = 0
let ticks = 0
let maxGapMs = 0
let last: number | undefined
/** Every process seen, by identity, with its largest `VmHWM` read. */
const seen = new Map<string, { member: ProcStat; hwmKiB: number }>()

self.addEventListener('message', (event: MessageEvent<MonitorRequest>) => {
  const request = event.data
  try {
    if (request.kind === 'start') {
      const childrenFiles = hasChildrenFiles(request.root, request.procRoot)
      const read = (): void => {
        const now = performance.now()
        if (last !== undefined) maxGapMs = Math.max(maxGapMs, now - last)
        last = now
        const sample = sampleTree(request.root, request.procRoot, childrenFiles)
        ticks++
        sampledPeakRssKiB = Math.max(sampledPeakRssKiB, sample.rssKiB)
        for (const member of sample.members) {
          const key = identity(member)
          const hwmKiB = sample.hwmKiB.get(key) ?? 0
          const entry = seen.get(key)
          if (entry === undefined) seen.set(key, { member, hwmKiB })
          else entry.hwmKiB = Math.max(entry.hwmKiB, hwmKiB)
        }
      }
      read()
      timer = setInterval(read, request.intervalMs)
      self.postMessage({ kind: 'started' })
    } else {
      if (timer !== undefined) clearInterval(timer)
      let peakHwmKiB = 0
      for (const entry of seen.values()) peakHwmKiB += entry.hwmKiB
      const members = [...seen.values()].map(entry => entry.member)
      const result: MonitorResult = { sampledPeakRssKiB, peakHwmKiB, ticks, maxGapMs, members }
      self.postMessage({ kind: 'result', result })
    }
  } catch (error) {
    if (timer !== undefined) clearInterval(timer)
    self.postMessage({ kind: 'error', message: error instanceof Error ? error.message : String(error) })
  }
})
