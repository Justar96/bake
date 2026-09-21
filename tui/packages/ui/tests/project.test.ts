/**
 * Projection behavior. Runs under `bun test` because the module under test is
 * pure — no harness, no Node built-ins, no clock.
 */

import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { project } from '../src/project.ts'

/** Build a session event literal without restating the durable envelope. */
const event = (partial: unknown): SessionEvent => partial as SessionEvent

describe('project', () => {
  it('renders a human prompt as a user row', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'run the tests' }] },
    }))).toEqual([{ kind: 'user', text: 'run the tests' }])
  })

  it('drops synthetic context injected by the loop', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'plugin' }, content: [{ type: 'text', text: 'AGENTS.md changed' }] },
    }))).toEqual([])
  })

  it('separates reasoning from answer text, in order', () => {
    expect(project(event({
      type: 'assistant/message',
      data: { message: { content: [
        { type: 'reasoning', text: 'checking the config' },
        { type: 'text', text: 'done' },
      ] } },
    }))).toEqual([
      { kind: 'reasoning', text: 'checking the config' },
      { kind: 'assistant', text: 'done' },
    ])
  })

  it('ignores a compaction rewrite of a tool result', () => {
    const data = { message: { content: [{ toolCallId: 'c1', isError: false, content: 'ok' }] } }
    expect(project(event({ type: 'tool/result', surfaceOp: 'append', data })))
      .toEqual([{ kind: 'tool-result', callId: 'c1', ok: true, text: 'ok' }])
    expect(project(event({ type: 'tool/result', surfaceOp: 'replace', data }))).toEqual([])
  })

  it('surfaces a failed turn as an error notice', () => {
    expect(project(event({
      type: 'turn/end',
      data: { turn: 1, reason: { kind: 'error', error: { code: 'MISSING_CREDENTIAL', message: 'no API key' } } },
    }))).toEqual([{ kind: 'notice', tone: 'error', text: 'MISSING_CREDENTIAL: no API key' }])
  })

  it('renders nothing for an event type this build does not know', () => {
    expect(project(event({ type: 'some/future/event', data: {} }))).toEqual([])
  })
})
