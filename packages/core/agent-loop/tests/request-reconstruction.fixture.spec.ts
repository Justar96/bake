/**
 * Real-runtime request-reconstruction fixture for the first turn of the
 * original THEOREM (`request-reconstruction.spec.ts`), which remains the oracle
 * for its full three-request scenario. The live composed loop must dispatch
 * exactly the hand-written `expected-requests.json`, and the committed
 * `session.jsonl`, a byte-exact capture of one such run, must replay to the
 * same requests. Only request message ids are normalized; the log keeps its
 * captured ids and timing, so a fresh run is not expected to reproduce its
 * bytes. Tests only read the committed files; a new or corrected scenario gets
 * a new directory instead of a rewrite.
 */

import { readFile } from 'node:fs/promises'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SESSION_FORMAT_VERSION } from 'bake-session'
import type { JsonValue } from 'bake-util-values'
import { scanLog } from 'bake-session-persistence-jsonl/src/format.ts'
import {
  MessageIdentities,
  compareBytes,
  compareJson,
  encodeLog,
  normalizeRequests,
  parseExpectedRequests,
  projectRequest,
  replayRequests,
  runToolCallTurn,
  toolCallTurnScript,
} from './runtime-fixture.ts'
import type { ScenarioCapture } from './runtime-fixture.ts'

const FIXTURES = new URL('../../../../conformance/runtime/request-reconstruction/', import.meta.url)

async function readFixture(scenario: string): Promise<{ expected: JsonValue[]; log: Buffer }> {
  const directory = new URL(`${scenario}/`, FIXTURES)
  const [expected, log] = await Promise.all([
    readFile(new URL('expected-requests.json', directory)),
    readFile(new URL('session.jsonl', directory)),
  ])
  return { expected: parseExpectedRequests(expected), log }
}

function liveRequests(capture: ScenarioCapture): { [key: string]: JsonValue }[] {
  return normalizeRequests(capture.requests.map(projectRequest))
}

/** Replace the only occurrence of `from` in `log` with `to`, keeping the copy's length. */
function replaceOnce(log: Buffer, from: string, to: string): Buffer {
  const text = log.toString('utf8')
  expect(text.split(from)).toHaveLength(2)
  expect(Buffer.byteLength(to)).toBe(Buffer.byteLength(from))
  return Buffer.from(text.replace(from, to))
}

