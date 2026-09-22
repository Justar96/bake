/** Synthetic workloads retain complete replayable histories with reproducible input. */
import { expect, it } from 'vitest'
import { parseSessionLog } from '@deepseek-ai/dsh-llm-replay'
import { Session } from '@deepseek-ai/dsh-session'
import { history, reply } from '../performance/history.ts'
import { summarize, type Sample } from '../performance/report.ts'

it('generates identical complete mixed histories and preserves all markers through restore', () => {
  const first = history(12, '/synthetic-workspace')
  expect(history(12, '/synthetic-workspace')).toEqual(first)
  expect(first.dimensions.toolCount).toBe(3)
  const text = [JSON.stringify({ type: 'session', ...first.header }), ...first.events.map(event => JSON.stringify(event)), ''].join('\n')
  const events = parseSessionLog(text)
  const restored = Session.create(first.header.id, events, first.header)
  const contents = JSON.stringify(restored.deriveMessages())
  for (let turn = 1; turn <= 12; turn++) expect(contents).toContain(`H${String(turn).padStart(5, '0')}_END`)
  expect(events.filter(event => event.type === 'tool/result')).toHaveLength(3)
  expect(events.filter(event => event.type === 'turn/end')).toHaveLength(12)
  expect(first.dimensions.deltaCount).toBeGreaterThan(400)
})

it('includes the last visible delta before the terminal finish', () => {
  const chunks = reply()
  expect(chunks.at(-1)).toEqual({ type: 'finish', reason: { kind: 'stop' } })
  expect(chunks.filter(chunk => chunk.type === 'text-delta')).toHaveLength(100)
  expect(chunks.at(-3)).toEqual({ type: 'text-delta', index: 0, text: '\nPERF_STREAM_DONE' })
})

it('keeps failed samples out of medians and distinguishes all-failed workloads', () => {
  const memory = { beforeGc: process.memoryUsage(), afterGc: process.memoryUsage(), resources: process.resourceUsage(), cpu: process.cpuUsage(), sequence: 1 }
  const sample: Sample = { workload: 'small', iteration: 0, dimensions: { turns: 50, events: 326, deltaCount: 2050, toolCount: 12, fileBytes: 0, sessionId: 'synthetic' },
    readyMs: 100, idleInputMs: [1, 7, 2], idleBytes: 0, initialBytes: 0, firstDeltaMs: 10, liveInputMs: 3, streamMs: 1500, streamBytes: 0,
    historyMarkerOccurrences: 50, readyMemory: memory, settledMemory: memory }
  const results = summarize([sample, { ...sample, iteration: 1, readyMs: 300 },
    { workload: 'small', iteration: 2, error: 'failed' }, { workload: 'tail', iteration: 0, error: 'failed' }])
  expect(results[0]).toMatchObject({ workload: 'small', completed: 2, failed: 1, readyMs: 200, maxIdleInputMs: 7 })
  expect(results[1]).toMatchObject({ workload: 'tail', completed: 0, failed: 1, readyMs: null, retainedHeapMiB: null })
})
