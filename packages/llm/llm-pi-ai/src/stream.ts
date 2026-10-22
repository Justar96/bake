/**
 * pi-ai assistant event translation into the Harness streaming protocol.
 *
 * pi-ai tool-call arguments are parsed objects while the Harness keeps their
 * raw JSON representation. pi-ai also reports failures as terminal stream
 * events, which this module maps into Harness finish chunks.
 *
 * @module bake-llm-pi-ai/stream
 */

import { brandString } from 'bake-brand'
import { CONTEXT_WINDOW_EXCEEDED_CODE, EMPTY_RESPONSE_CODE, isContextWindowExceededError, isQuotaExceededError, LlmError, QUOTA_EXCEEDED_CODE } from 'bake-llm'
import type { FinishReason, StreamChunk, TokenUsage, ToolCallId } from 'bake-llm'
import { isContextOverflow } from '@earendil-works/pi-ai/utils/overflow'
import type { AssistantMessage, AssistantMessageEvent, Usage as PiUsage } from '@earendil-works/pi-ai'
import { DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE } from './config.ts'
import { toPiReplayState } from './replay.ts'

/**
 * Map pi-ai usage (reasoning folded into output by pi-ai).
 * @param usage - cumulative usage from the terminal pi-ai event.
 * @returns harness counts with pi-ai's exact total; cache fields appear only
 *   when non-zero (pi-ai reports zeros, not absence).
 */
export function mapUsage(usage: PiUsage): TokenUsage {
  return {
    inputTokens: usage.input,
    outputTokens: usage.output,
    totalTokens: usage.totalTokens,
    ...usage.cacheRead > 0 ? { cacheReadTokens: usage.cacheRead } : {},
    ...usage.cacheWrite > 0 ? { cacheWriteTokens: usage.cacheWrite } : {},
  }
}

/**
 * The usage chunk for a terminal pi-ai message, or none when the provider
 * reported no accounting.
 *
 * pi-ai starts every message at zero and fills in what the endpoint sends, so
 * an all-zero count means the endpoint sent nothing; some proxies stream
 * without usage. Every request carries at least its prompt, so zero is never
 * a real reading, and recording it would read as an empty context rather than
 * an unknown one.
 *
 * @param usage - cumulative usage from the terminal pi-ai event.
 * @returns the chunks to yield before `finish`: one `usage`, or none.
 */
function usageChunks(usage: PiUsage): StreamChunk[] {
  const reported = usage.input + usage.output + usage.cacheRead + usage.cacheWrite + usage.totalTokens > 0
  return reported ? [{ type: 'usage', usage: mapUsage(usage) }] : []
}

// XXX(pi-ai upstream): pi-ai flattens the caught error to `error.message`
// (api/anthropic-messages.js: `errorMessage = error instanceof Error ?
// error.message : JSON.stringify(error)`), discarding the original Error and its
// `cause` chain before it reaches us. undici carries the actionable transport
// detail on `cause` (e.g. `SocketError: other side closed`) but hands the fetch
// wrapper a bare `terminated`, so we are left pattern-matching terse words here.
// If pi-ai ever forwards the original Error (or a fetch/dispatcher hook that lets
// us capture the cause ourselves), classify on `code`/`cause` instead of text.
function classifyPiAiError(message: string): string {
  if (/\b(?:401|403)\b/.test(message)) return 'AUTH'
  if (isQuotaExceededError(message)) return QUOTA_EXCEEDED_CODE
  if (/\b429\b|rate.?limit/i.test(message)) return 'RATE_LIMIT'
  // A rejected request body (gateway or provider size cap): resending the
  // same request cannot succeed, so it is invalid, not transient.
  if (/\b413\b|failed to buffer the request body:\s*length limit exceeded|payload too large|request body too large/i.test(message)) return 'INVALID_REQUEST'
  if (/\b400\b|invalid.?request/i.test(message)) return 'INVALID_REQUEST'
  if (/\b5\d\d\b/.test(message)) return 'SERVER'
  if (/\btime(?:d)?\s*out\b|timeout/i.test(message)) return 'TIMEOUT'
  // A stream truncated before the provider's terminal event: each pi-ai provider
  // throws its own wording when the wire closes mid-response without a terminal
  // event (`… stream ended before message_stop`, `… before a terminal response
  // event`, `… ended without a terminal event`, `Stream ended without
  // finish_reason`). The connection dropped mid-response, so this is a transport
  // truncation, not a model-level error.
  if (/stream ended (?:before|without)\b/i.test(message)) return 'TRANSPORT'
  if (/\b(?:network|connection|socket|fetch)\b|\bECONN[A-Z]+\b/i.test(message)
    || /\b(?:other side closed|HTTP2 request did not get a response|WebSocket closed unexpectedly)\b/i.test(message)
    // undici renders a mid-stream socket drop as a bare `terminated` (its
    // `cause` — the real SocketError — was flattened away upstream); Node's
    // stream layer says `Premature close`.
    || /\bterminated\b|premature close/i.test(message)) {
    return 'TRANSPORT'
  }
  return 'PI_AI_ERROR'
}