describe('tool-call-turn request-reconstruction fixture', () => {
  it('dispatches exactly the independently specified requests, which its own log replays', async ({ signal }) => {
    const { expected } = await readFixture('tool-call-turn')
    const capture = await runToolCallTurn(toolCallTurnScript(), signal)

    expect(capture.events.filter(event => event.type === 'step/start')).toHaveLength(2)
    expect(capture.events.filter(event => event.type === 'assistant/message' || event.type === 'assistant/attempt')).toHaveLength(2)
    expect(capture.events.at(-1)).toMatchObject({ type: 'turn/end', data: { reason: { kind: 'completed' } } })
    expect(compareJson(expected, liveRequests(capture), 'requests')).toEqual({ outcome: 'pass' })
    const ownLog = Buffer.from(encodeLog(capture.header, capture.events))
    expect(compareJson(expected, normalizeRequests(replayRequests(ownLog)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('dispatches identical normalized requests from two private runs', async ({ signal }) => {
    const first = liveRequests(await runToolCallTurn(toolCallTurnScript(), signal))
    const second = liveRequests(await runToolCallTurn(toolCallTurnScript(), signal))
    expect(JSON.stringify(second)).toBe(JSON.stringify(first))
  })

  it('stores a complete current-format Session that replays to the expected requests', async () => {
    const fixture = await readFixture('tool-call-turn')
    const { meta, events, committedBytes } = scanLog(fixture.log)
    expect(meta.version).toBe(SESSION_FORMAT_VERSION)
    expect(committedBytes).toBe(fixture.log.length)
    // Admission through a fresh Session, and a byte-exact re-encode of what was read.
    expect(() => Session.create(SessionId(meta.id), events, meta)).not.toThrow()
    expect(compareBytes(fixture.log, Buffer.from(encodeLog(meta, events)))).toEqual({ outcome: 'pass' })
    expect(compareJson(fixture.expected, normalizeRequests(replayRequests(fixture.log)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('unwinds through disposal when the idle wait is aborted', async () => {
    const reason = new Error('test aborted')
    await expect(runToolCallTurn(toolCallTurnScript(), AbortSignal.abort(reason))).rejects.toBe(reason)
  })

  it('fails when a committed fixture file is missing', async () => {
    await expect(readFixture('missing-scenario')).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('refuses a torn log rather than replaying its prefix', async () => {
    const { log } = await readFixture('tool-call-turn')
    // Without its newline the final `turn/end` row is torn, so all of it is uncommitted.
    const torn = log.subarray(0, -1)
    const finalRow = torn.length - torn.lastIndexOf(0x0A) - 1
    expect(() => replayRequests(torn)).toThrow(`log has ${finalRow} uncommitted trailing bytes`)
  })
})

describe('negative controls', () => {
  it('a one-byte change in the scripted tool argument fails the expected requests', async ({ signal }) => {
    const { expected } = await readFixture('tool-call-turn')
    const live = liveRequests(await runToolCallTurn(toolCallTurnScript({ text: 'onf' }), signal))
    expect(compareJson(expected, live, 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests[1].messages[2].content[1].arguments: expected "{\\"text\\":\\"one\\"}", got "{\\"text\\":\\"onf\\"}"',
    })
  })

  it('a changed persisted tool result replays to a mismatching request', async () => {
    const fixture = await readFixture('tool-call-turn')
    const mutated = replaceOnce(fixture.log, '"text":"echo: one"', '"text":"echo: onx"')
    expect(compareJson(fixture.expected, normalizeRequests(replayRequests(mutated)), 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests[1].messages[3].content[0].content[0].text: expected "echo: one", got "echo: onx"',
    })
  })

  it('swapping the system and user message events is refused, or replays out of order once renumbered', async () => {
    const fixture = await readFixture('tool-call-turn')
    const lines = fixture.log.toString('utf8').split('\n')
    const system = lines.findIndex(line => line.startsWith('{"type":"system/message","seq":4,'))
    expect(lines[system + 1]).toMatch(/^\{"type":"user\/message","seq":5,/)
    const swapped = [...lines]
    ;[swapped[system], swapped[system + 1]] = [lines[system + 1]!, lines[system]!]
    expect(() => replayRequests(Buffer.from(swapped.join('\n')))).toThrow('line 5: released v2 row 4 has seq gap (expected 4, got 5)')

    // With sequence numbers rewritten to match, the reader admits the order and replay exposes it.
    swapped[system] = lines[system + 1]!.replace('"seq":5,', '"seq":4,')
    swapped[system + 1] = lines[system]!.replace('"seq":4,', '"seq":5,')
    expect(compareJson(fixture.expected, normalizeRequests(replayRequests(Buffer.from(swapped.join('\n')))), 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests[0].messages[0].role: expected "system", got "user"',
    })
  })

  it('a dropped expected request fails on the request count', async () => {
    const fixture = await readFixture('tool-call-turn')
    expect(compareJson(fixture.expected.slice(0, 1), normalizeRequests(replayRequests(fixture.log)), 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests: expected 1 items, got 2',
    })
  })
})

describe('request normalization', () => {
  const uuid = '0f8e2c1a-4b3d-4e5f-8a9b-1c2d3e4f5a6b'
  const other = '9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c6d'

  it('maps only message ids, consistently across requests, and keeps UUID-shaped text exact', () => {
    const message = { id: uuid, role: 'user', content: [{ type: 'text', text: other }], source: { kind: 'user' } }
    const reply = { ...message, id: other }
    expect(normalizeRequests([{ messages: [message] }, { messages: [message, reply], extra: uuid }])).toEqual([
      { messages: [{ ...message, id: 'message-1' }] },
      { messages: [{ ...message, id: 'message-1' }, { ...reply, id: 'message-2' }], extra: uuid },
    ])
  })

  it('rejects an id that is not a generated UUID', () => {
    expect(() => new MessageIdentities().placeholder('message-1', 'here')).toThrow('here: expected a generated message id, got "message-1"')
  })

  it('rejects an unknown request member and keeps null distinct from omission', async ({ signal }) => {
    const [request] = (await runToolCallTurn(toolCallTurnScript(), signal)).requests
    expect(() => projectRequest({ ...request!, extra: 1 } as never)).toThrow('request member "extra" is not in the fixture projection')
    const projected = projectRequest(request!)
    const withNull = projectRequest({ ...request!, stop: null } as never)
    expect(compareJson(projected, withNull)).toEqual({ outcome: 'fail', detail: '$.stop: unexpected member' })
    expect(projectRequest({ ...request!, stop: undefined } as never)).toEqual(projected)
  })
})
