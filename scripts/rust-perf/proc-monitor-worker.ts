/** The worker behind `TreeMonitor`: reads a process tree's RSS on a fixed period until told to stop. */
import { hasChildrenFiles, sampleTree, type MonitorRequest, type MonitorResult } from './proc-tree.ts'

declare const self: Worker

let timer: ReturnType<typeof setInterval> | undefined
let peakRssKiB = 0
let ticks = 0
let maxGapMs = 0
let last: number | undefined
const pids = new Set<number>()

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
        peakRssKiB = Math.max(peakRssKiB, sample.rssKiB)
        for (const member of sample.members) pids.add(member.pid)
      }
      read()
      timer = setInterval(read, request.intervalMs)
      self.postMessage({ kind: 'started' })
    } else {
      if (timer !== undefined) clearInterval(timer)
      const result: MonitorResult = { peakRssKiB, ticks, maxGapMs, pids: [...pids] }
      self.postMessage({ kind: 'result', result })
    }
  } catch (error) {
    if (timer !== undefined) clearInterval(timer)
    self.postMessage({ kind: 'error', message: error instanceof Error ? error.message : String(error) })
  }
})
