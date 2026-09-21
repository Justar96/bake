/** Transcript appends preserve old snapshots without reading their rows. */
import { expect, it } from 'bun:test'
import { appendTranscript, emptyTranscript, transcriptRows } from '../src/transcript.ts'
import type { Row } from '../src/rows.ts'

it('keeps snapshots stable and reads suffixes across batch boundaries', () => {
  const first = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'hello' }])
  const second = appendTranscript(first, [{ kind: 'reasoning', text: 'thinking' }, { kind: 'assistant', text: 'answer' }])
  expect(appendTranscript(second, [])).toBe(second)
  expect(transcriptRows(first)).toEqual([{ kind: 'user', text: 'hello' }])
  expect(transcriptRows(second, 1)).toEqual([{ kind: 'reasoning', text: 'thinking' }, { kind: 'assistant', text: 'answer' }])
  expect(transcriptRows(second, 2)).toEqual([{ kind: 'assistant', text: 'answer' }])
  expect(transcriptRows(second, 3)).toEqual([])
})

it('assembles a 10000-row replay with one row read per item and no prefix copies', () => {
  let reads = 0
  let transcript = emptyTranscript
  for (let index = 0; index < 10_000; index++) {
    const rows = new Proxy<Row[]>([{ kind: 'assistant', text: `row-${index}` }], {
      get(target, property, receiver) {
        if (property === '0') reads++
        return Reflect.get(target, property, receiver)
      },
    })
    transcript = appendTranscript(transcript, rows)
  }
  expect(reads).toBe(0)
  expect(transcriptRows(transcript).map(row => row.kind)).toHaveLength(10_000)
  expect(reads).toBe(10_000)
  expect(transcriptRows(transcript, 9999)).toEqual([{ kind: 'assistant', text: 'row-9999' }])
})
