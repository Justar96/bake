/** Input recall traverses authoritative presentation snapshots only when requested. */
import { expect, it } from 'bun:test'
import { inputHistory } from '../src/history.ts'
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
  // the transcript renders in the rail rather than losing it.
  const transcript = appendTranscript(emptyTranscript, [
    { kind: 'command', name: 'model', args: ' deepseek/chat high' },
    { kind: 'command', name: 'help', args: '' },
  ])
  expect([...inputHistory(transcript, [])]).toEqual(['/help', '/model deepseek/chat high'])
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
