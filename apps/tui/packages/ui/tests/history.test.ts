/** Input recall traverses authoritative presentation snapshots only when requested. */
import { expect, it } from 'bun:test'
import { inputHistory, recallCursor } from '../src/history.ts'
import { appendTranscript, emptyTranscript, type Transcript } from '../src/transcript.ts'

it('recalls pending human input and committed user rows, newest first', () => {
  const transcript = appendTranscript(emptyTranscript, [
    { kind: 'user', text: 'First' }, { kind: 'assistant', text: 'Do not recall' },
    { kind: 'notice', tone: 'info', text: 'Secret prompt metadata' }, { kind: 'user', text: 'Review this' },
  ])
  expect([...inputHistory(transcript, [{ text: 'Queued' }])]).toEqual(['Queued', 'Review this', 'First'])
})

it('rebuilds a recalled command into the draft the user typed', () => {
  // The row keeps the name and arguments apart, so recall restores the slash
  // the transcript renders in the rail instead of losing it.
  const transcript = appendTranscript(emptyTranscript, [
    { kind: 'command', name: 'model', args: ' deepseek/chat high' },
    { kind: 'command', name: 'help', args: '' },
  ])
  expect([...inputHistory(transcript, [])]).toEqual(['/help', '/model deepseek/chat high'])
})

it('recalls local redacted lines but skips redacted entries without one', () => {
  const transcript = appendTranscript(emptyTranscript, [
    { kind: 'command', name: 'model', args: '', inputOmitted: true },
    { kind: 'command', name: 'attach', args: '', inputOmitted: true, recall: '/attach notes with spaces.bin' },
    { kind: 'command', name: 'login', args: '', inputOmitted: true },
    { kind: 'command', name: 'agents', args: '', inputOmitted: true, recall: '/agents' },
  ])
  expect([...inputHistory(transcript, [])]).toEqual(['/agents', '/attach notes with spaces.bin'])
})

it('does not read earlier batches to recall the latest prompt', () => {
  const earlier: Transcript = { length: 10_000, get rows() { throw new Error('Earlier history read') } }
  const latest = appendTranscript(earlier, [{ kind: 'user', text: 'Latest' }])
  expect(inputHistory(latest, []).next().value).toBe('Latest')
})

it('skips attachment-only input without recalling generated file metadata', () => {
  const history = appendTranscript(emptyTranscript, [
    { kind: 'user', text: 'Inspect', attachments: [{ name: 'private.bin', bytes: 4 }] },
    { kind: 'user', text: '', attachments: [{ name: 'private.bin', bytes: 4 }] },
  ])
  expect([...inputHistory(history, [{ text: '' }])]).toEqual(['Inspect'])
})

it('recalls a multi-line prompt whole, as one entry', () => {
  const history = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'First line\nsecond line' }])
  expect([...inputHistory(history, [])]).toEqual(['First line\nsecond line'])
})

it('opens an entry of several rows on the row the next press leaves from', () => {
  const entry = { text: 'one\ntwo', cursor: 5 }
  // The next Up keeps walking back rather than climbing the entry row by row.
  expect(recallCursor(entry, 'older', 2)).toBe(0)
  // The next Down keeps walking forward from its last row.
  expect(recallCursor(entry, 'newer', 2)).toBe(7)
  // One row is both first and last, so its caret stays: at the end where it was loaded, or where a visit left it.
  expect(recallCursor({ text: 'single', cursor: 6 }, 'older', 1)).toBe(6)
  expect(recallCursor({ text: 'single', cursor: 2 }, 'newer', 1)).toBe(2)
})
