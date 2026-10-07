/**
 * The tool-call-turn request-reconstruction fixture: composes the real loop
 * around the scripted model, replays requests from a Session log, maps
 * generated message ids to stable placeholders, and compares requests as
 * exact JSON values. Committed fixture files are read by the spec and never
 * written here.
 */

import { Context } from '@deepseek-ai/cordis'
import LlmRuntime, { createUserMessage } from 'bake-llm'
import type { GenerateOptions, StreamChunk } from 'bake-llm'
import SessionStore, { Session, SessionId, foldRequestHeader } from 'bake-session'
import type { SessionEvent, SessionHeader } from 'bake-session'
import { eventLines, scanLog, toHeaderLine } from 'bake-session-persistence-jsonl/src/format.ts'
import SessionProjectionRegistry from 'bake-session-projection'
import SystemPrompt from 'bake-system-prompt'
import ToolRuntime, { defineContentToolFixture } from 'bake-tools'
import AgentRegistry, { type Agent } from 'bake-agent'
import { snapshotJsonValue, type JsonValue } from 'bake-util-values'
import AgentLoop from 'bake-agent-loop'
import { MockAdapter, textResponse, toolCallResponse } from './mock-adapter.ts'

/** Header of `expected-requests.json`; any other schema or version is refused. */
export const EXPECTED_REQUESTS_SCHEMA = 'bake/runtime-conformance/requests'
export const EXPECTED_REQUESTS_VERSION = 1

/** The first turn of the original THEOREM: `echo {text:'one'}` with text `calling`, then `done`. */
export function toolCallTurnScript(args: object = { text: 'one' }): StreamChunk[][] {
  return [toolCallResponse('c1', 'echo', args, 'calling'), textResponse('done')]
}

/** What one run exposes: every dispatched request plus the Session it was dispatched from. */
export interface ScenarioCapture {
  readonly requests: readonly GenerateOptions[]
  readonly header: SessionHeader
  readonly events: readonly SessionEvent[]
}

/**
 * Run the user message `go` through a private composed runtime until the agent
 * is idle again, then dispose the Context.
 *
 * Composition matches the original THEOREM harness except
 * `includeHarnessIdentity: false`, so every prompt byte comes from the
 * scenario. Disposal is awaited on every exit, including a failed plugin
 * load, a rejected step, or `signal` aborting the idle wait.
 * @param script - scripted model responses, consumed one per request.
 * @param signal - aborts the idle wait, typically the test's own signal.
 * @returns the dispatched requests and a detached Session snapshot taken before disposal.
 */
export async function runToolCallTurn(script: StreamChunk[][], signal: AbortSignal): Promise<ScenarioCapture> {
  const ctx = new Context()
  const adapter = new MockAdapter(script)
  let stopWaiting: (() => void) | undefined
  let capture: ScenarioCapture
  try {
    await ctx.plugin(LlmRuntime)
    await ctx.plugin(SessionStore)
    await ctx.plugin(SessionProjectionRegistry)
    await ctx.plugin(SystemPrompt, { personaPrefix: 'stable base', includeHarnessIdentity: false })
    await ctx.plugin(ToolRuntime)
    await ctx.plugin(AgentRegistry)
    await ctx.plugin(AgentLoop, { agents: [] })
    ctx.llm.registerAdapter(['mock'], adapter)
    ctx.tools.register(defineContentToolFixture({
      name: 'echo',
      description: 'echo back',
      parameters: { text: { type: 'string' } },
      async execute(args) {
        return [{ type: 'text', text: `echo: ${String(args.text)}` }]
      },
    }))
    const agent = await ctx.agentLoop.create(SessionId('a1'), { provider: 'mock', model: 'mock' })
    // Subscribe before the follow-up so a fast turn cannot finish unobserved.
    const idle = waitForIdle(ctx, agent, signal)
    stopWaiting = idle.stop
    agent.followup(createUserMessage({ content: [{ type: 'text', text: 'go' }], source: { kind: 'user' } }))
    await idle.done
    capture = {
      requests: [...adapter.requests],
      header: structuredClone(agent.session.header),
      events: structuredClone(agent.session.snapshotEvents()),
    }
  } finally {
    stopWaiting?.()
    await ctx.fiber.dispose()
  }
  if (adapter.requests.length !== capture.requests.length) throw new Error('a model request was dispatched during disposal')
  return capture
}

function waitForIdle(ctx: Context, agent: Agent, signal: AbortSignal): { done: Promise<void>; stop: () => void } {
  let stop = (): void => {}
  const done = new Promise<void>((resolve, reject) => {
    const onAbort = (): void => {
      stop()
      reject(signal.reason instanceof Error ? signal.reason : new Error('scenario aborted before idle'))
    }
    const off = ctx.on('agent/status', ({ agent: subject, status }) => {
      if (subject === agent && status === 'idle') {
        stop()
        resolve()
      }
    })
    stop = () => {
      off()
      signal.removeEventListener('abort', onAbort)
    }
    if (signal.aborted) onAbort()
    else signal.addEventListener('abort', onAbort, { once: true })
  })
  return { done, stop }
}