/** Longest credential cooldown a gateway hint may request, in seconds. */
const MAX_GATEWAY_RESET_SECONDS = 3600

/**
 * Read the credential-cooldown hint a multi-credential gateway puts in its
 * error body. CLIProxyAPI and CliRelay answer `429 model_cooldown` and
 * `503 model_unavailable` with `"reset_seconds": N` once every credential for
 * the model is cooling down; pi-ai keeps the body in the error message but
 * drops the response headers, so the body is the only place the wait survives.
 * Without it the retry policy spends its attempts before the cooldown ends.
 * @param message - pi-ai's flattened error text, status and body included.
 * @returns the advertised wait in milliseconds, or `undefined` when absent or implausible.
 */
function gatewayResetMs(message: string): number | undefined {
  const match = /"reset_seconds"\s*:\s*(\d+(?:\.\d+)?)/.exec(message)
  if (match === null) return undefined
  const seconds = Number(match[1])
  if (!Number.isFinite(seconds) || seconds <= 0 || seconds > MAX_GATEWAY_RESET_SECONDS) return undefined
  return Math.ceil(seconds * 1000)
}

/**
 * Map a terminal pi-ai event to the harness finish reason.
 * @param message - the assistant message carried by the `done` or `error` event.
 * @param contextWindow - resolved catalog capacity for usage-based overflow detection.
 * @returns the mapped harness reason. Recognized error text, `stop` usage above
 *   `contextWindow`, and zero-output `length` usage that fills the window map
 *   to `CONTEXT_WINDOW_EXCEEDED`; a `stop` with no content blocks maps to an
 *   `EMPTY_RESPONSE` error, while terminal `pending` and `deferred` states map
 *   to non-retryable `PI_AI_ERROR` failures.
 */
