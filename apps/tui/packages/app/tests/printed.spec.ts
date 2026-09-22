/** Streamed text printed ahead of its commit, folded from the rows it is handed. */
import { describe, expect, it as test } from 'vitest'
import type { Row } from '@dsh-tui/ui'
import { Printed } from '../src/printed.ts'
import type { KeyedRow } from '../src/live.ts'

const answer = (text: string, key = 1): KeyedRow => ({ key, row: { kind: 'assistant', text } })
const reasoning = (text: string, key = 0): KeyedRow => ({ key, row: { kind: 'reasoning', text } })
const call: KeyedRow = { key: 2, row: { kind: 'tool-call', callId: 'c1', tool: 'bash', input: '...' } }

/** Feed an answer growing one chunk at a time, collecting everything printed. */
function stream(printed: Printed, chunks: readonly string[]): { printed: Row[], live: readonly Row[] } {
  const all: Row[] = []
  let text = ''
  let live: readonly Row[] = []
  for (const chunk of chunks) {
    text += chunk
    const split = printed.split([answer(text)])
    all.push(...split.print)
    live = split.live
  }
  return { printed: all, live }
}

describe('printing a streaming answer', () => {
  test('prints each line once it ends and keeps the line still arriving live', () => {
    const printed = new Printed()
    const { printed: rows, live } = stream(printed, ['First li', 'ne.\nSecond ', 'line.\nThi'])
    expect(rows).toEqual([
      { kind: 'assistant', text: 'First line.' },
      { kind: 'assistant', text: 'Second line.', continued: true },
    ])
    expect(live).toEqual([{ kind: 'assistant', text: 'Thi', continued: true }])
  })

  test('keeps a paragraph break with the paragraph it opens', () => {
    const { printed: rows, live } = stream(new Printed(), ['One.\n', '\n', 'Two.\n'])
    expect(rows).toEqual([
      { kind: 'assistant', text: 'One.' },
      { kind: 'assistant', text: '\nTwo.', continued: true },
    ])
    // Still a row while empty, so the header keeps reading `writing`.
    expect(live).toEqual([{ kind: 'assistant', text: '', continued: true }])
  })

  test('draws nothing ahead of the first finished line', () => {
    const printed = new Printed()
    expect(printed.split([answer('Hel')])).toEqual({ print: [], live: [{ kind: 'assistant', text: 'Hel' }] })
    expect(printed.any).toBe(false)
  })

  test('prints reasoning whole once the answer starts, and never before', () => {
    const printed = new Printed()
    expect(printed.split([reasoning('Think.\nMore.\n')]).print).toEqual([])
    const { print, live } = printed.split([reasoning('Think.\nMore.\n'), answer('Ans')])
    expect(print).toEqual([{ kind: 'reasoning', text: 'Think.\nMore.' }])
    expect(live).toEqual([{ kind: 'assistant', text: 'Ans' }])
  })

  test('prints nothing past a tool call, which commits through its own event', () => {
    const printed = new Printed()
    const { print, live } = printed.split([answer('Checking.\n'), call, answer('After.\n', 3)])
    expect(print).toEqual([{ kind: 'assistant', text: 'Checking.' }])
    expect(live.map(row => row.kind)).toEqual(['tool-call', 'assistant'])
  })
})

describe('reconciling the commit', () => {
  test('adds only what did not print', () => {
    const printed = new Printed()
    stream(printed, ['One.\nTwo.\nThr'])
    expect(printed.reconcile([{ kind: 'assistant', text: 'One.\nTwo.\nThree.' }]))
      .toEqual([{ kind: 'assistant', text: 'Three.', continued: true }])
  })

  test('adds nothing when everything printed, even without the final newline', () => {
    const printed = new Printed()
    stream(printed, ['Done.\n'])
    expect(printed.reconcile([{ kind: 'assistant', text: 'Done.' }])).toEqual([])
    expect(printed.reconcile([{ kind: 'assistant', text: 'Done.\n' }])).toEqual([])
  })

  test('drops printed reasoning and keeps the order of the rest', () => {
    const printed = new Printed()
    printed.split([reasoning('Why.'), answer('Because.\nSo')])
    expect(printed.reconcile([{ kind: 'reasoning', text: 'Why.' }, { kind: 'assistant', text: 'Because.\nSo.' }]))
      .toEqual([{ kind: 'assistant', text: 'So.', continued: true }])
  })

  test('prints a block again whole when the commit changed it', () => {
    const printed = new Printed()
    stream(printed, ['Draft.\n'])
    const committed: Row = { kind: 'assistant', text: 'Final.' }
    expect(printed.reconcile([committed])).toEqual([committed])
  })

  test('passes a commit through untouched when nothing printed', () => {
    const rows: Row[] = [{ kind: 'reasoning', text: 'a' }, { kind: 'assistant', text: 'b' }]
    expect(new Printed().reconcile(rows)).toEqual(rows)
  })
})
