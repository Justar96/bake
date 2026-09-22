/** Placement of transcript rows into display lines. */
import { describe, expect, test } from 'bun:test'
import { COLUMN, MARKER, VERB } from '../src/layout.ts'
import { compactModel, compactPath, hintFor, present, styleOf, tailLines, verbFor, type ResultBound } from '../src/present.ts'

/** Every result line drawn, for the tests that are about placement, not bounds. */
const shown: ResultBound = { lines: Number.MAX_SAFE_INTEGER, unit: 'lines', more: 'more lines' }
/** A result with its whole body, outcome line first. */
const both = (row: Parameters<typeof present>[0]) => present(row, shown)
/** A preview bound: the outcome, the head of the output, and a count for the rest. */
const live: ResultBound = { lines: 3, unit: 'lines', more: 'more lines' }
/** A collapsed bound: the outcome, the card's headline, and the size. */
const committed: ResultBound = { lines: 0, unit: 'lines', more: 'more lines' }

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

describe('compactModel', () => {
  test('drops the provider, which the user reads every frame and acts on never', () => {
    expect(compactModel('deepseek-official/deepseek-flash')).toBe('deepseek-flash')
    expect(compactModel('anthropic/claude-sonnet-4')).toBe('claude-sonnet-4')
  })

  test('leaves a bare model name alone', () => {
    expect(compactModel('deepseek-flash')).toBe('deepseek-flash')
  })
})

describe('compactPath', () => {
  test('shortens against home', () => {
    expect(compactPath('/Users/me/projects/bake', '/Users/me')).toBe('~/projects/bake')
    expect(compactPath('/Users/me', '/Users/me')).toBe('~')
  })

  test('leaves a path outside home absolute', () => {
    expect(compactPath('/etc/hosts', '/Users/me')).toBe('/etc/hosts')
  })

  test('does not shorten a sibling that merely shares the prefix', () => {
    // /Users/mestre is not inside /Users/me.
    expect(compactPath('/Users/mestre/work', '/Users/me')).toBe('/Users/mestre/work')
  })

  test('leaves the path alone when home is unknown', () => {
    expect(compactPath('/srv/app', undefined)).toBe('/srv/app')
    expect(compactPath('/srv/app', '')).toBe('/srv/app')
  })
})

describe('hintFor', () => {
  const idle = { running: false, asking: false, listing: false, drafting: false }

  test('says nothing when there is nothing to say', () => {
    expect(hintFor(idle)).toBeUndefined()
  })

  test('offers send only once there is a draft to send', () => {
    expect(hintFor({ ...idle, drafting: true })).toBe('send')
  })

  test('prefers the most immediate action when several apply', () => {
    // A pending question outranks a running turn, which outranks a draft.
    expect(hintFor({ running: true, asking: true, listing: true, drafting: true })).toBe('answer')
    expect(hintFor({ running: true, asking: false, listing: true, drafting: true })).toBe('select')
    expect(hintFor({ running: true, asking: false, listing: false, drafting: true })).toBe('interrupt')
  })
})

