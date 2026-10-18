/** Loop-shape metrics over synthetic Bake and pi event streams. */
import { describe, expect, test } from 'bun:test'
import {
  backgroundCompactions, backgroundStarts, type Call, compactions, contextMasking, editCheckSplits, excessRequests, isBareGlob, jsonModeCompactions, nativeReadBatches, nativeSummaryUsage, normalizeBake, normalizePi, orientationCalls, ranCheck,
  readsFile, requestsOverFloor, runawayAbort, shellEdits, skillLoads, verifiedBeforeFinal,
} from './metrics.ts'

const TEST = /\bnode\s+(?:\.\/)?test\.cjs\b/
const WORKSPACE = '/tmp/eval/workspace'

test('native experiment metrics count committed masking and same-response read batches', () => {
  expect(contextMasking([{ type: 'context_masking', maskedResults: 2, removedBytes: 1200 },
    { type: 'context_masking', maskedResults: 3, removedBytes: 700 }, { type: 'compaction_end' }]))
    .toEqual({ commits: 2, maskedResults: 5, removedBytes: 1900 })
  const assistant = (names: string[]) => ({ type: 'message_end', message: { role: 'assistant', content: names.map(name => ({ type: 'toolCall', name })) } })
  expect(nativeReadBatches([assistant(['read', 'read', 'ls', 'grep']), assistant(['read']),
    assistant(['read', 'edit']), assistant([]), { type: 'tool_execution_start', toolName: 'read' }]))
    .toEqual({ batches: 2, multiCallBatches: 1, calls: 5, maxCalls: 4 })
})
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
  test('counts native summary costs once, including discarded attempts and missing usage', () => {
    const usage = { input: 7, output: 2, cacheRead: 3, cacheWrite: 0, totalTokens: 12 }
    const events = [
      { type: 'compaction_end', result: { usage } },
      { type: 'background_compaction', status: 'started', elapsedMs: 0, attempts: [] },
      { type: 'background_compaction', status: 'committed', elapsedMs: 5, result: { usage }, attempts: [{ dispatched: true, usage }] },
      { type: 'background_compaction', status: 'discarded', elapsedMs: 8, attempts: [{ dispatched: true, usage }, { dispatched: true, usage: null }, { dispatched: false, usage: null }] },
    ]
    expect(nativeSummaryUsage(events)).toEqual([
      ...Array.from({ length: 3 }, () => ({ inputTokens: 7, outputTokens: 2, cacheReadTokens: 3, cacheWriteTokens: 0, totalTokens: 12 })),
      undefined,
    ])
    expect(backgroundCompactions(events)).toEqual({ started: 1, committed: 1, discarded: 1, attempts: 4, dispatched: 3, missingUsage: 1, elapsedMs: 13 })
  })

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
  test('count pwd, root listings, an unfiltered find, and a bare glob before the first read', () => {
    const calls = [
      bash(0, 'pwd'), bash(0, 'ls -la'), bash(0, `cd ${WORKSPACE} && ls`), bash(0, 'find . -type f'), bash(0, 'pwd && ls && tree'),
      call(0, 'glob', { pattern: '**/*.js' }), call(0, 'glob', { pattern: '*' }), call(0, 'glob', { pattern: '**' }), call(0, 'glob', { pattern: '**/*' }),
      call(1, 'read', { file_path: 'src/money.js' }), bash(2, 'ls'),
    ]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(9)
  })

  test('do not count a glob whose pattern names a file stem, such as the path_discovery search', () => {
    expect(orientationCalls([call(0, 'glob', { pattern: '**/money.js' })], [WORKSPACE])).toBe(0)
    expect(orientationCalls([call(0, 'glob', { pattern: 'src/*.js' }), call(0, 'glob', { pattern: '**/test*' })], [WORKSPACE])).toBe(0)
    expect(isBareGlob('**/*.{js,ts}')).toBe(true)
    expect(isBareGlob('**/money.js')).toBe(false)
  })

  test('do not count a find with a name or path filter', () => {
    const calls = [bash(0, 'find . -name "*.js"'), bash(0, 'find . -path "*/billing/*"'), bash(0, 'find . -iname money.js')]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(0)
  })

  test('do not count a shell command with any stage beyond a listing', () => {
    const calls = [bash(0, 'ls && node test.cjs'), bash(0, 'ls | head'), bash(0, 'pwd; grep -rn roundMoney .'), bash(0, `cd ${WORKSPACE} && cat test.cjs`)]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(0)
  })

  test('do not count a listing of a subdirectory, a search, or a targeted glob', () => {
    const calls = [bash(0, 'ls src'), bash(0, 'find src -type f'), call(0, 'grep', { pattern: 'roundMoney' }), call(0, 'glob', { pattern: '*.js', path: 'src' }), bash(0, 'node test.cjs')]
    expect(orientationCalls(calls, [WORKSPACE])).toBe(0)
  })

  test('end the window at a shell read of a file', () => {
    for (const read of ['cat src/money.js', 'head -n 20 src/money.js', 'tail -5 test.cjs', "sed -n '1,40p' src/money.js", 'less src/money.js', `cd ${WORKSPACE} && cat test.cjs | grep assert`]) {
      expect(orientationCalls([bash(0, 'ls'), bash(1, read), bash(2, 'ls'), bash(2, 'pwd')], [WORKSPACE])).toBe(1)
    }
  })

  test('do not take a write, a filter on piped input, or sed without -n for a read', () => {
    for (const command of ["cat > src/money.js <<'EOF'", 'ls | head -n 5', 'ls | tail -3', "sed -i 's/a/b/' src/money.js", "printf x | sed -n '1p'"]) {
      expect(readsFile(command)).toBe(false)
    }
    expect(orientationCalls([bash(0, "cat > notes.txt <<'EOF'"), bash(1, 'ls')], [WORKSPACE])).toBe(1)
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

describe('skill loads', () => {
  test('count the skill tool, a read of a SKILL.md under .agents/skills, and a shell print of one, each skill once in first-load order', () => {
    expect(skillLoads([
      call(0, 'read', { path: '/w/.agents/skills/money-check/SKILL.md' }),
      call(0, 'read', { path: 'src/money.js' }),
      call(1, 'skill', { name: 'release-notes' }),
      call(1, 'read', { file_path: '.agents/skills/money-check/SKILL.md' }),
      bash(2, 'cat .agents/skills/translations/SKILL.md .agents/skills/docker-deploy/SKILL.md'),
    ])).toEqual(['money-check', 'release-notes', 'translations', 'docker-deploy'])
    expect(skillLoads([bash(0, 'cd .agents/skills/money-check && cat SKILL.md')])).toEqual(['money-check'])
    expect(skillLoads([bash(0, "head -n 40 '/w/.agents/skills/money-check/SKILL.md'")])).toEqual(['money-check'])
  })

  test('count a glob or a find that prints every skill as *, a load of all of them', () => {
    expect(skillLoads([bash(0, 'cat .agents/skills/*/* 2>/dev/null | head -50')])).toEqual(['*'])
    expect(skillLoads([bash(0, 'cat .agents/skills/*/SKILL.md')])).toEqual(['*'])
    expect(skillLoads([bash(0, `cd /w && find .agents -type f | xargs -I{} sh -c 'echo "== {}"; cat "{}"'`)])).toEqual(['*'])
    expect(skillLoads([bash(0, 'find .agents -name SKILL.md -exec cat {} +')])).toEqual(['*'])
    expect(skillLoads([bash(0, 'find .agents -type f | head'), bash(1, 'ls .agents/skills/*')])).toEqual([])
  })

  test('do not count listing, searching, or running a skill\'s other files', () => {
    expect(skillLoads([
      call(0, 'ls', { path: '.agents/skills' }),
      bash(0, 'ls -R .agents/skills'),
      bash(0, 'grep -r description .agents/skills/*/SKILL.md'),
      bash(1, 'node .agents/skills/money-check/scripts/check.cjs --all'),
      call(1, 'read', { path: '.agents/skills/money-check/scripts/check.cjs' }),
      call(2, 'read', { path: 'docs/SKILL.md' }),
    ])).toEqual([])
  })
})

describe('counters', () => {
  test('excess requests are those above the floor, never negative; requests over the floor keep the sign', () => {
    expect(excessRequests(7, 3)).toBe(4)
    expect(excessRequests(2, 3)).toBe(0)
    expect(requestsOverFloor(7, 3)).toBe(4)
    expect(requestsOverFloor(2, 3)).toBe(-1)
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

  test('JSON-mode compactions count completed and failed compaction_end events, not aborted ones', () => {
    expect(jsonModeCompactions([
      { type: 'compaction_start' }, { type: 'compaction_end', result: { summary: 's' }, aborted: false },
      { type: 'compaction_start' }, { type: 'compaction_end', aborted: false, errorMessage: 'summary failed' },
      { type: 'compaction_start' }, { type: 'compaction_end', aborted: true },
    ])).toEqual({ summaries: 1, errors: 1 })
  })
})
