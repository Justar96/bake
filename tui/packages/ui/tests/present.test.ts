/** Placement of transcript rows into display lines. */
import { describe, expect, test } from 'bun:test'
import { COLUMN, MARKER, VERB } from '../src/layout.ts'
import { present, styleOf, verbFor } from '../src/present.ts'

describe('verbFor', () => {
  test('names the action, not the implementation', () => {
    // bash, shell and zsh are one kind of event to a reader.
    expect(verbFor('bash')).toBe(VERB.run)
    expect(verbFor('Shell')).toBe(VERB.run)
    expect(verbFor('exec_command')).toBe(VERB.run)
  })

  test('maps the common tool families', () => {
    expect(verbFor('read_file')).toBe(VERB.read)
    expect(verbFor('str_replace_editor')).toBe(VERB.edit)
    expect(verbFor('write_file')).toBe(VERB.edit)
    expect(verbFor('grep')).toBe(VERB.find)
    expect(verbFor('web_fetch')).toBe(VERB.fetch)
  })

  test('falls back to a verb rather than showing an unknown tool name raw', () => {
    expect(verbFor('some_new_tool')).toBe(VERB.run)
  })
})

describe('present', () => {
  test('marks the user row once, not on every wrapped line', () => {
    const lines = present({ kind: 'user', text: 'first\nsecond' })
    expect(lines.map(line => line.marker)).toEqual([MARKER.prompt, MARKER.none])
    expect(lines.every(line => line.column === COLUMN.rail)).toBe(true)
  })

  test('leaves the answer unmarked, at the prose column', () => {
    const [line] = present({ kind: 'assistant', text: 'Two registrations.' })
    expect(line).toEqual({
      marker: MARKER.none, verb: '', text: 'Two registrations.', column: COLUMN.rail, tone: 'plain',
    })
  })

  test('opens reasoning with a verb and indents its continuation', () => {
    const lines = present({ kind: 'reasoning', text: 'first\nsecond' })
    expect(lines[0]!.verb).toBe(VERB.think)
    expect(lines[1]!.verb).toBe('')
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
    expect(lines.every(line => line.tone === 'quiet')).toBe(true)
  })

  test('presents a call as its verb and argument', () => {
    const [line] = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'rg -n foo' })
    expect(line?.verb).toBe(VERB.run)
    expect(line?.text).toBe('rg -n foo')
  })

  test('aligns output under the call, carrying no verb of its own', () => {
    const lines = present({ kind: 'tool-result', callId: 'c1', ok: true, text: 'a\nb' })
    expect(lines.every(line => line.verb === '')).toBe(true)
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
  })

  test('does not dim failed output, which is the thing the user needs', () => {
    const ok = present({ kind: 'tool-result', callId: 'c1', ok: true, text: 'fine' })
    const bad = present({ kind: 'tool-result', callId: 'c1', ok: false, text: 'boom' })
    expect(ok[0]!.tone).toBe('quiet')
    expect(bad[0]!.tone).toBe('failed')
  })

  test('names a notice by its tone', () => {
    expect(present({ kind: 'notice', tone: 'info', text: 'saved' })[0]!.verb).toBe(VERB.note)
    expect(present({ kind: 'notice', tone: 'warn', text: 'slow' })[0]!.verb).toBe(VERB.note)
    const error = present({ kind: 'notice', tone: 'error', text: 'failed' })[0]!
    expect(error.verb).toBe(VERB.error)
    expect(error.tone).toBe('failed')
  })

  test('emits nothing for empty text rather than occupying a row it cannot fill', () => {
    expect(present({ kind: 'assistant', text: '' })).toEqual([])
    expect(present({ kind: 'tool-result', callId: 'c1', ok: true, text: '' })).toEqual([])
  })

  test('drops the empty line a trailing newline would add', () => {
    expect(present({ kind: 'assistant', text: 'one\n' })).toHaveLength(1)
    expect(present({ kind: 'assistant', text: 'one\ntwo\n' })).toHaveLength(2)
  })

  test('never dims a failure, and keeps colour semantic', () => {
    expect(styleOf('failed')).toEqual({ color: 'red', dim: false, bold: false })
    expect(styleOf('asking')).toEqual({ color: 'cyan', dim: false, bold: false })
    expect(styleOf('quiet')).toEqual({ dim: true, bold: false })
    expect(styleOf('said')).toEqual({ dim: false, bold: true })
    expect(styleOf('plain')).toEqual({ dim: false, bold: false })
    expect(styleOf('unknown' as never)).toEqual({ dim: false, bold: false })
  })

  test('renders nothing for a row kind this build does not know', () => {
    // The session log may carry events newer than this surface.
    expect(present({ kind: 'from-the-future', text: 'x' } as never)).toEqual([])
  })
})
