/** Synthetic workloads retain complete replayable histories with reproducible input. */
import { expect, it } from 'vitest'
import { parseSessionLog } from '@deepseek-ai/dsh-llm-replay'
import { Session } from '@deepseek-ai/dsh-session'
import { history, reply } from '../performance/history.ts'
import { summarize, WORKLOADS, type Sample } from '../performance/report.ts'

function restore(fixture: ReturnType<typeof history>): Session {
  const text = [JSON.stringify({ type: 'session', ...fixture.header }), ...fixture.events.map(event => JSON.stringify(event)), ''].join('\n')
  return Session.create(fixture.header.id, parseSessionLog(text), fixture.header)
}

it('generates identical complete mixed histories and preserves all markers through restore', () => {
  const first = history(12, '/synthetic-workspace')
  expect(history(12, '/synthetic-workspace')).toEqual(first)
  expect(first.dimensions.toolCount).toBe(3)
  const restored = restore(first)
  const contents = JSON.stringify(restored.deriveMessages())
  for (let turn = 1; turn <= 12; turn++) expect(contents).toContain(`H${String(turn).padStart(5, '0')}_END`)
  expect(first.events.filter(event => event.type === 'tool/result')).toHaveLength(3)
  expect(first.events.filter(event => event.type === 'turn/end')).toHaveLength(12)
  expect(first.dimensions.deltaCount).toBeGreaterThan(400)
  expect(first.dimensions).toMatchObject({ assistantMarkerCount: 12, historyMarkerCount: 13 })
  expect(contents).toContain('H00013_END')
})

it('replays every tool call and result in the dense-tool workload', () => {
  const fixture = history(3, '/synthetic-workspace', WORKLOADS.tools)
  const restored = restore(fixture)
  expect(fixture.dimensions).toMatchObject({ turns: 3, toolCount: 12, assistantMarkerCount: 3, historyMarkerCount: 4 })
  const messages = restored.deriveMessages()
  const calls = messages.flatMap(message => message.content.filter(block => block.type === 'tool-call'))
  const results = messages.flatMap(message => message.content.filter(block => block.type === 'tool-result'))
  expect(new Set(calls.map(call => call.id)).size).toBe(12)
  expect(results.map(result => result.toolCallId)).toEqual(calls.map(call => call.id))
  expect(results.every(result => result.content.some(block => block.type === 'text' && block.text.startsWith('Synthetic tool output: observed value matches expected value.\n')))).toBe(true)
  expect(JSON.stringify(results.at(-1))).toContain('H00004_END')
  expect(fixture.dimensions.toolOutputBytes).toBeGreaterThan(90_000)
})

it('preserves every 4 KiB section of large answers and counts their complete byte size', () => {
  const fixture = history(2, '/synthetic-workspace', WORKLOADS['large-output'])
  const restored = restore(fixture)
  const answers = restored.deriveMessages().filter(message => message.role === 'assistant')
    .flatMap(message => message.content.filter(block => block.type === 'text').map(block => block.text))
  expect(answers.map(answer => Buffer.byteLength(answer))).toEqual([65_536, 65_536])
  const markers = answers.flatMap(answer => [...answer.matchAll(/H\d{5}_END/g)].map(match => match[0]))
  expect(markers).toEqual(Array.from({ length: 32 }, (_, index) => `H${String(index + 1).padStart(5, '0')}_END`))
  expect(fixture.dimensions).toMatchObject({ turns: 2, historyMarkerCount: 32, assistantTextBytes: 131_072, maxAssistantTextBytes: 65_536 })
})

it('includes the last visible delta before the terminal finish', () => {
  const chunks = reply()
  expect(chunks.at(-1)).toEqual({ type: 'finish', reason: { kind: 'stop' } })
  expect(chunks.filter(chunk => chunk.type === 'text-delta')).toHaveLength(100)
  expect(chunks.at(-3)).toEqual({ type: 'text-delta', index: 0, text: '\nPERF_STREAM_DONE' })
})

it('keeps failed samples out of medians and distinguishes all-failed workloads', () => {
  const memory = { beforeGc: process.memoryUsage(), afterGc: process.memoryUsage(), resources: process.resourceUsage(), cpu: process.cpuUsage(), sequence: 1 }
  const sample: Sample = { workload: 'fresh', iteration: 0, dimensions: { ...history(0, '/synthetic-workspace').dimensions, fileBytes: 0, sessionId: 'synthetic' },
    initialInputMs: 5, firstInputMs: 80, historyMarkersAtFirstInput: 0, readyMs: 100, idleInputMs: [1, 7, 2], idleBytes: 0, initialBytes: 1000, firstDeltaMs: 10, liveInputMs: 3, streamMs: 1500, streamBytes: 0,
    historyMarkerOccurrences: 0, readyMemory: memory, settledMemory: memory }
  const results = summarize([sample, { ...sample, iteration: 1, firstInputMs: 120, readyMs: 300, initialBytes: 3000 },
    { workload: 'fresh', iteration: 2, error: 'failed' }, { workload: 'tail', iteration: 0, error: 'failed' }])
  expect(results[0]).toMatchObject({ workload: 'fresh', completed: 2, failed: 1, firstInputMs: 100, readyMs: 200, maxIdleInputMs: 7, initialBytes: 2000 })
  expect(results[1]).toMatchObject({ workload: 'tail', completed: 0, failed: 1, firstInputMs: null, readyMs: null, retainedHeapMiB: null, settledRetainedHeapMiB: null, initialBytes: null })
})
