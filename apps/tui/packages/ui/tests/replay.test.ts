/** Replay preserves presentation order while bounding each admitted batch. */
import { expect, it } from 'bun:test'
import { budgetFor } from '../src/layout.ts'
import { present, type PresentedLine, type ResultBound } from '../src/present.ts'
import { ReplayCursor, type ReplayBatch } from '../src/replay.ts'
import type { Row } from '../src/rows.ts'
import { appendTranscript, emptyTranscript, type Transcript } from '../src/transcript.ts'

const budget = budgetFor({ columns: 80, rows: 24 })
const result: ResultBound = { lines: 3, unit: 'lines', more: 'more lines' }

function drain(cursor: ReplayCursor, transcript: Transcript): ReplayBatch[] {
  const batches: ReplayBatch[] = []
  for (let batch = cursor.next(transcript, budget, result); batch !== undefined; batch = cursor.next(transcript, budget, result)) {
    batches.push(batch)
  }
  return batches
}

it('preserves presentation and cumulative positions across transcript batches and invisible rows', () => {
  const rows: Row[] = [
    { kind: 'user', text: 'hello' },
    { kind: 'assistant', text: '**answer**\n\nsecond paragraph' },
    { kind: 'notice', tone: 'info', placement: 'turn-end', text: 'Completed' },
    { kind: 'assistant', text: 'continued', continued: true },
    { kind: 'notice', tone: 'error', text: 'failed' },
  ]
  const first = appendTranscript(emptyTranscript, rows.slice(0, 2))
  const transcript = appendTranscript(first, rows.slice(2))
  const cursor = new ReplayCursor()
  expect(cursor.next(emptyTranscript, budget, result)).toBeUndefined()
  expect(cursor.caughtUp).toBe(true)
  const batches = drain(cursor, transcript)
  expect(batches.flatMap(batch => batch.lines)).toEqual(rows.flatMap(row => present(row, result)))
  let count = 0
  for (const batch of batches) {
    expect(batch.start).toBe(count)
    count += batch.lines.length
  }
  expect(cursor.next(transcript, budget, result)).toBeUndefined()
  const invisible = appendTranscript(transcript, [{ kind: 'notice', tone: 'info', placement: 'turn-end', text: 'Completed' }])
  expect(cursor.next(invisible, budget, result)).toBeUndefined()
  const appended = appendTranscript(invisible, [{ kind: 'rate', text: '42 tokens/s' }])
  expect(cursor.next(appended, budget, result)).toMatchObject({ start: count, height: 1, lines: [{ text: '42 tokens/s' }] })
})

it('finishes a captured snapshot before scanning appends and never rereads old rows', () => {
  let reads = 0
  let tailReads = 0
  let transcript = emptyTranscript
  const expected: string[] = []
  for (let index = 0; index < 1500; index++) {
    const text = `row-${index}`
    expected.push(text)
    const rows = new Proxy<Row[]>([{ kind: 'rate', text }], {
      get(target, property, receiver) {
        if (property === '0') reads++
        return Reflect.get(target, property, receiver)
      },
    })
    transcript = appendTranscript(transcript, rows)
  }
  const cursor = new ReplayCursor()
  const first = cursor.next(transcript, budget, result)!
  expect(first.lines).toHaveLength(512)
  expect(cursor.caughtUp).toBe(false)
  const tail = new Proxy(appendTranscript(transcript, [{ kind: 'rate', text: 'appended' }]), {
    get(target, property, receiver) {
      if (property === 'previous') tailReads++
      return Reflect.get(target, property, receiver)
    },
  })
  const second = cursor.next(tail, budget, result)!
  expect(second.start).toBe(512)
  expect(second.lines).toHaveLength(512)
  expect(tailReads).toBe(0)
  const batches = [first, second, ...drain(cursor, tail)]
  expect(batches.flatMap(batch => batch.lines.map(line => line.text))).toEqual([...expected, 'appended'])
  expect(reads).toBe(1500)
  expect(tailReads).toBe(1)
  expect(cursor.caughtUp).toBe(true)
  expect(cursor.next(tail, budget, result)).toBeUndefined()
  expect(reads).toBe(1500)
})

it('replays a thousand-line answer in bounded batches and presents its code once', () => {
  const code = Array.from({ length: 1000 }, (_, index) => `line-${index}`)
  const row: Row = { kind: 'assistant', text: ['```text', ...code, '```'].join('\n') }
  let highlights = 0
  const highlighted: ResultBound = { ...result, code: (lines) => {
    highlights++
    return lines.map(line => [{ length: line.length, color: '#ffffff' }])
  } }
  const transcript = appendTranscript(emptyTranscript, [row])
  const cursor = new ReplayCursor()
  const lines: PresentedLine[] = []
  const appended = appendTranscript(transcript, [{ kind: 'rate', text: 'after the answer' }])
  for (let batch = cursor.next(transcript, budget, highlighted); batch !== undefined; batch = cursor.next(appended, budget, highlighted)) {
    expect(batch.start).toBe(lines.length)
    expect(batch.lines.length).toBeGreaterThan(0)
    expect(batch.lines.length).toBeLessThanOrEqual(512)
    expect(batch.height).toBeLessThanOrEqual(1024)
    lines.push(...batch.lines)
  }
  expect(lines.filter(line => line.literal).map(line => line.text)).toEqual(code.map(line => `  ${line}`))
  expect(lines.at(-1)!.text).toBe('after the answer')
  expect(highlights).toBe(1)
})

it('bounds wrapped height and allows one oversized line without dropping its neighbours', () => {
  const row: Row = { kind: 'notice', placement: 'command', tone: 'info', text: ['short', 'x'.repeat(1500), 'last'].join('\n') }
  const transcript = appendTranscript(emptyTranscript, [row])
  const cursor = new ReplayCursor()
  const narrow = budgetFor({ columns: 3, rows: 24 })
  const first = cursor.next(transcript, narrow, result)!
  expect(first.lines.map(line => line.text)).toEqual(['short'])
  expect(first.height).toBe(5)
  const oversized = cursor.next(transcript, narrow, result)!
  expect(oversized.lines.map(line => line.text)).toEqual(['x'.repeat(1500)])
  expect(oversized.height).toBe(1500)
  const last = cursor.next(transcript, narrow, result)!
  expect(last.lines.map(line => line.text)).toEqual(['last'])
  expect(last.start).toBe(2)
  expect(cursor.next(transcript, narrow, result)).toBeUndefined()
})

it('limits text per batch independently of wrapped height', () => {
  const texts = ['a'.repeat(80_000), 'b'.repeat(80_000), 'c'.repeat(160_000), 'tail']
  const transcript = appendTranscript(emptyTranscript, [{ kind: 'notice', placement: 'command', tone: 'info', text: texts.join('\n') }])
  const cursor = new ReplayCursor()
  const wide = budgetFor({ columns: 500_000, rows: 24 })
  const batches: ReplayBatch[] = []
  for (let batch = cursor.next(transcript, wide, result); batch !== undefined; batch = cursor.next(transcript, wide, result)) {
    batches.push(batch)
  }
  expect(batches.map(batch => batch.lines.map(line => line.text))).toEqual(texts.map(text => [text]))
  expect(batches.every(batch => batch.height === 1)).toBe(true)
})