export function mapStopReason(message: AssistantMessage, contextWindow?: number): FinishReason {
  const piAiOverflow = isContextOverflow(message, contextWindow)
  const harnessOverflow = message.stopReason === 'error'
    && message.errorMessage !== undefined
    && isContextWindowExceededError(message.errorMessage)
  if (piAiOverflow || harnessOverflow) {
    return {
      kind: 'error',
      failure: {
        message: message.errorMessage ?? `pi-ai detected context overflow for model "${message.model}"`,
        code: CONTEXT_WINDOW_EXCEEDED_CODE,
      },
    }
  }

  switch (message.stopReason) {
    case 'stop':
      // A terminal stop that produced no content blocks is a degenerate
      // provider completion, not a successful (empty) assistant message.
      if (message.content.length === 0) {
        return {
          kind: 'error',
          failure: {
            message: `model "${message.model}" returned a completed response with no content`,
            code: EMPTY_RESPONSE_CODE,
          },
        }
      }
      return { kind: 'stop' }
    case 'length': return { kind: 'max-tokens' }
    case 'toolUse': return { kind: 'tool-calls' }
    case 'pending': return {
      kind: 'error',
      failure: { message: `pi-ai stream for model "${message.model}" ended pending`, code: 'PI_AI_ERROR' },
    }
    case 'deferred': return {
      kind: 'error',
      failure: { message: `pi-ai deferred response for model "${message.model}" is not supported`, code: 'PI_AI_ERROR' },
    }
    case 'aborted': return {
      kind: 'aborted',
      failure: { message: message.errorMessage ?? 'pi-ai stream aborted', code: 'ABORTED' },
    }
    case 'error': {
      const text = message.errorMessage ?? 'pi-ai stream error'
      const code = classifyPiAiError(text)
      const retryAfterMs = code === 'RATE_LIMIT' || code === 'SERVER' ? gatewayResetMs(text) : undefined
      return {
        kind: 'error',
        failure: { message: text, code, ...retryAfterMs === undefined ? {} : { providerRetryAfterMs: retryAfterMs } },
      }
    }
  }
}

/**
 * Scan state for one streaming tool call's argument text, kept so each delta
 * is read once: whether the scan is inside a JSON string (and just after a
 * backslash there), and how many whitespace characters outside any string
 * ended the text so far.
 */
interface ArgumentScan {
  inString: boolean
  escaped: boolean
  trailingWhitespace: number
}

/**
 * Advance one tool call's argument scan over the next delta. Only JSON
 * whitespace outside strings counts: whitespace inside a string value is
 * content the model may legitimately write, such as a file's indentation,
 * while a runaway pads between tokens. Any other character outside a string
 * resets the run.
 * @param scan - the call's state, updated in place.
 * @param delta - the next raw argument text.
 */
function scanArguments(scan: ArgumentScan, delta: string): void {
  for (let position = 0; position < delta.length; position++) {
    const code = delta.charCodeAt(position)
    if (scan.inString) {
      if (scan.escaped) scan.escaped = false
      else if (code === 0x5C) scan.escaped = true
      else if (code === 0x22) scan.inString = false
      continue
    }
    if (code === 0x20 || code === 0x0A || code === 0x0D || code === 0x09) {
      scan.trailingWhitespace++
      continue
    }
    scan.trailingWhitespace = 0
    if (code === 0x22) scan.inString = true
  }
}

/**
 * Translate the pi-ai event stream into StreamChunks. pi-ai never throws
 * mid-stream — failures arrive as `error` events, which become error/aborted
 * `finish` chunks (the harness protocol's other error-delivery style).
 * @param events - one assistant turn's pi-ai event stream.
 * @param contextWindow - resolved catalog capacity for usage-based overflow detection.
 * @param callerSignal - caller cancellation state; an aborted caller makes any
 *   in-band terminal error an aborted finish.
 * @param requestedModel - request model identity recorded for durable replay.
 * @param maxToolArgumentWhitespace - longest run of whitespace outside JSON
 *   strings a tool call's streamed arguments may end with. Some models finish
 *   the arguments' last value, then stream whitespace for minutes without
 *   closing the object; the run is the only sign, because every padding delta
 *   resets the idle watchdog.
 * @returns the harness chunks, ending with `usage`, when the provider
 *   reported any, then `finish`; throws
 *   `LlmError` (`STREAM_CLOSED`) if the source ends without a terminal event,
 *   and `LlmError` (`TRANSPORT`) once a tool call's trailing whitespace
 *   exceeds `maxToolArgumentWhitespace`. Throwing ends the iteration of
 *   `events`; the caller's teardown aborts the request.
 */
