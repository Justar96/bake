/** Loop-shape metrics over synthetic Bake and pi event streams. */
import { describe, expect, test } from 'bun:test'
import {
  backgroundStarts, type Call, compactions, editCheckSplits, excessRequests, normalizeBake, normalizePi, orientationCalls, ranCheck,
  runawayAbort, shellEdits, verifiedBeforeFinal,
} from './metrics.ts'

const TEST = /\bnode\s+(?:\.\/)?test\.cjs\b/
const WORKSPACE = '/tmp/eval/workspace'
let nextId = 0
const call = (step: number, tool: string, input: Record<string, unknown> = {}): Call => ({ tool, input, callId: `c${nextId++}`, step })
const bash = (step: number, command: string, extra: Record<string, unknown> = {}) => call(step, 'bash', { command, ...extra })

/** A Bake `--json` stream: one array of tool calls per step, then the final reply. */
function bakeStream(steps: [string, Record<string, unknown>][][], final = 'done'): any[] {
  const events: any[] = [{ type: 'session', sessionId: 's', cwd: WORKSPACE }, { type: 'status', phase: 'turn_start', turn: 1 }]
  steps.forEach((calls, step) => {
    events.push({ type: 'status', phase: 'step_start', turn: 1, step })
    for (const [tool, input] of calls) {
      const callId = `b${nextId++}`
      events.push({ type: 'tool_call', callId, tool, input })
      events.push({ type: 'tool_result', callId, status: tool === 'fail' ? 'error' : 'completed', result: 'ok' })
    }
    events.push({ type: 'status', phase: 'step_end', turn: 1, step, usage: { inputTokens: 10, outputTokens: 1 } })
  })
  events.push({ type: 'status', phase: 'turn_end', turn: 1, reason: 'stop' }, { type: 'final', text: final })
  return events
}

describe('normalization', () => {
  test('assigns each Bake call the step that made it', () => {
    const normalized = normalizeBake(bakeStream([[['read', { file_path: 'a' }], ['read', { file_path: 'b' }]], [['edit', {}]], []]))
    expect(normalized.calls.map(c => [c.tool, c.step])).toEqual([['read', 0], ['read', 0], ['edit', 1]])
    expect(normalized.steps).toHaveLength(3)
    expect(normalized.final).toBe('done')
  })

  test('assigns each pi call the assistant message that made it', () => {
    const assistant = (text: string) => ({ type: 'message_end', message: { role: 'assistant', content: [{ type: 'text', text }], usage: { input: 5, output: 1 } } })
    const normalized = normalizePi([
      assistant(''), { type: 'tool_execution_start', toolName: 'read', toolCallId: 'p1', args: { path: 'a' } },
      { type: 'tool_execution_end', toolCallId: 'p1', result: { content: [{ type: 'text', text: 'x' }] } },
      assistant(''), { type: 'tool_execution_start', toolName: 'bash', toolCallId: 'p2', args: { command: 'node test.cjs' } },
      { type: 'tool_execution_start', toolName: 'read', toolCallId: 'p3', parentToolCallId: 'p2', args: {} },
      assistant('fixed'),
    ])
    expect(normalized.calls.map(c => [c.tool, c.step])).toEqual([['read', 0], ['bash', 1]])
    expect(normalized.final).toBe('fixed')
  })
})

describe('edit/check splits', () => {
  test('count an edit-only step followed by a step that opens with the check', () => {
    const split = [call(0, 'read'), call(1, 'edit'), bash(2, 'node test.cjs')]
    expect(editCheckSplits(split, TEST)).toBe(1)
  })

  test('do not count an edit sent with its check, or a later step that checks second', () => {
    expect(editCheckSplits([call(0, 'read'), call(1, 'edit'), bash(1, 'node test.cjs')], TEST)).toBe(0)
    expect(editCheckSplits([call(1, 'edit'), call(2, 'read'), bash(2, 'node test.cjs')], TEST)).toBe(0)
    expect(editCheckSplits([call(1, 'edit'), call(2, 'read'), bash(3, 'node test.cjs')], TEST)).toBe(0)
  })

  test('count a shell edit as an edit', () => {
    expect(editCheckSplits([bash(0, 'sed -i s/floor/round/ src/money.js'), bash(1, 'node ./test.cjs')], TEST)).toBe(1)
    expect(shellEdits([bash(0, 'sed -i s/floor/round/ src/money.js'), bash(1, 'node test.cjs')])).toBe(1)
  })
})

