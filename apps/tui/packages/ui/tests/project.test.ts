/**
 * Projection behavior. Runs under `bun test` because the module under test is
 * pure — no harness, no Node built-ins, no clock.
 */

import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { announcedCalls, argumentsTitle, formatRate, outputRate, project, projector, type Projector } from '../src/project.ts'
import { PENDING_ARGUMENTS } from '../src/present.ts'
import { dictionaries } from '../src/copy.ts'

/** Build a session event literal without restating the durable envelope. */
const event = (partial: unknown): SessionEvent => partial as SessionEvent

/** A projector whose lookup finds no tool. Every call keeps its raw arguments. */
const bare = (): Projector => projector(dictionaries.en, () => undefined)

describe('project', () => {
  it('renders a human prompt as a user row', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'run the tests' }] },
    }), bare())).toEqual([{ kind: 'user', text: 'run the tests' }])
  })

  it('drops synthetic context injected by the loop', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'plugin' }, content: [{ type: 'text', text: 'AGENTS.md changed' }] },
    }), bare())).toEqual([])
  })

  it('separates reasoning from answer text, in order', () => {
    expect(project(event({
      type: 'assistant/message',
      data: { message: { content: [
        { type: 'reasoning', text: 'checking the config' },
        { type: 'text', text: 'done' },
      ] } },
    }), bare())).toEqual([
      { kind: 'reasoning', text: 'checking the config' },
      { kind: 'assistant', text: 'done' },
    ])
  })

  it('reads the calls a message announces, without committing them as rows', () => {
    const message = event({
      type: 'assistant/message',
      data: { message: { content: [
        { type: 'text', text: 'Reading both' },
        { type: 'tool-call', id: 'c1', name: 'read', arguments: '{"file_path":"a.ts"}' },
        { type: 'tool-call', id: 'c2', name: 'read', arguments: '{"file_path":"b.ts"}' },
      ] } },
    })
    expect(announcedCalls(message)).toEqual([
      { kind: 'tool-call', callId: 'c1', tool: 'read', input: PENDING_ARGUMENTS },
      { kind: 'tool-call', callId: 'c2', tool: 'read', input: PENDING_ARGUMENTS },
    ])
    expect(project(message, bare())).toEqual([{ kind: 'assistant', text: 'Reading both' }])
    expect(announcedCalls(event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'completed' } } }))).toEqual([])
  })

  it('ignores a compaction rewrite of a tool result', () => {
    const data = { message: { content: [{ toolCallId: 'c1', isError: false, content: 'ok' }] } }
    expect(project(event({ type: 'tool/result', surfaceOp: 'append', data }), bare()))
      .toEqual([{ kind: 'tool-result', callId: 'c1', ok: true, text: 'ok' }])
    expect(project(event({ type: 'tool/result', surfaceOp: 'replace', data }), bare())).toEqual([])
  })

  it('surfaces a failed turn as an error notice', () => {
    expect(project(event({
      type: 'turn/end',
      data: { turn: 1, reason: { kind: 'error', error: { code: 'MISSING_CREDENTIAL', message: 'no API key' } } },
    }), bare())).toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'error', text: 'MISSING_CREDENTIAL: no API key' }])
  })

  it('words a cancelled turn and a compaction in the reader locale', () => {
    expect(project(event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'interrupted' } } }), bare()))
      .toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: dictionaries.en.cancelled }])
    expect(project(event({ type: 'compaction/summary', data: {} }), projector(dictionaries.zh, () => undefined)))
      .toEqual([{ kind: 'notice', tone: 'info', text: dictionaries.zh.compacted }])
  })

  it.each(['en', 'zh'] as const)('renders each turn ending without treating it as agent idle (%s)', locale => {
    const copy = dictionaries[locale]
    const endings = [
      ['completed', copy.turnCompleted, 'info'],
      ['blocked', copy.turnBlocked, 'warn'],
      ['max-tokens', copy.turnMaxTokens, 'warn'],
      ['aborted', copy.cancelled, 'warn'],
      ['plugin-stop', 'plugin-stop', 'warn'],
    ] as const
    for (const [kind, label, tone] of endings) {
      expect(project(event({ type: 'turn/end', data: { turn: 7, reason: { kind } } }), projector(copy, () => undefined)))
        .toEqual([{ kind: 'notice', placement: 'turn-end', tone, text: label }])
    }
    expect(project(event({ type: 'turn/end', surfaceOp: 'replace', data: { turn: 7, reason: { kind: 'completed' } } }), bare()))
      .toEqual([])
  })

  it('keeps a command apart from the words the user sent the model', () => {
    expect(project(event({ type: 'command/run', data: { name: 'model', args: ' deepseek/chat' } }), bare()))
      .toEqual([{ kind: 'command', name: 'model', args: ' deepseek/chat' }])
  })

  it('presents a call through the tool that declared how it reads', () => {
    const seam = projector(dictionaries.en, name => name === 'bash'
      ? { presentCall: (args: unknown) => ({ card: 'terminal' as const, title: (args as { command: string }).command, description: 'List the directory' }) }
      : undefined)
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'bash', arguments: '{"command":"ls -a"}' },
    }), seam)).toEqual([{
      kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a',
      detail: [{ text: 'List the directory' }],
    }])
  })

  it('reads the arguments as fields when no tool claims the call', () => {
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'mystery', arguments: '{"a":1}' },
    }), bare())).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'mystery', input: 'a: 1' }])
  })

  it('heads a call by the field it acts on, and keeps text that is not a JSON object as sent', () => {
    expect(argumentsTitle('{"command":"cargo check 2>&1 | head","description":"Check"}')).toBe('cargo check 2>&1 | head')
    expect(argumentsTitle('{"file_path":"/x.md","limit":5}')).toBe('/x.md')
    expect(argumentsTitle('{"a":"b","n":[1,2]}')).toBe('a: b, n: [1,2]')
    for (const raw of ['{"pa', '[1,2]', '"text"']) expect(argumentsTitle(raw)).toBe(raw)
  })

  it('renders a result card in place of the model-facing text', () => {
    const seam = projector(dictionaries.en, () => ({
      presentCall: () => ({ card: 'generic' as const, title: 'Read notes.md' }),
      presentResult: () => ({ card: 'read' as const, path: 'notes.md', offset: 1, totalLines: 2,
        lines: [{ number: 1, text: 'first' }, { number: 2, text: 'second' }] }),
    }))
    project(event({ type: 'tool/call', data: { callId: 'c1', name: 'read', arguments: '{}' } }), seam)
    expect(project(event({
      type: 'tool/result',
      data: { message: { content: [{ toolCallId: 'c1', isError: false, content: 'raw envelope text' }] } },
    }), seam)).toEqual([{
      kind: 'tool-result', callId: 'c1', ok: true, text: '',
      detail: [{ text: '1  first', source: 'notes.md', codeOffset: 3, number: 1, codeStart: true },
        { text: '2  second', source: 'notes.md', codeOffset: 3, number: 2 }],
    }])
  })

  it('survives a tool whose presenter throws', () => {
    const seam = projector(dictionaries.en, () => ({ presentCall: () => { throw new Error('bad args') } }))
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'brittle', arguments: '{"a":1}' },
    }), seam)).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'brittle', input: 'a: 1' }])
  })

  it('renders nothing for an event type this build does not know', () => {
    expect(project(event({ type: 'some/future/event', data: {} }), bare())).toEqual([])
  })
})

