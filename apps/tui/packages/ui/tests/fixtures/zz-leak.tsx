import { PassThrough, Writable } from 'node:stream'
import React from 'react'
import { render } from 'ink'
import { App } from '/home/jt-pengu/bake/apps/tui/packages/ui/src/app.tsx'
import { dictionaries } from '/home/jt-pengu/bake/apps/tui/packages/ui/src/copy.ts'
import { appendTranscript, emptyTranscript } from '/home/jt-pengu/bake/apps/tui/packages/ui/src/transcript.ts'
const mode = process.argv[2] ?? 'idle'
let bytes = 0
const out = new Writable({ write(c, _e, d) { bytes += c.length; d() } })
Object.assign(out, { isTTY: true, columns: 120, rows: 40 })
const input = new PassThrough(); Object.assign(input, { isTTY: true, setRawMode: () => input, ref() {}, unref() {} })
let t = 0; const ticks = new Set<() => void>()
const clock = { now: () => t, every: (_ms: number, tick: () => void) => { ticks.add(tick); return () => ticks.delete(tick) } }
let committed = emptyTranscript
const live = mode === 'tool' ? [{ kind: 'tool-call', callId: 'c', tool: 'bash', input: 'sleep 1000' }] : []
const props = (): any => ({ files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
  completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
  committed, live, pending: [], status: 'running', stopping: false, command: undefined, notice: undefined, interaction: undefined,
  todos: undefined, model: 'm/m', cwd: '/w', sessionId: 's', copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
  clock, onSubmit() {}, onCancel() {}, onInterrupt() {}, onAnswer() {} })
const ui = render(<App {...props()} />, { stdout: out as any, stderr: out as any, stdin: input as any, patchConsole: false, exitOnCtrlC: false, maxFps: 1e6 })
const heap = () => { (globalThis as any).gc(); return Math.round(process.memoryUsage().heapUsed / 1e6) }
await ui.waitUntilRenderFlush()
const N = Number(process.argv[3] ?? 20000)
const report: string[] = [`0:${heap()}MB`]
for (let i = 1; i <= N; i++) {
  t += 150; for (const tick of ticks) tick()
  if (mode === 'grow' && i % 20 === 0) { committed = appendTranscript(committed, [{ kind: 'assistant', text: `answer ${i} ` + 'word '.repeat(40) }]); ui.rerender(<App {...props()} />) }
  await new Promise(r => setImmediate(r))
  if (i % (N / 5) === 0) report.push(`${i}:${heap()}MB`)
}
console.log(mode, report.join(' '), 'out', Math.round(bytes / 1e6) + 'MB')
ui.unmount(); process.exit(0)
