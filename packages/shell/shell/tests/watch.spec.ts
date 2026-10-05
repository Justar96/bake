/** Live-output polling shared by the foreground shell executors. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { SubprocessOutputReader } from 'bake-subprocess'
import { LIVE_OUTPUT_MAX_CHARS, LIVE_OUTPUT_POLL_MS, watchOutput } from 'bake-shell'

afterEach(() => { vi.useRealTimers() })

/** An in-memory collect-mode reader whose text the test appends to. */
function reader(): SubprocessOutputReader & { write(text: string): void } {
  let text = ''
  return {
    write(more: string) { text += more },
    readFrom(fromByte: number) {
      return { text: text.slice(fromByte), nextOffset: text.length, lossy: false }
    },
  }
}

describe('watchOutput', () => {
  it('passes the combined tail each poll the streams grew, stdout before stderr', () => {
    vi.useFakeTimers()
    const stdout = reader()
    const stderr = reader()
    const tails: string[] = []
    const stop = watchOutput({ stdout, stderr }, (tail) => { tails.push(tail) })
    stdout.write('a\n')
    stderr.write('e\n')
    vi.advanceTimersByTime(LIVE_OUTPUT_POLL_MS)
    vi.advanceTimersByTime(LIVE_OUTPUT_POLL_MS)
    stdout.write('b\n')
    vi.advanceTimersByTime(LIVE_OUTPUT_POLL_MS)
    expect(tails).toEqual(['a\ne\n', 'a\ne\nb\n'])
    stop()
    stdout.write('c\n')
    vi.advanceTimersByTime(LIVE_OUTPUT_POLL_MS * 3)
    expect(tails).toHaveLength(2)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('bounds the tail and never polls without a receiver', () => {
    vi.useFakeTimers()
    const stdout = reader()
    const stderr = reader()
    expect(watchOutput({ stdout, stderr }, undefined)).toBeTypeOf('function')
    expect(vi.getTimerCount()).toBe(0)
    const tails: string[] = []
    const stop = watchOutput({ stdout, stderr }, (tail) => { tails.push(tail) })
    stdout.write(`head${'x'.repeat(LIVE_OUTPUT_MAX_CHARS)}`)
    vi.advanceTimersByTime(LIVE_OUTPUT_POLL_MS)
    expect(tails).toEqual(['x'.repeat(LIVE_OUTPUT_MAX_CHARS)])
    stop()
  })
})
