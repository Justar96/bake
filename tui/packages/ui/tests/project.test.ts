/**
 * Projection behavior. Runs under `bun test` because the module under test is
 * pure — no harness, no Node built-ins, no clock.
 */

import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { project, projector, type Projector } from '../src/project.ts'
import { dictionaries } from '../src/copy.ts'

/** Build a session event literal without restating the durable envelope. */
const event = (partial: unknown): SessionEvent => partial as SessionEvent

/** A projector whose lookup finds no tool: every call keeps its raw arguments. */
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
    }), bare())).toEqual([{ kind: 'notice', tone: 'error', text: 'MISSING_CREDENTIAL: no API key' }])
  })

  it('words a cancelled turn and a compaction in the reader locale', () => {
    expect(project(event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'interrupted' } } }), bare()))
      .toEqual([{ kind: 'notice', tone: 'warn', text: dictionaries.en.cancelled }])
    expect(project(event({ type: 'compaction/summary', data: {} }), projector(dictionaries.zh, () => undefined)))
      .toEqual([{ kind: 'notice', tone: 'info', text: dictionaries.zh.compacted }])
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

  it('keeps the raw arguments when no tool claims the call', () => {
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'mystery', arguments: '{"a":1}' },
    }), bare())).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'mystery', input: '{"a":1}' }])
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
      detail: [{ text: '1  first' }, { text: '2  second' }],
    }])
  })

  it('survives a tool whose presenter throws', () => {
    const seam = projector(dictionaries.en, () => ({ presentCall: () => { throw new Error('bad args') } }))
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'brittle', arguments: '{"a":1}' },
    }), seam)).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'brittle', input: '{"a":1}' }])
  })

  it('renders nothing for an event type this build does not know', () => {
    expect(project(event({ type: 'some/future/event', data: {} }), bare())).toEqual([])
  })
})