describe('outputRate', () => {
  /** A committed answer whose first token arrived at 1000 and which finished at `finish`. */
  const answer = (options: { finish?: string, outputTokens?: number, interrupted?: true, at?: number } = {}) => ({
    turn: 1, step: 1,
    message: { role: 'assistant', content: [{ type: 'text', text: 'Done.' }] },
    ...options.outputTokens === 0 ? {} : { usage: { inputTokens: 10, outputTokens: options.outputTokens ?? 120 } },
    ...options.interrupted === undefined ? {} : { interrupted: true },
    stream: [
      { type: 'chunk', time: 400, chunk: { type: 'block-start', index: 0, blockType: 'reasoning' } },
      { type: 'reasoning-chunks', time0: 1000, index: 0, dt: [50], texts: ['Hm', 'm'] },
      { type: 'text-chunks', time0: 1500, index: 1, dt: [], texts: ['Done.'] },
      { type: 'chunk', time: options.at ?? 4000, chunk: { type: 'finish', reason: { kind: options.finish ?? 'stop' } } },
    ],
  }) as never

  it('divides the reported output tokens by the time from the first token to the finish', () => {
    // The wait before the first token, from 400, is not generation time.
    expect(outputRate(answer())).toEqual({ tokens: 120, ms: 3000 })
    expect(formatRate({ tokens: 120, ms: 3000 }, dictionaries.en)).toBe('120 tokens \u00b7 40.0 tok/s')
    expect(formatRate({ tokens: 4200, ms: 20_000 }, dictionaries.en)).toBe('4.2k tokens \u00b7 210 tok/s')
    expect(formatRate({ tokens: 120, ms: 3000 }, dictionaries.zh)).toBe('120 token \u00b7 40.0 token/\u79d2')
  })

  it('reports nothing for a step that is not a whole final answer', () => {
    expect(outputRate(answer({ finish: 'tool-calls' }))).toBeUndefined()
    expect(outputRate(answer({ interrupted: true }))).toBeUndefined()
    expect(outputRate(answer({ outputTokens: 0 }))).toBeUndefined()
    expect(outputRate(answer({ at: 1000 }))).toBeUndefined()
  })

  it('puts the rate under the answer it measures, and only under an answer', () => {
    expect(project(event({ type: 'assistant/message', data: answer() }), bare())).toEqual([
      { kind: 'assistant', text: 'Done.' },
      { kind: 'rate', text: '120 tokens \u00b7 40.0 tok/s' },
    ])
    const silent = { ...(answer() as object), message: { role: 'assistant', content: [{ type: 'reasoning', text: 'Hmm' }] } }
    expect(project(event({ type: 'assistant/message', data: silent }), bare())).toEqual([{ kind: 'reasoning', text: 'Hmm' }])
  })
})
