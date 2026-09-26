/**
 * Records the harness side of a desktop trace as protocol spans: one
 * `bake.turn` per turn, parented on the `traceparent` of the user message
 * that started it, with its steps, model requests, tool calls, and approval
 * waits nested inside.
 * @module @deepseek-ai/dsh-desktop/spans
 */

import { randomBytes } from 'node:crypto'
import type { HarnessSpan } from './protocol.ts'

type Attrs = HarnessSpan['attrs']

/** Epoch microseconds with sub-millisecond resolution. */
export function nowUs(): number {
  return Math.round((performance.timeOrigin + performance.now()) * 1000)
}

const TRACEPARENT = /^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$/

/** Parses a W3C `traceparent`; malformed or all-zero values yield `undefined`. */
export function parseTraceparent(value: unknown): { traceId: string; spanId: string } | undefined {
  if (typeof value !== 'string') return undefined
  const match = TRACEPARENT.exec(value)
  if (match === null) return undefined
  const [, traceId, spanId] = match as unknown as [string, string, string]
  if (/^0+$/.test(traceId) || /^0+$/.test(spanId)) return undefined
  return { traceId, spanId }
}

/** An open span; `end` records it once. */
export interface OpenSpan {
  readonly traceId: string
  readonly spanId: string
  /** `traceparent` for messages sent inside this span. */
  readonly traceparent: string
  set(attrs: Attrs): void
  end(status?: 'ok' | 'error', attrs?: Attrs): void
}

/** Source of ids and time, injectable for tests. */
export interface SpanClock {
  now(): number
  id(bytes: 8 | 16): string
}

export const defaultClock: SpanClock = {
  now: nowUs,
  id: bytes => randomBytes(bytes).toString('hex'),
}

/**
 * Collects finished spans for batched upload. Callers keep the parent
 * handles; the recorder only buffers what ended.
 */
export class SpanRecorder {
  private buffer: HarnessSpan[] = []
  /** Spans discarded because the buffer was full. */
  dropped = 0

  constructor(
    private readonly clock: SpanClock = defaultClock,
    private readonly maxBuffered = 4096,
  ) {}

  /**
   * Starts a span now. Without a parent it starts a new trace.
   * @param parent - an open span, a `traceparent`, or nothing.
   */
  start(name: string, parent?: OpenSpan | string, attrs: Attrs = {}, lane?: string): OpenSpan {
    const context = typeof parent === 'string' ? parseTraceparent(parent) : parent
    const traceId = context?.traceId ?? this.clock.id(16)
    const spanId = this.clock.id(8)
    const startUs = this.clock.now()
    const merged: Attrs = { ...attrs }
    let ended = false
    return {
      traceId,
      spanId,
      traceparent: `00-${traceId}-${spanId}-01`,
      set: (more) => {
        Object.assign(merged, more)
      },
      end: (status = 'ok', more = {}) => {
        if (ended) return
        ended = true
        const span: HarnessSpan = {
          traceId,
          spanId,
          name,
          startUs,
          endUs: this.clock.now(),
          status,
          attrs: { ...merged, ...more },
        }
        if (context !== undefined) span.parentSpanId = context.spanId
        if (lane !== undefined) span.lane = lane
        this.push(span)
      },
    }
  }

  private push(span: HarnessSpan): void {
    if (this.buffer.length >= this.maxBuffered) {
      this.buffer.shift()
      this.dropped++
    }
    this.buffer.push(span)
  }

  /** Removes and returns every finished span. */
  drain(): HarnessSpan[] {
    const out = this.buffer
    this.buffer = []
    return out
  }
}