export async function* toStreamChunks(
  events: AsyncIterable<AssistantMessageEvent>,
  contextWindow?: number,
  callerSignal?: AbortSignal,
  requestedModel?: string,
  maxToolArgumentWhitespace: number = DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE,
): AsyncGenerator<StreamChunk> {
  // pi-ai contentIndex ↔ our block index map 1:1 (both count blocks from 0
  // in stream order), but we track ids per index for tool calls.
  const toolIds = new Map<number, { id: string; name: string }>()
  const scans = new Map<number, ArgumentScan>()

  for await (const event of events) {
    switch (event.type) {
      case 'start':
        break
      case 'text_start':
        yield { type: 'block-start', index: event.contentIndex, blockType: 'text' }
        break
      case 'text_delta':
        yield { type: 'text-delta', index: event.contentIndex, text: event.delta }
        break
      case 'text_end':
        yield { type: 'block-end', index: event.contentIndex, block: { type: 'text', text: event.content } }
        break
      case 'thinking_start':
        yield { type: 'block-start', index: event.contentIndex, blockType: 'reasoning' }
        break
      case 'thinking_delta':
        yield { type: 'reasoning-delta', index: event.contentIndex, text: event.delta }
        break
      case 'thinking_end':
        yield { type: 'block-end', index: event.contentIndex, block: { type: 'reasoning', text: event.content } }
        break
      case 'toolcall_start': {
        // The id/name live on the partial's content at this index.
        const partial = event.partial.content[event.contentIndex]
        const id = partial?.type === 'toolCall' ? partial.id : ''
        const name = partial?.type === 'toolCall' ? partial.name : ''
        toolIds.set(event.contentIndex, { id, name })
        yield { type: 'block-start', index: event.contentIndex, blockType: 'tool-call' }
        break
      }
      case 'toolcall_delta': {
        const known = toolIds.get(event.contentIndex)
        let scan = scans.get(event.contentIndex)
        if (scan === undefined) {
          scan = { inString: false, escaped: false, trailingWhitespace: 0 }
          scans.set(event.contentIndex, scan)
        }
        scanArguments(scan, event.delta)
        if (scan.trailingWhitespace > maxToolArgumentWhitespace) {
          // A broken response stream, not a refusal of the request: the same
          // request usually completes, so it takes the transient code.
          throw new LlmError(
            `model "${requestedModel ?? event.partial.model}" streamed ${scan.trailingWhitespace} whitespace characters`
            + ` after the arguments of tool call "${known?.name ?? ''}" without closing them`,
            'TRANSPORT',
          )
        }
        yield {
          type: 'tool-call-delta',
          index: event.contentIndex,
          id: brandString<ToolCallId>(known?.id ?? ''),
          ...known?.name !== undefined && known.name.length > 0 ? { name: known.name } : {},
          argumentsDelta: event.delta,
        }
        break
      }
      case 'toolcall_end':
        yield {
          type: 'block-end',
          index: event.contentIndex,
          block: {
            type: 'tool-call',
            id: brandString<ToolCallId>(event.toolCall.id),
            name: event.toolCall.name,
            // pi-ai hands back the PARSED arguments; the harness vocabulary
            // keeps the raw string.
            arguments: JSON.stringify(event.toolCall.arguments),
          },
        }
        break
      case 'done':
        yield* usageChunks(event.message.usage)
        yield {
          type: 'finish',
          reason: mapStopReason(event.message, contextWindow),
          replayState: toPiReplayState(event.message, requestedModel),
        }
        return
      case 'error':
        // In-stream error delivery (pi-ai's style) → error finish chunk
        // (the harness's other sanctioned error path besides throwing).
        yield* usageChunks(event.error.usage)
        yield {
          type: 'finish',
          reason: mapStopReason(
            callerSignal?.aborted ? { ...event.error, stopReason: 'aborted' } : event.error,
            contextWindow,
          ),
        }
        return
      // no default: AssistantMessageEvent is pi-ai's closed union; a new
      // event type should fail compilation here via tsc's exhaustiveness
      // when one is added (switch covers all current variants).
    }
  }
  throw new LlmError('pi-ai event stream ended without done/error', 'STREAM_CLOSED')
}
