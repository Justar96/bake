/** Main-process diagnostics; samples are explicitly requested outside timed input intervals. */
import { writeFileSync, renameSync } from 'node:fs'
import { isMainThread } from 'node:worker_threads'
const path = process.env.DSH_TUI_PERF_METRICS
if (isMainThread && path !== undefined) {
  let sequence = 0
  process.on('SIGUSR2', () => {
    if (globalThis.gc === undefined) throw new Error('performance measurement requires --expose-gc')
    const beforeGc = process.memoryUsage()
    globalThis.gc()
    writeFileSync(path + '.tmp', JSON.stringify({ sequence: ++sequence, beforeGc, afterGc: process.memoryUsage(),
      resources: process.resourceUsage(), cpu: process.cpuUsage() }))
    renameSync(path + '.tmp', path)
  })
}
