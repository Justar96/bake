/** Transcript cost as history grows: the property `<Static>` exists to provide. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { render } from 'ink'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { appendTranscript, emptyTranscript, type Transcript } from '../src/transcript.ts'
import type { Row } from '../src/rows.ts'

/** A write-capturing stand-in for the terminal; Ink only needs these members. */
function fakeStdout(): NodeJS.WriteStream & { chunks: string[] } {
  const stream = {
    chunks: [] as string[],
    columns: 200,
    rows: 40,
    isTTY: true,
    write(chunk: string, callback?: () => void): boolean {
      stream.chunks.push(chunk)
      callback?.()
      return true
    },
    on: () => stream,
    off: () => stream,
    once: () => stream,
    removeListener: () => stream,
  }
  return stream as unknown as NodeJS.WriteStream & { chunks: string[] }
}

/**
 * A stdin Ink can put into raw mode. Without one, `useInput` calls
 * `setRawMode` on the real non-TTY stdin, throws inside an effect, and the
 * component stops re-rendering after its first frame — which reads exactly
 * like a transcript that never repaints.
 */
function fakeStdin(): NodeJS.ReadStream {
  const stream = {
    isTTY: true,
    setRawMode: () => stream,
    setEncoding: () => stream,
    resume: () => stream,
    pause: () => stream,
    ref: () => stream,
    unref: () => stream,
    on: () => stream,
    off: () => stream,
    once: () => stream,
    addListener: () => stream,
    removeListener: () => stream,
    read: () => null,
  }
  return stream as unknown as NodeJS.ReadStream
}

function props(committed: Transcript, overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8,
    committed,
    live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-scale', copy: dictionaries.en, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

/** One assistant row per index; the index is the marker each assertion looks for. */
const rowsUpTo = (count: number): Row[] =>
  Array.from({ length: count }, (_, index) => ({ kind: 'assistant', text: `row-${index}-marker` }))

const instances: { unmount: () => void }[] = []
afterEach(() => {
  for (const instance of instances.splice(0)) instance.unmount()
})

/** Render a transcript of `count` rows and settle its first frame. */
async function mounted(count: number): Promise<{
  stdout: NodeJS.WriteStream & { chunks: string[] }
  append: (rows: readonly Row[]) => Promise<string>
}> {
  const stdout = fakeStdout()
  let transcript = appendTranscript(emptyTranscript, rowsUpTo(count))
  const instance = render(<App {...props(transcript)} />, {
    stdout, stdin: fakeStdin(), patchConsole: false, exitOnCtrlC: false,
  })
  instances.push(instance)
  await instance.waitUntilRenderFlush()
  return {
    stdout,
    async append(rows: readonly Row[]): Promise<string> {
      stdout.chunks.length = 0
      transcript = appendTranscript(transcript, rows)
      instance.rerender(<App {...props(transcript)} />)
      await instance.waitUntilRenderFlush()
      return stdout.chunks.join('')
    },
  }
}

describe('transcript cost', () => {
  it.each([50, 10_000])('does not read %i committed rows while streaming', async count => {
    let reads = 0
    const history = new Proxy(rowsUpTo(count), {
      get(target, property, receiver) {
        if (typeof property === 'string' && /^\d+$/.test(property)) reads++
        return Reflect.get(target, property, receiver)
      },
    })
    const committed = appendTranscript(emptyTranscript, history)
    const stdout = fakeStdout()
    const instance = render(<App {...props(committed)} />, {
      stdout, stdin: fakeStdin(), patchConsole: false, exitOnCtrlC: false,
    })
    instances.push(instance)
    await instance.waitUntilRenderFlush()
    reads = 0
    instance.rerender(<App {...props(committed, { live: [{ kind: 'assistant', text: 'stream-marker' }] })} />)
    await instance.waitUntilRenderFlush()
    expect(stdout.chunks.join('')).toContain('stream-marker')
    expect(reads).toBe(0)
    instance.rerender(<App {...props(appendTranscript(committed, [{ kind: 'assistant', text: 'append-marker' }]))} />)
    await instance.waitUntilRenderFlush()
    expect(stdout.chunks.join('')).toContain('append-marker')
    expect(reads).toBe(0)
  })

  it('writes a committed row once and never again', async () => {
    const ui = await mounted(50)
    const written = await ui.append([{ kind: 'assistant', text: 'row-50-marker' }])

    expect(written).toContain('row-50-marker')
    expect(written).not.toContain('row-0-marker')
    expect(written).not.toContain('row-49-marker')
  })

  it('prints every committed row once across coalesced and flushed appends', async () => {
    const stdout = fakeStdout()
    let transcript = emptyTranscript
    const instance = render(<App {...props(transcript)} />, {
      stdout, stdin: fakeStdin(), patchConsole: false, exitOnCtrlC: false,
    })
    instances.push(instance)
    await instance.waitUntilRenderFlush()
    const expected: string[] = []
    for (let burst = 0; burst < 4; burst++) {
      for (let item = 0; item < 8; item++) {
        const text = `burst-${burst}-${item}-marker`
        expected.push(text)
        transcript = appendTranscript(transcript, [{ kind: 'assistant', text }])
        instance.rerender(<App {...props(transcript)} />)
      }
      await instance.waitUntilRenderFlush()
    }
    const printed = stdout.chunks.join('').match(/burst-\d+-\d+-marker/g)
    expect(printed).toEqual(expected)
  })

  it('emits bounded terminal output when appending at 50 and 2000 rows', async () => {
    const short = await mounted(50)
    const long = await mounted(2000)

    const row = { kind: 'assistant', text: 'appended-marker' } as const
    const afterShort = await short.append([row])
    const afterLong = await long.append([row])

    expect(afterShort).toContain('appended-marker')
    expect(afterLong).toContain('appended-marker')
    expect(afterLong.length).toBeLessThan(afterShort.length * 1.5)
  })
})
