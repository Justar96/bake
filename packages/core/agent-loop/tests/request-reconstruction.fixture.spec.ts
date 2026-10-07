/**
 * Real-runtime request-reconstruction fixtures. `tool-call-turn` is the first
 * turn of the original THEOREM (`request-reconstruction.spec.ts`), which
 * remains the oracle for its full three-request scenario; `dynamic-tools` adds,
 * removes, and restores a tool across three turns, and `tool-updates.spec.ts`
 * remains the oracle for tool-update routes; `retry-attempt` retries a failed
 * model request with another model, one request per settlement. The live composed loop must
 * dispatch exactly each hand-written `expected-requests.json`, and each
 * committed `session.jsonl`, a byte-exact capture of one such run, must
 * replay to the same requests. Only generated message ids, and in
 * `dynamic-tools` the tool-history anchors that name them, are normalized; the
 * logs keep their captured ids and timing, so a fresh run is not expected to
 * reproduce their bytes. Tests only read the committed files; a new or
 * corrected scenario gets a new directory instead of a rewrite.
 */

import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { describe, expect, it } from 'vitest'
import { Session, SessionId, SESSION_FORMAT_VERSION, foldRequestHeader } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { snapshotJsonValue, type JsonValue } from 'bake-util-values'
import { scanLog } from 'bake-session-persistence-jsonl/src/format.ts'
import {
  FETCH_DESCRIPTION,
  MessageIdentities,
  compareBytes,
  compareJson,
  encodeLog,
  normalizeAnchoredRequests,
  normalizeRequests,
  parseExpectedRequests,
  projectRequest,
  replayRequests,
  runDynamicToolsScenario,
  runRetryAttemptScenario,
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

const DYNAMIC_LOG_SHA256 = '43852e686ea6ef5f599065a7ead57f82d27f8b20e9e936e81b0b0596636e61e2'
const DYNAMIC_ROWS = 38
/** Each request's Assistant settlement seq, where its replay prefix ends. */
const DYNAMIC_CUTS = [8, 15, 25, 35]

function dynamicRequests(capture: ScenarioCapture): { [key: string]: JsonValue }[] {
  return normalizeAnchoredRequests(capture.requests.map(projectRequest))
}

/** The log's request rows: header reasons, update rows, and settlement cuts. */
function requestRows(events: readonly SessionEvent[]) {
  return {
    reasons: events.flatMap(event => event.type === 'request/header' ? [event.data] : [])
      .map(data => [data.reason, Object.hasOwn(data, 'startsSeries')]),
    updates: events.flatMap(event => event.type === 'request/tool-update'
      ? [[event.seq, event.data.headerSeq, event.data.additions, event.data.removals]]
      : []),
    cuts: events.filter(event => event.type === 'assistant/message' || event.type === 'assistant/attempt').map(event => event.seq),
  }
}

const DYNAMIC_ROWS_EXPECTED = {
  reasons: [['initial', false], ['change', false], ['change', false], ['change', false]],
  updates: [[14, 13, ['fetch'], []], [24, 23, [], ['fetch']], [34, 33, ['fetch'], []]],
  cuts: DYNAMIC_CUTS,
}

/** Replace `from` with the same-length `to` in the one log row with seq `seq`. */
function replaceInRow(log: Buffer, seq: number, from: string, to: string): Buffer {
  const lines = log.toString('utf8').split('\n')
  const row = lines[seq + 1]!
  expect(row.startsWith('{"type":') && row.includes(`"seq":${seq},`)).toBe(true)
  expect(row.split(from)).toHaveLength(2)
  expect(Buffer.byteLength(to)).toBe(Buffer.byteLength(from))
  lines[seq + 1] = row.replace(from, to)
  return Buffer.from(lines.join('\n'))
}

describe('dynamic-tools request-reconstruction fixture', () => {
  it('dispatches exactly the independently specified requests, which its own log replays', async ({ signal }) => {
    const { expected } = await readFixture('dynamic-tools')
    const capture = await runDynamicToolsScenario(signal)

    expect(capture.requests).toHaveLength(4)
    expect(capture.events).toHaveLength(DYNAMIC_ROWS)
    expect(requestRows(capture.events)).toEqual(DYNAMIC_ROWS_EXPECTED)
    expect(capture.events.filter(event => event.type === 'system/message')).toHaveLength(1)
    expect(capture.events.filter(event => event.type === 'turn/end').map(event => event.data.reason))
      .toEqual(Array.from({ length: 3 }, () => ({ kind: 'completed' })))
    // A registration failure inside `install` would surface as an error result, not a thrown test.
    expect(capture.events.filter(event => event.type === 'tool/result').map(event => event.data.message.content))
      .toEqual([[{ type: 'tool-result', toolCallId: 'c1', content: [{ type: 'text', text: 'installed' }], isError: false }]])
    for (const request of capture.requests) {
      expect(request.toolUpdates).toBeUndefined()
      expect(request.tools?.some(schema => Object.hasOwn(schema, 'deferLoading'))).toBe(false)
    }
    expect(compareJson(expected, dynamicRequests(capture), 'requests')).toEqual({ outcome: 'pass' })
    const ownLog = Buffer.from(encodeLog(capture.header, capture.events))
    expect(compareJson(expected, normalizeAnchoredRequests(replayRequests(ownLog)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('dispatches identical normalized requests from two private runs', async ({ signal }) => {
    const first = dynamicRequests(await runDynamicToolsScenario(signal))
    const second = dynamicRequests(await runDynamicToolsScenario(signal))
    expect(JSON.stringify(second)).toBe(JSON.stringify(first))
  })

  it('stores a complete current-format Session that replays to the expected requests', async () => {
    const fixture = await readFixture('dynamic-tools')
    expect(createHash('sha256').update(fixture.log).digest('hex')).toBe(DYNAMIC_LOG_SHA256)
    const { meta, events, committedBytes, inheritedEventCount } = scanLog(fixture.log)
    expect(meta.version).toBe(SESSION_FORMAT_VERSION)
    expect([meta.isSeeded, inheritedEventCount, committedBytes]).toEqual([false, 0, fixture.log.length])
    expect(events).toHaveLength(DYNAMIC_ROWS)
    expect(requestRows(events)).toEqual(DYNAMIC_ROWS_EXPECTED)
    expect(() => Session.create(SessionId(meta.id), events, meta)).not.toThrow()
    expect(compareBytes(fixture.log, Buffer.from(encodeLog(meta, events)))).toEqual({ outcome: 'pass' })
    expect(compareJson(fixture.expected, normalizeAnchoredRequests(replayRequests(fixture.log)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('unwinds through disposal when the idle wait is aborted before or between turns', async () => {
    const reason = new Error('test aborted')
    await expect(runDynamicToolsScenario(AbortSignal.abort(reason))).rejects.toBe(reason)
    // Aborting while idle after turn 1 starts turn 2 against a rejected wait; disposal must still quiesce it.
    const controller = new AbortController()
    const turns: number[] = []
    const afterTurn = (turn: number): void => {
      turns.push(turn)
      if (turn === 1) controller.abort(reason)
    }
    await expect(runDynamicToolsScenario(controller.signal, { afterTurn })).rejects.toBe(reason)
    expect(turns).toEqual([1])
  })
})

describe('dynamic-tools negative controls', () => {
  it('a one-byte change in the declared fetch description fails the expected requests', async ({ signal }) => {
    const { expected } = await readFixture('dynamic-tools')
    const live = dynamicRequests(await runDynamicToolsScenario(signal, { fetchDescription: 'fetch a urm' }))
    expect(FETCH_DESCRIPTION).toBe('fetch a url')
    expect(compareJson(expected, live, 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests[1].tools[0].description: expected "fetch a url", got "fetch a urm"',
    })
  })

  it('a redeclared fetch in the last header resets the replayed tool history', async () => {
    const fixture = await readFixture('dynamic-tools')
    const mutated = replaceInRow(fixture.log, 33, '"description":"fetch a url"', '"description":"fetch a urx"')
    const replayed = normalizeAnchoredRequests(replayRequests(mutated))
    const changed = { ...(fixture.expected[3] as { tools: JsonValue[] }).tools[0] as object, description: 'fetch a urx' }
    const install = (fixture.expected[0] as { tools: JsonValue[] }).tools[0]!
    expect(replayed[3]!.toolHistory).toEqual({ tools: [changed, install], updates: [] })
    expect(compareJson(fixture.expected.slice(0, 3), replayed.slice(0, 3), 'requests')).toEqual({ outcome: 'pass' })
    expect(compareJson(fixture.expected, replayed, 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests[3].tools[0].description: expected "fetch a url", got "fetch a urx"',
    })
  })

  it('an update anchored to an earlier message is refused by Session admission', async () => {
    const fixture = await readFixture('dynamic-tools')
    const { events } = scanLog(fixture.log)
    const anchor = (seq: number): string => (events[seq] as Extract<SessionEvent, { type: 'request/tool-update' }>).data.afterMessageId
    const mutated = replaceInRow(fixture.log, 24, anchor(24), anchor(14))
    expect(() => replayRequests(mutated)).toThrow('request/tool-update must follow the current user or tool-result message')
  })

  it('a dropped expected request or a cut past the settlement fails', async () => {
    const fixture = await readFixture('dynamic-tools')
    const replayed = normalizeAnchoredRequests(replayRequests(fixture.log))
    expect(compareJson(fixture.expected.slice(0, 3), replayed, 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests: expected 3 items, got 4',
    })
    // R2's prefix extended through its own settlement carries the response it asked for.
    const { meta, events } = scanLog(fixture.log)
    const late = Session.create(SessionId(meta.id), events.slice(0, DYNAMIC_CUTS[1]! + 1), meta).deriveMessages()
    const messages = normalizeRequests([{ messages: snapshotJsonValue<unknown>(late) as JsonValue }])[0]!.messages
    expect(compareJson((fixture.expected[1] as { messages: JsonValue }).messages, messages!, 'messages')).toEqual({
      outcome: 'fail',
      detail: 'messages: expected 4 items, got 5',
    })
  })

  it('the message-id normalizer leaves anchors raw, and the anchored one refuses foreign anchors', async () => {
    const fixture = await readFixture('dynamic-tools')
    const raw = replayRequests(fixture.log)
    const comparison = compareJson(fixture.expected, normalizeRequests(raw), 'requests')
    expect(comparison).toMatchObject({ outcome: 'fail' })
    expect(comparison.outcome === 'fail' && comparison.detail)
      .toMatch(/^requests\[1\]\.toolHistory\.updates\[0\]\.afterMessageId: expected "message-4", got "[0-9a-f-]{36}"$/)

    const where = 'requests[1].toolHistory.updates[0].afterMessageId'
    const withAnchor = (anchor: JsonValue): { [key: string]: JsonValue }[] => {
      const requests = structuredClone(raw)
      const history = requests[1]!.toolHistory as { updates: { afterMessageId: JsonValue }[] }
      history.updates[0]!.afterMessageId = anchor
      return requests
    }
    const fresh = '0f8e2c1a-4b3d-4e5f-8a9b-1c2d3e4f5a6b'
    expect(() => normalizeAnchoredRequests(withAnchor(fresh))).toThrow(`${where}: ${fresh} is not a message of this request`)
    const later = ((raw[3]!.messages as { id: string }[])[7]!).id
    expect(() => normalizeAnchoredRequests(withAnchor(later))).toThrow(`${where}: ${later} is not a message of this request`)
    expect(() => normalizeAnchoredRequests(withAnchor('message-4'))).toThrow(`${where}: expected a generated message id, got "message-4"`)
    expect(() => normalizeAnchoredRequests(withAnchor(null))).toThrow(`${where}: expected a generated message id, got null`)
    expect(() => normalizeAnchoredRequests([{ ...raw[0]!, toolUpdates: [] }])).toThrow('requests[0].toolUpdates is not normalized')
  })
})

const RETRY_LOG_SHA256 = 'cd79f036ffd20337af3393ab1dcfb57f62aa7434129a6ec3ddea9398c9a7a2db'
const RETRY_ROWS = 14

/** The retry log's header, context, and settlement rows. */
function retryRows(events: readonly SessionEvent[]) {
  return {
    headers: events.flatMap(event => event.type === 'request/header'
      ? [[event.seq, event.data.reason, event.data.header.config.model, Object.hasOwn(event.data, 'startsSeries')]]
      : []),
    contexts: events.flatMap(event => event.type === 'request/context' ? [[event.seq, event.data]] : []),
    settlements: events.flatMap(event => event.type === 'assistant/attempt' || event.type === 'assistant/message'
      ? [[event.seq, event.type]]
      : []),
    absent: events.filter(event => event.type === 'request/tool-update').length
      + events.filter(event => event.type === 'system/message').length - 1,
  }
}

const RETRY_ROWS_EXPECTED = {
  headers: [[6, 'initial', 'mock', false], [9, 'change', 'mock-b', false]],
  contexts: [[7, { provider: 'mock', model: 'mock' }], [10, { provider: 'mock', model: 'mock-b' }]],
  settlements: [[8, 'assistant/attempt'], [11, 'assistant/message']],
  absent: 0,
}

/** Replace the whole row with seq `seq`. */
function replaceRow(log: Buffer, seq: number, row: object): Buffer {
  const lines = log.toString('utf8').split('\n')
  expect(lines[seq + 1]).toMatch(new RegExp(`^\\{"type":"[^"]+","seq":${seq},`))
  lines[seq + 1] = JSON.stringify(row)
  return Buffer.from(lines.join('\n'))
}

describe('retry-attempt request-reconstruction fixture', () => {
  it('dispatches exactly the independently specified requests, which its own log replays', async ({ signal }) => {
    const { expected } = await readFixture('retry-attempt')
    const capture = await runRetryAttemptScenario(signal)

    expect(capture.decisions).toEqual(['retry'])
    expect(capture.requests).toHaveLength(2)
    expect(capture.events).toHaveLength(RETRY_ROWS)
    expect(retryRows(capture.events)).toEqual(RETRY_ROWS_EXPECTED)
    expect(capture.events.at(-1)).toMatchObject({ type: 'turn/end', data: { reason: { kind: 'completed' } } })
    for (const request of capture.requests) expect(request.toolUpdates).toBeUndefined()
    expect(compareJson(expected, liveRequests(capture), 'requests')).toEqual({ outcome: 'pass' })
    const ownLog = Buffer.from(encodeLog(capture.header, capture.events))
    expect(compareJson(expected, normalizeRequests(replayRequests(ownLog)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('dispatches identical normalized requests from two private runs', async ({ signal }) => {
    const first = liveRequests(await runRetryAttemptScenario(signal))
    const second = liveRequests(await runRetryAttemptScenario(signal))
    expect(JSON.stringify(second)).toBe(JSON.stringify(first))
  })

  it('stores a complete current-format Session that replays to the expected requests', async () => {
    const fixture = await readFixture('retry-attempt')
    expect(createHash('sha256').update(fixture.log).digest('hex')).toBe(RETRY_LOG_SHA256)
    const { meta, events, committedBytes, inheritedEventCount } = scanLog(fixture.log)
    expect(meta.version).toBe(SESSION_FORMAT_VERSION)
    expect([meta.isSeeded, inheritedEventCount, committedBytes]).toEqual([false, 0, fixture.log.length])
    expect(events).toHaveLength(RETRY_ROWS)
    expect(retryRows(events)).toEqual(RETRY_ROWS_EXPECTED)
    expect(() => Session.create(SessionId(meta.id), events, meta)).not.toThrow()
    expect(compareBytes(fixture.log, Buffer.from(encodeLog(meta, events)))).toEqual({ outcome: 'pass' })
    expect(compareJson(fixture.expected, normalizeRequests(replayRequests(fixture.log)), 'requests')).toEqual({ outcome: 'pass' })
  })

  it('unwinds through disposal when the idle wait is aborted before the turn or at the retry', async () => {
    const reason = new Error('test aborted')
    await expect(runRetryAttemptScenario(AbortSignal.abort(reason))).rejects.toBe(reason)
    const controller = new AbortController()
    await expect(runRetryAttemptScenario(controller.signal, { onRetry: () => controller.abort(reason) })).rejects.toBe(reason)
  })

  it('bounds wrong retry decisions by the script and the listener', async ({ signal }) => {
    const { expected } = await readFixture('retry-attempt')
    const none = await runRetryAttemptScenario(signal, { retries: 0 })
    expect(none.decisions).toEqual(['delegate'])
    expect(compareJson(expected, liveRequests(none), 'requests')).toEqual({ outcome: 'fail', detail: 'requests: expected 2 items, got 1' })
    const twice = await runRetryAttemptScenario(signal, { failures: 2, retries: 2 })
    expect(twice.decisions).toEqual(['retry', 'retry'])
    expect(compareJson(expected, liveRequests(twice), 'requests')).toEqual({ outcome: 'fail', detail: 'requests: expected 2 items, got 3' })
    // Two requests equal to the expectation, but the second also fails and the turn ends in error.
    const exhausted = await runRetryAttemptScenario(signal, { failures: 2, retries: 1 })
    expect(exhausted.decisions).toEqual(['retry', 'delegate'])
    expect(compareJson(expected, liveRequests(exhausted), 'requests')).toEqual({ outcome: 'pass' })
    expect(exhausted.events.at(-1)).toMatchObject({ type: 'turn/end', data: { reason: { kind: 'error' } } })
  })
})

describe('retry-attempt negative controls', () => {
  it('without the attempt settlement, one request replays', async () => {
    const fixture = await readFixture('retry-attempt')
    const withoutAttempt = replaceRow(fixture.log, 8, { type: 'request/context', seq: 8, time: 1, data: { provider: 'mock', model: 'mock' } })
    expect(compareJson(fixture.expected, normalizeRequests(replayRequests(withoutAttempt)), 'requests')).toEqual({
      outcome: 'fail',
      detail: 'requests: expected 2 items, got 1',
    })
  })

  it('a second cut before the changed header replays the first model', async () => {
    const fixture = await readFixture('retry-attempt')
    const { meta, events } = scanLog(fixture.log)
    const prefix = events.slice(0, 9)
    const header = foldRequestHeader(prefix)!
    const early = Session.create(SessionId(meta.id), prefix, meta)
    const request = normalizeRequests([{
      ...header.config,
      messages: snapshotJsonValue<unknown>(early.deriveMessages()) as JsonValue,
      toolHistory: snapshotJsonValue<unknown>(early.toolHistory()) as JsonValue,
      ...header.tools !== undefined ? { tools: header.tools as unknown as JsonValue } : {},
      sessionId: meta.id,
    }])
    expect(compareJson(fixture.expected[1]!, request[0]!, 'requests[1]')).toEqual({
      outcome: 'fail',
      detail: 'requests[1].model: expected "mock-b", got "mock"',
    })
  })

  it('an attempt inside a prefix must have a stream array and lossless JSON', async () => {
    const fixture = await readFixture('retry-attempt')
    const { events } = scanLog(fixture.log)
    const attempt = events[8]!
    for (const [stream, message] of [
      [{}, 'seed assistant/attempt at index 8 has invalid settlement fields'],
      [[{ dt: -0 }], 'seed event at index 8 is not losslessly JSON-serializable'],
    ] as const) {
      const mutated = replaceRow(fixture.log, 8, { ...attempt, data: { ...attempt.data, stream } })
        .toString('utf8').replace('"dt":0', '"dt":-0')
      expect(() => replayRequests(Buffer.from(mutated))).toThrow(message)
    }
  })
})
