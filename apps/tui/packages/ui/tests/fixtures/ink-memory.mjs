/** Isolated production renderer workload; terminal output is counted, never retained. */
import assert from 'node:assert/strict'
import { PassThrough, Writable } from 'node:stream'
import React from 'react'
import { Box, Text, render } from 'ink'

assert.equal(typeof globalThis.gc, 'function', 'Run with node --expose-gc')
let bytes = 0
let tail = ''
const output = new Writable({
  write(chunk, _encoding, done) {
    bytes += chunk.length
    tail = (tail + chunk.toString()).slice(-1024)
    done()
  },
})
Object.assign(output, { isTTY: true, columns: 100, rows: 24 })
const input = new PassThrough()
Object.assign(input, { isTTY: true, setRawMode: () => input, ref() {}, unref() {} })
const view = text => React.createElement(Box, { width: 100 },
  React.createElement(Text, { wrap: 'truncate-end' }, text))
const ui = render(view('warmup'), {
  stdout: output, stderr: output, stdin: input,
  patchConsole: false, exitOnCtrlC: false, interactive: true, maxFps: 1_000_000,
})
const samples = []
const sample = frames => {
  globalThis.gc()
  samples.push({ frames, heapUsed: process.memoryUsage().heapUsed })
}
const start = performance.now()
try {
  await ui.waitUntilRenderFlush()
  sample(0)
  // A long tool headline is truncated on screen but measured from its complete
  // source. Distinct values must not remain alive in process-global caches.
  for (let frame = 1; frame <= 3000; frame++) {
    ui.rerender(view(`frame-${frame}:` + 'synthetic output '.repeat(1024)))
    await ui.waitUntilRenderFlush()
    if (frame % 1000 === 0) sample(frame)
  }
  assert.ok(tail.includes('frame-3000:'), 'the last update must reach the terminal')
  // A previously evicted value must still render correctly.
  ui.rerender(view('frame-1:' + 'synthetic output '.repeat(1024)))
  await ui.waitUntilRenderFlush()
  assert.ok(tail.includes('frame-1:'), 'revisiting text must reach the terminal')
} finally {
  ui.cleanup()
  await ui.waitUntilExit()
  input.destroy()
  output.destroy()
}
sample(3001)
process.stdout.write(JSON.stringify({ samples, bytes, elapsedMs: performance.now() - start }) + '\n')
