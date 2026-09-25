/** Main-process diagnostics; samples are explicitly requested outside timed input intervals. */
import { existsSync, readFileSync, writeFileSync, renameSync } from 'node:fs'
import { isMainThread } from 'node:worker_threads'
const path = process.env.DSH_TUI_PERF_METRICS
if (isMainThread && path !== undefined) {
  // The driver requests a sample by writing its number to a file rather than
  // sending a signal: a child of Bun on macOS can inherit a mask that blocks
  // SIGUSR2, so a signal may never reach a handler here.
  const request = path + '.request'
  let sequence = 0
  setInterval(() => {
    if (!existsSync(request)) return
    const wanted = Number(readFileSync(request, 'utf8'))
    if (!(wanted > sequence)) return
    if (globalThis.gc === undefined) throw new Error('performance measurement requires --expose-gc')
    const beforeGc = process.memoryUsage()
    globalThis.gc()
    sequence = wanted
    writeFileSync(path + '.tmp', JSON.stringify({ sequence, beforeGc, afterGc: process.memoryUsage(),
      resources: process.resourceUsage(), cpu: process.cpuUsage() }))
    renameSync(path + '.tmp', path)
  }, 20).unref()
}