/** Every data member of `GenerateOptions` the projection carries, in declaration order. */
const REQUEST_FIELDS: ReadonlySet<string> = new Set([
  'provider', 'model', 'reasoningEffort', 'messages', 'system', 'tools', 'toolHistory',
  'toolUpdates', 'temperature', 'maxTokens', 'stop', 'sessionId', 'purpose',
])

/**
 * Project one dispatched request onto lossless JSON. Only `signal` is
 * excluded, as a non-data `AbortSignal`; an `undefined` member is omitted as
 * JSON omits it. Any other own key, including a new `GenerateOptions` member,
 * throws rather than disappearing from the comparison.
 * @param request - one request the adapter received.
 * @returns the request as a detached JSON object.
 * @throws when a member is unknown or not losslessly JSON-serializable.
 */
export function projectRequest(request: GenerateOptions): { [key: string]: JsonValue } {
  const projected: { [key: string]: JsonValue } = {}
  for (const key of Reflect.ownKeys(request)) {
    if (typeof key !== 'string') throw new Error(`request has a symbol member ${String(key)}`)
    const value: unknown = (request as unknown as Record<string, unknown>)[key]
    if (key === 'signal') {
      if (value !== undefined && !(value instanceof AbortSignal)) throw new Error('request signal is not an AbortSignal')
      continue
    }
    if (!REQUEST_FIELDS.has(key)) throw new Error(`request member "${key}" is not in the fixture projection`)
    if (value === undefined) continue
    const snapshot = snapshotJsonValue(value)
    if (snapshot === undefined) throw new Error(`request member "${key}" is not lossless JSON`)
    projected[key] = snapshot as JsonValue
  }
  return projected
}

const GENERATED_MESSAGE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

/**
 * One bijection from generated message UUIDs to `message-N`, numbered by first
 * use. Use one instance per request sequence, so the same message keeps one
 * placeholder across every request that carries it.
 */
export class MessageIdentities {
  private readonly placeholders = new Map<string, string>()

  /**
   * @param raw - the value at a documented message-identity location.
   * @param where - that location, for the failure message.
   * @returns the stable placeholder for `raw`.
   * @throws when `raw` is not a `randomUUID()` v4 string.
   */
  placeholder(raw: unknown, where: string): string {
    if (typeof raw !== 'string' || !GENERATED_MESSAGE_ID.test(raw)) {
      throw new Error(`${where}: expected a generated message id, got ${JSON.stringify(raw)}`)
    }
    let placeholder = this.placeholders.get(raw)
    if (placeholder === undefined) {
      placeholder = `message-${this.placeholders.size + 1}`
      this.placeholders.set(raw, placeholder)
    }
    return placeholder
  }
}

/**
 * Replace each request's `messages[i].id`, the only generated value a request
 * of this scenario carries, numbering in dispatch order as the expected file
 * is written. Text, arguments, and every other member stay exact.
 * @param requests - projected requests in dispatch order.
 * @returns normalized copies.
 */
export function normalizeRequests(requests: readonly { [key: string]: JsonValue }[]): { [key: string]: JsonValue }[] {
  const ids = new MessageIdentities()
  return requests.map((request, index) => {
    const copy = structuredClone(request)
    const messages = copy.messages
    if (!Array.isArray(messages)) throw new Error(`requests[${index}].messages is not an array`)
    messages.forEach((message, position) => {
      if (message === null || typeof message !== 'object' || Array.isArray(message)) {
        throw new Error(`requests[${index}].messages[${position}] is not an object`)
      }
      message.id = ids.placeholder(message.id, `requests[${index}].messages[${position}].id`)
    })
    return copy
  })
}

/**
 * Encode an unseeded Session exactly as the JSONL writer stores a current generation.
 * @param header - Session header.
 * @param events - Session events in log order.
 * @returns the complete log text, newline-terminated.
 */
export function encodeLog(header: SessionHeader, events: readonly SessionEvent[]): string {
  return `${JSON.stringify(toHeaderLine(header))}\n${eventLines(events)}\n`
}

/**
 * Rebuild each step's request from committed log bytes alone: the messages a
 * fresh Session derives from the prefix ending before that step's Assistant
 * settlement, and the request header folded over the same prefix. This
 * mirrors how the loop assembles a request; the expected file, not this
 * function, is the independent specification.
 * This fixture assumes one settlement per step; retries need per-attempt cutoffs.
 * @param log - a complete current-format log.
 * @returns one projected request per `step/start`, in log order.
 * @throws when the log is torn, seeded, or a step lacks a later settlement or header.
 */
