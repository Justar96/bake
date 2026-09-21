/** Transcript cost as history grows: the property `<Static>` exists to provide. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { render } from 'ink'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import type { Row } from '../src/rows.ts'

/** A write-capturing stand-in for the terminal; Ink only needs these members. */
function fakeStdout(): NodeJS.WriteStream & { chunks: string[] } {
  const stream = {
    chunks: [] as string[],
    columns: 200,
    rows: 40,
    isTTY: true,
    write(chunk: string): boolean {
      stream.chunks.push(chunk)
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

function props(committed: readonly Row[], overrides: Partial<AppProps> = {}): AppProps {
  return {
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

/** Ink throttles writes, so a measurement that does not wait measures nothing. */
const flush = async (): Promise<void> => { await new Promise(resolve => { setTimeout(resolve, 80) }) }

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
  const instance = render(<App {...props(rowsUpTo(count))} />, {
    stdout, stdin: fakeStdin(), patchConsole: false, exitOnCtrlC: false,
  })
  instances.push(instance)
  await flush()
  return {
    stdout,
    async append(rows: readonly Row[]): Promise<string> {
      stdout.chunks.length = 0
      instance.rerender(<App {...props(rows)} />)
      await flush()
      return stdout.chunks.join('')
    },
  }
}

describe('transcript cost', () => {
  it('writes a committed row once and never again', async () => {
    const ui = await mounted(50)
    const written = await ui.append([...rowsUpTo(50), { kind: 'assistant', text: 'row-50-marker' }])

    // The new row is emitted...
    expect(written).toContain('row-50-marker')
    // ...and no earlier row is re-emitted with it. A transcript that repaints
    // would carry every one of these on every append, which is the quadratic
    // cost `<Static>` exists to avoid.
    expect(written).not.toContain('row-0-marker')
    expect(written).not.toContain('row-49-marker')
  })

  it('costs the same to append at 50 rows as at 2000', async () => {
    const short = await mounted(50)
    const long = await mounted(2000)

    const row = { kind: 'assistant', text: 'appended-marker' } as const
    const afterShort = await short.append([...rowsUpTo(50), row])
    const afterLong = await long.append([...rowsUpTo(2000), row])

    expect(afterShort).toContain('appended-marker')
    expect(afterLong).toContain('appended-marker')
    // History length must not enter the cost of one append. Compared as a
    // ratio so the dynamic footer, which every append redraws, does not make
    // this an exact-byte assertion.
    expect(afterLong.length).toBeLessThan(afterShort.length * 1.5)
  })
})
