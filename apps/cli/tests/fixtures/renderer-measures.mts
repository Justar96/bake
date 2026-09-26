/**
 * Render Ink frames in a fresh process and report how many user-timing
 * measures Node retained. `select` chooses the build the way the CLI does
 * before anything loads React; `inherit` leaves the caller's NODE_ENV.
 */
import { PassThrough, Writable } from 'node:stream'
import { selectRendererBuild } from '../../src/bin.ts'

if (process.argv[2] === 'select') selectRendererBuild()
const React = (await import('react')).default
const { Box, Text, render } = await import('ink')
const output = new Writable({ write(_chunk, _encoding, done) { done() } })
Object.assign(output, { isTTY: true, columns: 80, rows: 24 })
const input = new PassThrough()
Object.assign(input, { isTTY: true, setRawMode: () => input, ref() {}, unref() {} })
const view = (frame: number) => React.createElement(Box, null, React.createElement(Text, null, `frame ${frame}`))
const ui = render(view(0), { stdout: output as never, stdin: input as never, patchConsole: false, exitOnCtrlC: false, maxFps: 1e6 })
for (let frame = 1; frame <= 200; frame++) {
  ui.rerender(view(frame))
  await ui.waitUntilRenderFlush()
}
ui.unmount()
input.destroy()
process.stdout.write(`${JSON.stringify({ measures: performance.getEntriesByType('measure').length, nodeEnv: process.env.NODE_ENV })}\n`)