describe('present', () => {
  test('opens a turn with a blank and divider, then marks it once', () => {
    // The blank is the transcript's only whitespace: it gives the eye somewhere
    // to land when scrolling back, and scrollback pays for it rather than the
    // dynamic region's budget.
    const lines = present({ kind: 'user', text: 'first\nsecond' }, shown)
    expect(lines.map(line => line.marker)).toEqual([MARKER.none, MARKER.none, MARKER.turn, MARKER.none])
    expect(lines[0]!.text).toBe('')
    expect(lines[1]!.divider).toBe(true)
    expect(lines.every(line => line.column === COLUMN.rail)).toBe(true)
  })

  test('lets attachment metadata recede from the words the user said', () => {
    const lines = present({ kind: 'user', text: 'Inspect this', attachments: [{ name: 'shot.png', bytes: 12 }] }, shown)
    expect(lines.map(line => line.tone)).toEqual(['plain', 'quiet', 'said', 'quiet'])
    expect(lines.at(-1)!.text).toContain('shot.png')
  })

  test('breaks the rail for a command, and keeps it quiet', () => {
    // A command addresses the surface, not the model: the rail carries the
    // slash so the row is unmistakable without colour, and the row recedes
    // because what the user is reading for is the notice beneath it.
    const [line] = present({ kind: 'command', name: 'model', args: ' deepseek/chat high' }, shown)
    expect(line).toEqual({
      marker: MARKER.command, verb: '', text: 'model deepseek/chat high',
      column: COLUMN.rail, tone: 'quiet',
    })
    expect(MARKER.command).not.toBe(MARKER.turn)
  })

  test('places a card under the call it belongs to', () => {
    const [, ...lines] = present({
      kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a',
      detail: [{ text: 'List the directory' }],
    }, shown)
    // The head carries the verb and the title; the card continues under it,
    // dim, so every card kind lands in one column whatever tool produced it.
    expect(lines.map(line => [line.verb, line.text, line.column, line.tone]))
      .toEqual([[VERB.run, 'ls -a', COLUMN.output, 'plain'], ['', 'List the directory', COLUMN.output, 'quiet']])
  })

  test('colours the two sides of a change and keeps its context at normal brightness', () => {
    const lines = present({
      kind: 'tool-result', callId: 'c1', ok: true, text: '',
      detail: [{ text: '  keep' }, { text: '- old', emphasis: 'removed' }, { text: '+ new', emphasis: 'added' }],
    }, shown).slice(1)
    expect(lines.map(line => line.tone)).toEqual(['plain', 'removed', 'added'])
    // A diff is read as a pair, so neither side recedes into supporting detail.
    expect(styleOf('added')).toEqual({ color: 'green', dim: false, bold: false })
    expect(styleOf('removed')).toEqual({ color: 'red', dim: false, bold: false })
  })

  test('keeps raw result text when a tool declared no card', () => {
    expect(both({ kind: 'tool-result', callId: 'c1', ok: true, text: 'a\nb' }).map(line => line.text))
      .toEqual(['[c1]  2 lines', 'a', 'b'])
  })

  test('marks the answer at the prose column', () => {
    const [, line] = present({ kind: 'assistant', text: 'Two registrations.' }, shown)
    expect(line).toEqual({
      marker: MARKER.reply, verb: '', text: 'Two registrations.', column: COLUMN.rail, tone: 'plain',
    })
  })

  test('opens reasoning with a verb and indents its continuation', () => {
    const [blank, ...lines] = present({ kind: 'reasoning', text: 'first\nsecond' }, shown)
    expect(blank!.text).toBe('')
    expect(lines[0]!.verb).toBe(VERB.think)
    expect(lines[1]!.verb).toBe('')
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
    expect(lines.every(line => line.tone === 'quiet')).toBe(true)
  })

  test('heads an action with its verb and title, and no call id', () => {
    const [, line, ...rest] = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'rg -n foo' }, shown)
    expect(line).toMatchObject({ marker: MARKER.action, pulse: true, verb: VERB.run, text: 'rg -n foo' })
    expect(rest).toEqual([])
  })

  test('drops a title\'s leading verb, and names a tool no verb family covers', () => {
    const read = present({ kind: 'tool-call', callId: 'c1', tool: 'read_file', input: 'Read /x.md' }, shown)[1]!
    expect([read.verb, read.text]).toEqual([VERB.read, '/x.md'])
    const other = present({ kind: 'tool-call', callId: 'c1', tool: 'todo', input: 'three items' }, shown)[1]!
    expect([other.verb, other.text, other.spans]).toEqual([VERB.run, 'todo three items', [{ length: 4, tone: 'strong' }]])
    const streaming = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: '...' }, shown)[1]!
    expect(streaming.text).toBe('bash')
  })

  test('turns a finished action\'s verb past and its marker green, in the same block', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls', result: { ok: true, text: 'a\nb\nc' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'ls', 'a', 'b', 'c'])
    expect(lines[1]).toMatchObject({ verb: 'ran', markerTone: 'done' })
    expect(lines[1]!.pulse).toBeUndefined()
    expect(lines.filter(line => line.verb !== '')).toHaveLength(1)
  })

  test('counts what a read returned on its head line instead of previewing it', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'read', input: 'Read /x.md', result: { ok: true, text: 'one\ntwo' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', '/x.md  2 lines'])
    expect(lines[1]!.spans).toEqual([{ length: 5, tone: 'plain' }, { length: 9, tone: 'quiet' }])
  })

  test('previews a failure whatever the verb, in red', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'read', input: 'Read /x', result: { ok: false, text: 'ENOENT' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', '/x', 'ENOENT'])
    expect(lines[1]).toMatchObject({ markerTone: 'failed', verbTone: 'failed' })
    expect(lines[2]!.tone).toBe('failed')
  })

  test('leaves out a result headline that repeats the call', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'edit', input: 'Edit one.ts', result: {
      ok: true, text: '', title: 'Edit one.ts', detail: [{ text: '- a', emphasis: 'removed' }, { text: '+ b', emphasis: 'added' }],
    } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'one.ts', '- a', '+ b'])
    expect(lines[1]!.verb).toBe('edited')
  })

  test('opens each section with a blank, and keeps a result against its call', () => {
    // Indentation separates an answer at the rail from output under a verb, but
    // two actions share the verb column: a `think` directly under the previous
    // call's output would read as more of that output. The answer needs the
    // blank for the opposite reason — nothing at all separates it from the
    // reasoning above it, so without one they read as a single paragraph.
    const opens = (row: Parameters<typeof present>[0]) => present(row, shown)[0]!.text === ''
    expect(opens({ kind: 'reasoning', text: 'why' })).toBe(true)
    expect(opens({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls' })).toBe(true)
    expect(opens({ kind: 'assistant', text: 'the answer' })).toBe(true)
    // A result continues its call's section, and a notice continues the command
    // that produced it, so neither floats away from what it belongs to.
    expect(opens({ kind: 'tool-result', callId: 'c1', ok: true, text: 'out' })).toBe(false)
    expect(opens({ kind: 'notice', tone: 'info', text: 'set' })).toBe(false)
  })

  test('identifies the completed call before its aligned output', () => {
    const lines = both({ kind: 'tool-result', callId: 'c1', ok: true, text: 'a\nb' })
    expect(lines[0]!.verb).toBe(VERB.done)
    expect(lines.slice(1).every(line => line.verb === '')).toBe(true)
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
  })

  test('keeps successful and failed output at normal brightness', () => {
    const ok = present({ kind: 'tool-result', callId: 'c1', ok: true, text: 'fine' }, shown)
    const bad = present({ kind: 'tool-result', callId: 'c1', ok: false, text: 'boom' }, shown)
    expect(ok[0]!.tone).toBe('plain')
    expect(bad[0]!.tone).toBe('failed')
  })

  test('marks failed cards as errors even when their raw text is empty', () => {
    const lines = both({ kind: 'tool-result', callId: 'c2', ok: false, text: '',
      detail: [{ text: 'Permission denied' }, { text: '+ not applied', emphasis: 'added' }] })
    expect(lines[0]!.verb).toBe(VERB.error)
    expect(lines.every(line => line.tone === 'failed')).toBe(true)
    expect(lines.map(line => line.text)).toEqual(['[c2]  2 lines', 'Permission denied', '+ not applied'])
  })

  test('names a notice by its tone', () => {
    expect(present({ kind: 'notice', tone: 'info', text: 'saved' }, shown)[0]!.verb).toBe(VERB.note)
    expect(present({ kind: 'notice', tone: 'warn', text: 'slow' }, shown)[0]!.verb).toBe(VERB.note)
    const error = present({ kind: 'notice', tone: 'error', text: 'failed' }, shown)[0]!
    expect(error.verb).toBe(VERB.error)
    expect(error.tone).toBe('failed')
  })

  test('separates turn outcomes from answers without a notice verb', () => {
    const [blank, footer] = present({ kind: 'notice', placement: 'turn-end', tone: 'error', text: 'failed' }, shown)
    expect(blank?.text).toBe('')
    expect(footer).toEqual({ marker: '-', verb: '', text: 'failed', column: COLUMN.rail, tone: 'failed' })
  })

  test('emits nothing for empty text rather than occupying a row it cannot fill', () => {
    // Not even the opening blank: a blank belongs to the lines under it, and on
    // its own it is a row of the live budget spent on content never sent. An
    // empty block is the everyday case while a turn is still streaming.
    expect(present({ kind: 'assistant', text: '' }, shown)).toEqual([])
    expect(present({ kind: 'reasoning', text: '' }, shown)).toEqual([])
    // A result is the exception: its outcome is the row, so it survives an
    // empty body, with no size and no preview under it.
    expect(present({ kind: 'tool-result', callId: 'c1', ok: true, text: '' }, committed))
      .toEqual([{
        marker: MARKER.none, verb: VERB.done, verbTone: 'done', text: '[c1]', column: COLUMN.output, tone: 'plain',
        spans: [{ length: 4, tone: 'quiet' }],
      }])
    expect(present({ kind: 'tool-result', callId: 'c1', ok: true, text: '' }, shown)).toHaveLength(1)
  })

  test('drops the empty line a trailing newline would add', () => {
    expect(present({ kind: 'assistant', text: 'one\n' }, shown)).toHaveLength(2)
    expect(present({ kind: 'assistant', text: 'one\ntwo\n' }, shown)).toHaveLength(3)
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
    expect(present({ kind: 'from-the-future', text: 'x' } as never, shown)).toEqual([])
  })
})

describe('present, bounding a tool result', () => {
  const listing = (count: number) => ({
    kind: 'tool-result' as const, callId: 'c1', ok: true,
    text: Array.from({ length: count }, (_, index) => `entry-${index}`).join('\n'),
  })

  test('collapses a committed result to its outcome and its size', () => {
    // The regression this guards: one `ls -la` used to commit seventy rows, so
    // every tool call scrolled the answer off the screen. The output is in the
    // session log; what scrollback needs is that the call ran and how much it
    // returned.
    const lines = present(listing(70), committed)
    expect(lines).toHaveLength(1)
    expect(lines[0]!.text).toBe('[c1]  70 lines')
    expect(lines[0]!.verb).toBe(VERB.done)
  })

  test('keeps the card headline on a collapsed result, because it names the subject', () => {
    // `70 lines` alone does not say which file was edited, and the hunks the
    // title stood over are the part the bound removed.
    const lines = present({
      kind: 'tool-result', callId: 'c2', ok: true, text: '',
      title: 'Edit packages/ui/src/app.tsx',
      detail: [{ text: '- old', emphasis: 'removed' }, { text: '+ new', emphasis: 'added' }],
    }, committed)
    expect(lines.map(line => line.text)).toEqual(['[c2]  Edit packages/ui/src/app.tsx  2 lines'])
  })

  test('previews the head under the outcome and counts the rest', () => {
    const lines = present(listing(70), live)
    expect(lines.map(line => line.text)).toEqual(['[c1]  70 lines', 'entry-0', 'entry-1', 'entry-2', '+67 more lines'])
    // The preview continues the outcome line rather than reopening the
    // section, footer included, and the count recedes as detail.
    expect(lines.slice(1).every(line => line.verb === '')).toBe(true)
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
    expect(lines.at(-1)!.tone).toBe('quiet')
  })

  test('draws one outcome line per result', () => {
    // An outcome line on two consecutive rows reads as two calls.
    expect(both(listing(70)).filter(line => line.verb !== '')).toHaveLength(1)
  })

  test('reports nothing extra when the whole result fits the bound', () => {
    expect(present(listing(2), live).map(line => line.text)).toEqual(['[c1]  2 lines', 'entry-0', 'entry-1'])
    expect(present(listing(0), committed).map(line => line.text)).toEqual(['[c1]'])
  })

  test('bounds card lines exactly as it bounds raw text', () => {
    // A card and a raw result are the same body to a reader; a bound that
    // caught only one of them would leave `read` and `grep` unbounded.
    const lines = present({
      kind: 'tool-result', callId: 'c3', ok: true, text: '',
      detail: Array.from({ length: 9 }, (_, index) => ({ text: `  ${index}  line` })),
    }, live)
    expect(lines).toHaveLength(5)
    expect(lines.at(-1)!.text).toBe('+6 more lines')
  })

  test('keeps a bounded failure undimmed, footer included', () => {
    const lines = present({ ...listing(9), ok: false }, live)
    expect(lines.every(line => line.tone === 'failed')).toBe(true)
    expect(present({ ...listing(9), ok: false }, committed)[0]!.verb).toBe(VERB.error)
    expect(lines.at(-1)!.text).toBe('+6 more lines')
  })

  test('reports the size in the words the locale supplied', () => {
    const zh: ResultBound = { lines: 0, unit: '\u884c', more: '\u884c\u672a\u663e\u793a' }
    expect(present(listing(70), zh)[0]!.text).toBe('[c1]  70 \u884c')
    expect(present(listing(70), { ...zh, lines: 3 }).at(-1)!.text).toBe('+67 \u884c\u672a\u663e\u793a')
  })
})

describe('tailLines', () => {
  const reasoning = (count: number) =>
    present({ kind: 'reasoning', text: Array.from({ length: count }, (_, index) => `line ${index}`).join('\n') }, shown)
  const call = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls\nwc -l\nsort' })
  const answer = (count: number) =>
    present({ kind: 'assistant', text: Array.from({ length: count }, (_, index) => `answer ${index}`).join('\n') }, shown)

  test('keeps the newest lines when the turn outgrows the region, under its opening blank', () => {
    const lines = tailLines(reasoning(20), 4)
    expect(lines).toHaveLength(4)
    expect(lines[0]!.text).toBe('')
    expect(lines.at(-1)!.text).toBe('line 19')
  })

  test('carries the verb onto the first line a cut section left standing', () => {
    // Without this the window shows dim lines at the output column with
    // nothing saying whether they are reasoning or a command's output — which
    // is exactly what a long turn looks like once it passes the budget.
    const lines = tailLines(reasoning(20), 4)
    expect(lines[1]!.verb).toBe(VERB.think)
    expect(lines[1]!.text).toBe('line 17')
    expect(lines.slice(2).every(line => line.verb === '')).toBe(true)
  })

  test('shows an older section whole or not at all', () => {
    // A tool's output above a streaming answer used to lose a line per line
    // of the answer, until only its footer was left.
    const lines = [...reasoning(3), ...call]
    expect(tailLines(lines, 9)).toEqual(lines)
    for (let budget = call.length; budget < lines.length; budget++) expect(tailLines(lines, budget)).toEqual(call)
    expect(tailLines(lines, 2)[1]!.verb).toBe(VERB.run)
    expect(tailLines(lines, 2)[1]!.text).toBe('sort')
  })

  test('leaves a line that already carries a verb, and restores the reply marker', () => {
    expect(tailLines(answer(3), 3).map(line => line.text)).toEqual(['', 'answer 1', 'answer 2'])
    expect(tailLines(answer(3), 3)[1]!.marker).toBe(MARKER.reply)
    expect(tailLines(answer(3), 3).slice(2).every(line => line.marker === MARKER.none)).toBe(true)
    expect(tailLines(reasoning(3), 4)[0]!.text).toBe('')
    expect(tailLines(reasoning(3), 4)[1]!.verb).toBe(VERB.think)
    expect(tailLines(reasoning(3), 3)[1]!.verb).toBe(VERB.think)
    expect(tailLines(reasoning(3), 3)[1]!.text).toBe('line 1')
  })

  test('counts wrapped rows, filling the window with prose but keeping output lines whole', () => {
    // Every line two rows tall: whole lines alone would leave an odd window a
    // row short, and the composer would bob with each paragraph.
    const tall = () => 2
    const prose = tailLines(answer(6), 6, line => line.text === '' ? 1 : tall())
    expect(prose.map(line => line.text)).toEqual(['', 'answer 3', 'answer 4', 'answer 5'])
    const output = tailLines(reasoning(6), 6, line => line.text === '' ? 1 : tall())
    expect(output.map(line => line.text)).toEqual(['', 'line 4', 'line 5'])
    expect(output[1]!.verb).toBe(VERB.think)
  })

  test('still shows one line taller than the window', () => {
    expect(tailLines(answer(1), 3, line => line.text === '' ? 1 : 9).map(line => line.text)).toEqual(['', 'answer 0'])
    expect(tailLines(answer(1), 1, line => line.text === '' ? 1 : 9).map(line => line.text)).toEqual(['answer 0'])
  })

  test('changes nothing when every line fits', () => {
    const lines = reasoning(3)
    expect(tailLines(lines, 10)).toEqual(lines)
    expect(tailLines(lines, 0)).toEqual([])
  })
})