describe('orientation calls', () => {
  test('count pwd, ls, find, and glob at the root before the first read', () => {
    const calls = [
      bash(0, 'pwd'), bash(0, 'ls -la'), bash(0, `cd ${WORKSPACE} && ls`), bash(0, 'find . -name "*.js"'), call(0, 'glob', { pattern: '**/*.js' }),
      call(1, 'read', { file_path: 'src/money.js' }), bash(2, 'ls'),
    ]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(5)
  })

  test('do not count a listing of a subdirectory, a search, or a targeted glob', () => {
    const calls = [bash(0, 'ls src'), bash(0, 'find src -name "*.js"'), call(0, 'grep', { pattern: 'roundMoney' }), call(0, 'glob', { pattern: '*.js', path: 'src' }), bash(0, 'node test.cjs')]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(0)
  })
})

describe('verification', () => {
  test('is verified when the check runs after the last edit, the same call included', () => {
    expect(verifiedBeforeFinal([call(0, 'edit'), bash(0, 'node test.cjs')], TEST)).toBe(true)
    expect(verifiedBeforeFinal([bash(0, 'sed -i s/a/b/ src/x.js && node test.cjs')], TEST)).toBe(true)
  })

  test('is unverified when an edit follows the last check, and null with no edit', () => {
    expect(verifiedBeforeFinal([call(0, 'edit'), bash(1, 'node test.cjs'), call(2, 'write')], TEST)).toBe(false)
    expect(verifiedBeforeFinal([call(0, 'read'), bash(1, 'node test.cjs')], TEST)).toBeNull()
    expect(ranCheck([call(0, 'edit')], TEST)).toBe(false)
    expect(ranCheck([bash(0, 'cd sub && node test.cjs')], TEST)).toBe(true)
  })

  test('uses the scenario check, so the default test is not the instructions check', () => {
    const project = /\bnode\s+(?:\.\/)?scripts\/check\.cjs\b/
    expect(verifiedBeforeFinal([call(0, 'edit'), bash(1, 'node test.cjs')], project)).toBe(false)
    expect(verifiedBeforeFinal([call(0, 'edit'), bash(1, 'node scripts/check.cjs --all')], project)).toBe(true)
  })
})

describe('counters', () => {
  test('excess requests are those above the floor, never negative', () => {
    expect(excessRequests(7, 3)).toBe(4)
    expect(excessRequests(2, 3)).toBe(0)
  })

  test('background starts count shell calls run in the background', () => {
    expect(backgroundStarts([bash(0, 'node slow-check.cjs', { run_in_background: true }), bash(0, 'node test.cjs'), call(1, 'job_output')])).toBe(1)
  })

  test('the runaway guard abort is recognized in any captured text', () => {
    const guard = 'LlmError: model "gpt-6.1-sol" streamed 2001 whitespace characters after the arguments of tool call "edit" without closing them'
    expect(runawayAbort(['', 'final', guard])).toBe(true)
    expect(runawayAbort(['request limit', 'Evaluation transport failure'])).toBe(false)
  })

  test('compactions count completed summaries and failed attempts', () => {
    expect(compactions([
      { type: 'compaction/start', data: {} }, { type: 'compaction/summary', data: {} }, { type: 'compaction/end', data: {} },
      { type: 'compaction/start', data: {} }, { type: 'compaction/end', data: { error: 'still above threshold' } },
    ])).toEqual({ summaries: 1, errors: 1 })
  })
})