export function replayRequests(log: Buffer): { [key: string]: JsonValue }[] {
  const { meta, events, inheritedEventCount, committedBytes } = scanLog(log)
  if (committedBytes !== log.length) throw new Error(`log has ${log.length - committedBytes} uncommitted trailing bytes`)
  if (meta.isSeeded || inheritedEventCount !== 0) throw new Error('replay expects an unseeded log')
  events.forEach((event, index) => {
    if (event.seq !== index) throw new Error(`events[${index}] has seq ${event.seq}`)
  })
  return events.filter(event => event.type === 'step/start').map((start) => {
    const settlement = events.find(event =>
      (event.type === 'assistant/message' || event.type === 'assistant/attempt')
      && event.data.turn === start.data.turn
      && event.data.step === start.data.step)
    if (settlement === undefined || settlement.seq < start.seq) {
      throw new Error(`step ${start.data.turn}.${start.data.step} has no later Assistant settlement`)
    }
    // The cut excludes the settlement: a request never contains its own response.
    const prefix = events.slice(0, settlement.seq)
    const session = Session.create(SessionId(meta.id), prefix, meta)
    const header = foldRequestHeader(prefix)
    if (header === undefined) throw new Error(`step ${start.data.turn}.${start.data.step} has no request header`)
    const request = snapshotJsonValue<unknown>({
      ...header.config,
      messages: session.deriveMessages(),
      toolHistory: session.toolHistory(),
      ...header.tools !== undefined ? { tools: header.tools } : {},
      sessionId: meta.id,
    })
    if (request === undefined) throw new Error('replayed request is not lossless JSON')
    return request as { [key: string]: JsonValue }
  })
}

/** One comparison outcome; `detail` names the first difference. */
export type Comparison = { outcome: 'pass' } | { outcome: 'fail'; detail: string }

function kind(value: JsonValue): string {
  return value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value
}

function firstDifference(expected: JsonValue, actual: JsonValue, path: string): string | undefined {
  if (kind(expected) !== kind(actual)) return `${path}: expected ${kind(expected)}, got ${kind(actual)}`
  if (Array.isArray(expected) && Array.isArray(actual)) {
    if (expected.length !== actual.length) return `${path}: expected ${expected.length} items, got ${actual.length}`
    for (let i = 0; i < expected.length; i++) {
      const difference = firstDifference(expected[i] as JsonValue, actual[i] as JsonValue, `${path}[${i}]`)
      if (difference !== undefined) return difference
    }
    return undefined
  }
  if (expected !== null && typeof expected === 'object' && actual !== null && typeof actual === 'object' && !Array.isArray(expected) && !Array.isArray(actual)) {
    for (const key of Object.keys(expected)) {
      if (!Object.hasOwn(actual, key)) return `${path}.${key}: missing`
    }
    for (const key of Object.keys(actual)) {
      if (!Object.hasOwn(expected, key)) return `${path}.${key}: unexpected member`
    }
    for (const key of Object.keys(expected)) {
      const difference = firstDifference(expected[key] as JsonValue, actual[key] as JsonValue, `${path}.${key}`)
      if (difference !== undefined) return difference
    }
    return undefined
  }
  return expected === actual ? undefined : `${path}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
}

/**
 * Compare JSON values exactly: every member and array position is significant,
 * `null`, `[]`, and an omitted member differ, and object key order is immaterial.
 * @param expected - the specification.
 * @param actual - the observation.
 * @param path - the root label used in `detail`.
 * @returns pass, or the first difference.
 */
export function compareJson(expected: JsonValue, actual: JsonValue, path = '$'): Comparison {
  const difference = firstDifference(expected, actual, path)
  return difference === undefined ? { outcome: 'pass' } : { outcome: 'fail', detail: difference }
}

/**
 * Compare two texts as exact UTF-8 bytes.
 * @param expected - the committed bytes.
 * @param actual - the observed bytes.
 * @returns pass, or the first differing byte offset and line.
 */
export function compareBytes(expected: Buffer, actual: Buffer): Comparison {
  const length = Math.min(expected.length, actual.length)
  let offset = 0
  while (offset < length && expected[offset] === actual[offset]) offset++
  if (offset === length && expected.length === actual.length) return { outcome: 'pass' }
  const line = expected.subarray(0, offset).toString('utf8').split('\n').length
  return { outcome: 'fail', detail: `first difference at byte ${offset} (line ${line}); lengths ${expected.length} and ${actual.length}` }
}

/**
 * Parse `expected-requests.json` and check its header.
 * @param bytes - the committed file.
 * @returns its ordered requests.
 * @throws when the schema, version, or request list is wrong.
 */
export function parseExpectedRequests(bytes: Buffer): JsonValue[] {
  const document = JSON.parse(bytes.toString('utf8')) as { schema?: unknown; version?: unknown; requests?: unknown }
  if (document.schema !== EXPECTED_REQUESTS_SCHEMA || document.version !== EXPECTED_REQUESTS_VERSION) {
    throw new Error(`expected requests must be ${EXPECTED_REQUESTS_SCHEMA} version ${EXPECTED_REQUESTS_VERSION}`)
  }
  if (!Array.isArray(document.requests) || Object.keys(document).length !== 3) {
    throw new Error('expected requests document must hold exactly schema, version, and a requests array')
  }
  return document.requests as JsonValue[]
}
