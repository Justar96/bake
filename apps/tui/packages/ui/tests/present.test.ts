/** Placement of transcript rows into display lines. */
import { describe, expect, test } from 'bun:test'
import { COLUMN, MARKER, TREE, VERB } from '../src/layout.ts'
import type { CardLine, ToolCallRow } from '../src/rows.ts'
import { PALETTE } from '../src/palette.ts'
import wrapAnsi from 'wrap-ansi'
import { compactModel, compactPath, CONNECTOR, fittedGroup, hintFor, isBlank, present, softBreaks, styleOf, tailLines, toolLabel, verbFor, type Highlight, type PresentedLine, type ResultBound } from '../src/present.ts'

/** Every result line drawn, for the tests that are about placement, not bounds. */
const shown: ResultBound = { lines: Number.MAX_SAFE_INTEGER, unit: 'lines', more: 'more lines' }
/** A result with its whole body, outcome line first. */
const both = (row: Parameters<typeof present>[0]) => present(row, shown)
/** A preview bound. The outcome, the head of the output, and a count for the rest. */
const live: ResultBound = { lines: 3, unit: 'lines', more: 'more lines' }
/** A collapsed bound. The outcome, the card's headline, and the size. */
const committed: ResultBound = { lines: 0, unit: 'lines', more: 'more lines' }

describe('toolLabel', () => {
  test('capitalizes each word of a tool name and runs them together', () => {
    expect(['bash', 'read_file', 'str_replace_editor', 'web-fetch', 'Grep', '__'].map(toolLabel))
      .toEqual(['Bash', 'ReadFile', 'StrReplaceEditor', 'WebFetch', 'Grep', '__'])
  })
})

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
    // A plan the agent rewrites is not an edit to the workspace.
    expect(verbFor('todo_write')).toBe(VERB.plan)
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
    // The blank is the transcript's only whitespace. Scrollback pays for it,
    // not the dynamic region's budget.
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

  test('draws a command as typed, from the rail, behind a blank', () => {
    // A command addresses the surface, not the model. Its slash takes the
    // first column, so the row is unmistakable without colour, and the name
    // is bold as an action's verb is.
    const [blank, line] = present({ kind: 'command', name: 'model', args: ' deepseek/chat high' }, shown)
    expect(blank!.text).toBe('')
    expect(line).toEqual({
      marker: MARKER.none, verb: '', text: '/model deepseek/chat high', column: COLUMN.rail, flush: true,
      tone: 'plain', spans: [{ length: '/model'.length, tone: 'said' }],
    })
  })

  test('hangs a command\'s outcome from it on a branch, red when it failed', () => {
    const lines = present({ kind: 'notice', placement: 'command', tone: 'info', text: 'Model set\nfor the next turn' }, shown)
    expect(lines.map(line => [line.marker, line.text, line.column, line.tone])).toEqual([
      [TREE.corner, 'Model set', COLUMN.rail, 'plain'], [MARKER.none, 'for the next turn', COLUMN.rail, 'plain'],
    ])
    expect(lines[0]!.markerTone).toBe('quiet')
    // An outcome is often a table, such as `/help` prints: it wraps at the full width, as output does.
    expect(lines.every(line => line.wide === true)).toBe(true)
    const [failed] = present({ kind: 'notice', placement: 'command', tone: 'error', text: 'Unknown command: /foo' }, shown)
    expect([failed!.marker, failed!.markerTone, failed!.tone]).toEqual([TREE.corner, 'failed', 'failed'])
    // A notice no command produced keeps its verb.
    expect(present({ kind: 'notice', tone: 'info', text: 'Compacting' }, shown)[0]!.verb).toBe(VERB.note)
  })

  test('places a card under the call it belongs to', () => {
    const [, ...lines] = present({
      kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a',
      detail: [{ text: 'List the directory' }],
    }, shown)
    // The head names the tool and its argument; the card hangs under it on
    // the connector, dim, so every card kind lands in one column whatever
    // tool produced it.
    expect(lines.map(line => [line.verb, line.text, line.column, line.tone]))
      .toEqual([['', 'Bash(ls -a)', COLUMN.rail, 'plain'], [CONNECTOR, 'List the directory', COLUMN.output, 'quiet']])
  })

  test('colours the two sides of a change and keeps its context at normal brightness', () => {
    const lines = present({
      kind: 'tool-result', callId: 'c1', ok: true, text: '',
      detail: [{ text: '  keep' }, { text: '- old', emphasis: 'removed' }, { text: '+ new', emphasis: 'added' }],
    }, shown).slice(1)
    expect(lines.map(line => line.tone)).toEqual(['plain', 'removed', 'added'])
    // A diff is a pair, so neither side recedes into supporting detail.
    expect(styleOf('added')).toEqual({ color: PALETTE.done, dim: false, bold: false })
    expect(styleOf('removed')).toEqual({ color: PALETTE.failed, dim: false, bold: false })
  })

  test('keeps raw result text when a tool declared no card', () => {
    expect(both({ kind: 'tool-result', callId: 'c1', ok: true, text: 'a\nb' }).map(line => line.text))
      .toEqual(['[c1]  2 lines', 'a', 'b'])
  })

  test('draws an answer\'s rate dim at the rail, continuing the answer rather than opening a section', () => {
    expect(present({ kind: 'rate', text: '120 tokens \u00b7 40.0 tok/s' }, shown)).toEqual([
      { marker: MARKER.none, verb: '', text: '120 tokens \u00b7 40.0 tok/s', column: COLUMN.rail, tone: 'quiet' },
    ])
  })

  test('draws the answer at the prose column, with no marker', () => {
    const [, line] = present({ kind: 'assistant', text: 'Two registrations.' }, shown)
    expect(line).toEqual({
      marker: MARKER.none, verb: '', text: 'Two registrations.', column: COLUMN.rail, tone: 'plain',
    })
  })

  test('draws reasoning as a paragraph at the rail, with no verb and no marker', () => {
    const [blank, ...lines] = present({ kind: 'reasoning', text: 'first\nsecond' }, shown)
    expect(blank!.text).toBe('')
    expect(lines.map(line => [line.marker, line.verb])).toEqual([[MARKER.none, ''], [MARKER.none, '']])
    expect(lines.every(line => line.column === COLUMN.rail && line.prose === true)).toBe(true)
    expect(lines.every(line => line.tone === 'thought')).toBe(true)
  })

  test('heads an action with its tool and argument, and no call id', () => {
    const [, line, ...rest] = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'rg -n foo' }, shown)
    expect(line).toMatchObject({ marker: MARKER.action, pulse: true, verb: '', text: 'Bash(rg -n foo)',
      spans: [{ length: 4, tone: 'strong' }, { length: 11, tone: 'plain' }] })
    expect(rest).toEqual([])
  })

  test('drops a title\'s leading verb, and names every tool by itself', () => {
    const read = present({ kind: 'tool-call', callId: 'c1', tool: 'read_file', input: 'Read /x.md' }, shown)[1]!
    expect(read.text).toBe('ReadFile(/x.md)')
    const other = present({ kind: 'tool-call', callId: 'c1', tool: 'deploy', input: 'three items' }, shown)[1]!
    expect([other.text, other.spans]).toEqual(['Deploy(three items)', [{ length: 6, tone: 'strong' }, { length: 13, tone: 'plain' }]])
    const bare = present({ kind: 'tool-call', callId: 'c1', tool: 'status', input: '' }, shown)[1]!
    expect([bare.text, bare.spans]).toEqual(['Status', [{ length: 6, tone: 'strong' }]])
    const streaming = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: '...' }, shown)[1]!
    expect(streaming.text).toBe('Bash(...)')
  })

  test('turns a finished action\'s marker green, and hangs its output from the head', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls', result: { ok: true, text: 'a\nb\nc' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'Bash(ls)', 'a', 'b', 'c'])
    expect(lines[1]).toMatchObject({ verb: '', markerTone: 'done' })
    expect(lines[1]!.pulse).toBeUndefined()
    // Only the first output line carries the connector.
    expect(lines.map(line => line.verb)).toEqual(['', '', CONNECTOR, '', ''])
    expect(lines[2]!.verbTone).toBe('quiet')
  })

  test('previews file reads within the configured result bound', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'read', input: 'Read /x.md', result: { ok: true, text: 'one\ntwo' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'Read(/x.md)', 'one', 'two'])
  })

  test('previews a failure whatever the verb, in red', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'read', input: 'Read /x', result: { ok: false, text: 'ENOENT' } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'Read(/x)', 'ENOENT'])
    expect(lines[1]).toMatchObject({ markerTone: 'failed' })
    expect(lines[2]!.tone).toBe('failed')
  })

  test('leaves out a result headline that repeats the call', () => {
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'edit', input: 'Edit one.ts', result: {
      ok: true, text: '', title: 'Edit one.ts', detail: [{ text: '- a', emphasis: 'removed' }, { text: '+ b', emphasis: 'added' }],
    } }, live)
    expect(lines.map(line => line.text)).toEqual(['', 'Edit(one.ts)  +1 −1', '- a', '+ b'])
  })

  test('opens each section with a blank, and keeps a result against its call', () => {
    // Indentation separates an answer at the rail from output under a verb, but
    // two actions share the verb column. A `think` directly under the previous
    // call's output would look like more of that output. The answer needs the
    // blank for the opposite reason. Nothing at all separates it from the
    // reasoning above it, so without one they look like a single paragraph.
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
    // Not even the opening blank. A blank belongs to the lines under it, and on
    // its own it is a row of the live budget spent on content never sent. An
    // empty block is the everyday case while a turn is still streaming.
    expect(present({ kind: 'assistant', text: '' }, shown)).toEqual([])
    expect(present({ kind: 'reasoning', text: '' }, shown)).toEqual([])
    // A result is the exception. Its outcome is the row, so it survives an
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
    expect(styleOf('failed')).toEqual({ color: PALETTE.failed, dim: false, bold: false })
    expect(styleOf('asking')).toEqual({ color: PALETTE.asking, dim: false, bold: false })
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
    // The regression this guards. One `ls -la` used to commit seventy rows, so
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
    // The preview continues the outcome line instead of reopening the
    // section, footer included, and the count is detail.
    expect(lines.slice(1).every(line => line.verb === '')).toBe(true)
    expect(lines.every(line => line.column === COLUMN.output)).toBe(true)
    expect(lines.at(-1)!.tone).toBe('quiet')
  })

  test('draws one outcome line per result', () => {
    // An outcome line on two consecutive rows would look like two calls.
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

describe('present, reporting an action', () => {
  const call = (tool: string, input: string, result?: { ok: boolean, text?: string, detail?: readonly CardLine[] }) => present({
    kind: 'tool-call', callId: 'c1', tool, input,
    ...result === undefined ? {} : { result: { ok: result.ok, text: result.text ?? '', ...result.detail === undefined ? {} : { detail: result.detail } } },
  }, live).slice(1)
  const text = (lines: readonly PresentedLine[]) => lines.map(line => line.text)

  test('drops a leading word naming the verb, but never a command\'s own', () => {
    expect(call('grep', 'Grep TODO in src')[0]!.text).toBe('Grep(TODO in src)')
    expect(call('write_file', 'Write a.ts')[0]!.text).toBe('WriteFile(a.ts)')
    expect(call('bash', 'grep -rn TODO src')[0]!.text).toBe('Bash(grep -rn TODO src)')
    expect(call('bash', 'exec ./deploy.sh')[0]!.text).toBe('Bash(exec ./deploy.sh)')
  })

  test('puts the card\'s summary on the head, where no bound hides it', () => {
    // The exit status is the last line of a command's output, which is the
    // part a preview cuts. On the head it stays visible after the preview is cut.
    const run = call('bash', 'bun test', { ok: true, detail: [{ text: 'boom' }, { text: 'exit 1', summary: 'failure' }] })
    expect(text(run)).toEqual(['Bash(bun test)  exit 1', 'boom'])
    expect(run[0]!.spans?.at(-1)).toEqual({ length: 'exit 1'.length + 2, tone: 'failed' })
    // A card's own count says more than how many lines it drew.
    const found = call('grep', 'Grep TODO', { ok: true, detail: [{ text: 'a.ts' }, { text: '  7  hit' }, { text: '1 match', summary: 'count' }] })
    expect(text(found)).toEqual(['Grep(TODO)  1 match', 'a.ts', '  7  hit'])
    expect(found[0]!.spans?.at(-1)?.tone).toBe('quiet')
  })

  test('shows the first lines of a command\'s output and its last, without the blanks at either end', () => {
    const output = ['', ...Array.from({ length: 9 }, (_, index) => `line ${index}`), '', ''].join('\n')
    expect(text(call('bash', 'make', { ok: true, text: output }))).toEqual(['Bash(make)', 'line 0', 'line 1', '+6 more lines', 'line 8'])
    // A count beside a blank would separate nothing, so the blank joins it.
    const padded = ['a', '', 'c', 'd', 'e', 'f', '', 'z'].join('\n')
    expect(text(call('bash', 'make', { ok: true, text: padded }))).toEqual(['Bash(make)', 'a', '+6 more lines', 'z'])
    // Output one line over the bound is drawn whole. The count costs the row it saves.
    expect(text(call('bash', 'make', { ok: true, text: 'a\nb\nc\nd' }))).toEqual(['Bash(make)', 'a', 'b', 'c', 'd'])
  })

  test('bounds a long input under its head as it bounds output', () => {
    const script = Array.from({ length: 10 }, (_, index) => `step ${index}`).join('\n')
    expect(text(call('bash', `sh <<EOF\n${script}\nEOF`))).toEqual(['Bash(sh <<EOF)', 'step 0', 'step 1', '+8 more lines', 'EOF'])
    // A bound that collapses output still keeps a line of the description.
    const described = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls', detail: [{ text: 'List files' }] }, committed)
    expect(text(described.slice(1))).toEqual(['Bash(ls)', 'List files'])
  })

  test('counts one line in the singular the locale supplied', () => {
    const read = present({ kind: 'tool-call', callId: 'c1', tool: 'read', input: 'Read a.md', result: { ok: true, text: 'only' } },
      { ...committed, single: 'line' })
    expect(read[1]!.text).toBe('Read(a.md)  1 line')
  })
})

describe('present, grouping a step\'s calls', () => {
  const ran = (callId: string, input: string, ok = true, text = ''): ToolCallRow =>
    ({ kind: 'tool-call', callId, tool: 'bash', input, result: { ok, text } })
  const failures: ResultBound = { ...live, failures: 'failed' }

  test('hangs each call from one head that counts them, in the rail', () => {
    const lines = present({ kind: 'tool-group', calls: [
      ran('a', 'make', true, 'one\ntwo'),
      { kind: 'tool-call', callId: 'b', tool: 'read', input: 'Read a.md', result: { ok: true, text: 'x' } },
      ran('c', 'false', false, 'boom'),
    ] }, failures)
    expect(lines[0]!.text).toBe('')
    expect(lines.slice(1).map(line => [line.marker, line.verb, line.text])).toEqual([
      [MARKER.action, '', 'ran 2 \u00b7 read 1 \u00b7 1 failed'],
      ['\u251c', '', 'Bash(make)'], ['\u2502', CONNECTOR, 'one'], ['\u2502', '', 'two'],
      ['\u251c', '', 'Read(a.md)'], ['\u2502', CONNECTOR, 'x'],
      ['\u2514', '', 'Bash(false)'], [MARKER.none, CONNECTOR, 'boom'],
    ])
    // The head carries the step's state, each branch its call's, and the stem recedes.
    expect([lines[1]!.markerTone, lines[2]!.markerTone, lines[3]!.markerTone, lines[7]!.markerTone]).toEqual(['failed', 'done', 'quiet', 'failed'])
    expect(lines[1]!.spans?.at(-1)).toEqual({ length: ' \u00b7 1 failed'.length, tone: 'failed' })
    // One blank opens the block; none separates its calls.
    expect(lines.filter(isBlank)).toHaveLength(1)
  })

  test('counts in the present tense, and blinks only the head while a call runs', () => {
    const [, head, first, second] = present({ kind: 'tool-group', calls: [
      ran('a', 'make'), { kind: 'tool-call', callId: 'b', tool: 'bash', input: 'make test' },
    ] }, failures)
    expect([head!.text, head!.pulse]).toEqual(['run 2', true])
    // A blinking branch would open a gap in the tree, so the branches hold still.
    expect([first!.pulse, second!.pulse, second!.text]).toEqual([false, false, 'Bash(make test)'])
  })
})

describe('present, drawing a change', () => {
  const edit = (detail: readonly CardLine[]) =>
    ({ kind: 'tool-call' as const, callId: 'c1', tool: 'edit', input: 'Edit a.ts', result: { ok: true, text: '', title: 'Edit a.ts', detail } })
  const change = (side: 'added' | 'removed', code: string, number: number, changed?: readonly (readonly [number, number])[]): CardLine =>
    ({ text: `${side === 'added' ? '+' : '-'} ${code}`, emphasis: side, source: 'a.ts', number, ...changed === undefined ? {} : { changed } })
  const gap: CardLine = { text: '⋯', emphasis: 'gap' }
  const detail = [change('removed', 'one', 3), change('added', 'uno', 3), gap, change('removed', 'two', 9), change('added', 'dos', 9)]

  test('says how much changed on the head, even with no lines under it', () => {
    const [, head] = present(edit(detail), committed)
    expect(head!.text).toBe('Edit(a.ts)  +2 −2')
    expect(head!.spans?.slice(-3)).toEqual([{ length: 4, tone: 'added' }, { length: 1, tone: 'plain' }, { length: 2, tone: 'removed' }])
    expect(present(edit(detail), committed)).toHaveLength(2)
    expect(present(edit([change('added', 'new', 1)]), committed)[1]!.text).toBe('Edit(a.ts)  +1')
  })

  test('numbers each changed line in the gutter, and not the gap', () => {
    const lines = present(edit(detail), shown).slice(2)
    expect(lines.map(line => [line.gutter, line.text, line.tone])).toEqual([
      ['3', '- one', 'removed'], ['3', '+ uno', 'added'], [undefined, '⋯', 'quiet'], ['9', '- two', 'removed'], ['9', '+ dos', 'added'],
    ])
    expect(lines.every(line => line.verb === '' && line.column === COLUMN.output)).toBe(true)
    // A number too wide for the column is dropped. Wrapping it would break the gutter.
    expect(present(edit([change('added', 'x', 1234567)]), shown)[2]!.gutter).toBeUndefined()
  })

  test('counts changed lines against the preview, and never ends it on a gap', () => {
    const two = present(edit(detail), { ...live, lines: 2 }).slice(2)
    expect(two.map(line => line.text)).toEqual(['- one', '+ uno', '+2 more lines'])
    const three = present(edit(detail), live).slice(2)
    // A count for one line would cost the row it saves, so the line is drawn.
    expect(three.map(line => line.text)).toEqual(['- one', '+ uno', '⋯', '- two', '+ dos'])
  })

  test('reverses the changed words in the side\'s tone', () => {
    const [line] = present(edit([change('added', 'return null', 1, [[9, 13]])]), shown).slice(2)
    expect(line!.spans).toEqual([{ length: 9, tone: 'added' }, { length: 4, tone: 'added', inverse: true }])
  })

  test('lays syntax colour over the side\'s tone, one run of a side at a time', () => {
    const calls: (readonly string[])[] = []
    const highlight: Highlight = lines => {
      calls.push(lines)
      return lines.map(text => [{ length: 3, color: '#268bd2' }, { length: text.length - 3 }])
    }
    const lines = present(edit([change('removed', 'one', 1), change('removed', 'two', 2, [[2, 5]]), change('added', 'uno', 1)]), { ...shown, code: highlight }).slice(2)
    expect(calls).toEqual([['one', 'two'], ['uno']])
    expect(lines[0]!.spans).toEqual([{ length: 2, tone: 'removed' }, { length: 3, tone: 'removed', color: '#268bd2' }])
    // Changed words stay reversed; highlighting does not reach them.
    expect(lines[1]!.spans).toEqual([{ length: 2, tone: 'removed' }, { length: 3, tone: 'removed', inverse: true }])
    // A highlighter that declines leaves the side's tone.
    expect(present(edit([change('added', 'uno', 1)]), { ...shown, code: () => undefined })[2]!.spans).toEqual([{ length: 5, tone: 'added' }])
  })

  test('highlights only the lines a preview draws', () => {
    const seen: string[] = []
    const highlight: Highlight = lines => {
      seen.push(...lines)
      return lines.map(text => [{ length: text.length, color: '#268bd2' }])
    }
    const file = Array.from({ length: 5000 }, (_, index): CardLine => ({ text: `line ${index + 1}`, source: 'a.ts', number: index + 1 }))
    const read = { kind: 'tool-call' as const, callId: 'c1', tool: 'read', input: 'Read a.ts', result: { ok: true, text: '', detail: file } }
    const lines = present(read, { ...live, code: highlight }).slice(2)
    // The head and the tail of the output, and nothing the count stands for.
    expect(seen).toEqual(['line 1', 'line 2', 'line 5000'])
    expect(lines.map(line => [line.text, line.spans?.[0]?.color])).toEqual([
      ['line 1', '#268bd2'], ['line 2', '#268bd2'], ['+4997 more lines', undefined], ['line 5000', '#268bd2'],
    ])
    // A collapsed result draws no lines, so none reach the grammar.
    seen.length = 0
    present(read, { ...committed, code: highlight })
    expect(seen).toEqual([])
    // A result row previews its head.
    seen.length = 0
    present({ kind: 'tool-result', callId: 'c1', ok: true, text: '', detail: file }, { ...live, code: highlight })
    expect(seen).toEqual(['line 1', 'line 2', 'line 3'])
  })

  test('keeps a failed change red throughout', () => {
    const failed = { ...edit(detail), result: { ok: false, text: '', detail } }
    const lines = present(failed, shown, undefined).slice(2)
    expect(lines.every(line => line.tone === 'failed' && line.spans === undefined)).toBe(true)
  })
})

describe('present, previewing reasoning in scrollback', () => {
  /** Rows at a fixed width, the way the transcript wraps them. */
  const wrap = (line: PresentedLine) => wrapAnsi(softBreaks(line.text, 20), 20, { hard: true, trim: false }).split('\n')
  const reasoning = (text: string, bound = live) => present({ kind: 'reasoning', text }, bound, wrap)

  test('keeps the first rows and counts the rest, one paragraph or many', () => {
    const [blank, ...lines] = reasoning('one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen')
    expect(blank!.text).toBe('')
    expect(lines.map(line => [line.verb, line.text])).toEqual([
      ['', 'one two three four'], ['', 'five six seven eight'], ['', 'nine ten eleven'], ['', '+3 more lines'],
    ])
    expect(lines.every(line => line.column === COLUMN.rail)).toBe(true)
    expect(lines.map(line => line.tone)).toEqual(['thought', 'thought', 'thought', 'quiet'])
    const many = reasoning(Array.from({ length: 8 }, (_, index) => `line ${index}`).join('\n'))
    expect(many.slice(1).map(line => line.text)).toEqual(['line 0', 'line 1', 'line 2', '+5 more lines'])
  })

  test('ends the preview on text rather than on a paragraph break', () => {
    const lines = reasoning('a\nb\n\nc\nd\ne')
    expect(lines.slice(1).map(line => line.text)).toEqual(['a', 'b', '+4 more lines'])
  })

  test('draws a block whole when a count would hide only its last row', () => {
    const lines = reasoning('a\nb\nc\nd')
    expect(lines.slice(1).map(line => line.text)).toEqual(['a', 'b', 'c', 'd'])
  })

  test('collapses to a size, slanted as reasoning, when the bound keeps no rows', () => {
    const lines = reasoning('a\nb\nc', committed)
    expect(lines.slice(1).map(line => [line.verb, line.text, line.tone, line.column])).toEqual([['', '3 lines', 'thought', COLUMN.rail]])
  })

  test('draws every row where no width is given, as the live region does', () => {
    expect(present({ kind: 'reasoning', text: 'a\nb\nc\nd\ne' }, live)).toHaveLength(6)
  })
})

describe('softBreaks', () => {
  const rows = (text: string, width: number) => wrapAnsi(softBreaks(text, width), width, { hard: true, trim: false }).split('\n')

  test('breaks at the space that would open a row, keeping the text length', () => {
    // `for` ends at the width, so Ink moves the space after it to the next row.
    expect(wrapAnsi('check it for the read', 12, { hard: true, trim: false }).split('\n')).toEqual(['check it for', ' the read'])
    expect(softBreaks('check it for the read', 12)).toBe('check it for\nthe read')
    expect(rows('check it for the read', 12)).toEqual(['check it for', 'the read'])
    for (let width = 4; width <= 30; width++) {
      const text = 'a loader reads profiles, then opens a journal so replay works; averyveryverylongword too'
      const broken = softBreaks(text, width)
      expect(broken.length).toBe(text.length)
      for (const row of rows(text, width)) {
        expect(row.startsWith(' ')).toBe(false)
        expect(row.length).toBeLessThanOrEqual(width)
      }
    }
  })

  test('leaves text that fits, or that the wrapper rewrites, as it is', () => {
    expect(softBreaks('short', 20)).toBe('short')
    expect(softBreaks('tab\there and there more', 7)).toBe('tab\there and there more')
  })
})

describe('tailLines', () => {
  const reasoning = (count: number) =>
    present({ kind: 'reasoning', text: Array.from({ length: count }, (_, index) => `line ${index}`).join('\n') }, shown)
  const call = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls\nwc -l\nsort' }, shown)
  const answer = (count: number) =>
    present({ kind: 'assistant', text: Array.from({ length: count }, (_, index) => `answer ${index}`).join('\n') }, shown)
  /** A call whose script runs to `count` lines. A section hanging from its head at the output column. */
  const script = (count: number) =>
    present({ kind: 'tool-call', callId: 'c2', tool: 'bash', input: Array.from({ length: count }, (_, index) => `line ${index}`).join('\n') }, shown)

  test('keeps the newest lines when the turn outgrows the region, under its opening blank', () => {
    const lines = tailLines(reasoning(20), 4)
    expect(lines).toHaveLength(4)
    expect(lines[0]!.text).toBe('')
    expect(lines.at(-1)!.text).toBe('line 19')
  })

  test('carries the connector onto the first line a cut section left standing', () => {
    // Without this the window shows lines at the output column with nothing
    // saying which action they belong to — which is exactly what a long turn
    // looks like once it passes the budget.
    const lines = tailLines(script(20), 4)
    expect(lines[1]!.verb).toBe(CONNECTOR)
    expect(lines[1]!.verbTone).toBe('quiet')
    expect(lines[1]!.text).toBe('line 17')
    expect(lines.slice(2).every(line => line.verb === '')).toBe(true)
  })

  test('shows an older section whole or not at all', () => {
    // A tool's output above a streaming answer used to lose a line per line
    // of the answer, until only its footer was left.
    const lines = [...reasoning(3), ...call]
    expect(tailLines(lines, 9)).toEqual(lines)
    for (let budget = call.length; budget < lines.length; budget++) expect(tailLines(lines, budget)).toEqual(call)
    expect(tailLines(lines, 2)[1]!.verb).toBe(CONNECTOR)
    expect(tailLines(lines, 2)[1]!.text).toBe('sort')
  })

  test('leaves a line that already carries a verb, and adds no marker to a cut answer', () => {
    expect(tailLines(answer(3), 3).map(line => line.text)).toEqual(['', 'answer 1', 'answer 2'])
    expect(tailLines(answer(3), 3).every(line => line.marker === MARKER.none)).toBe(true)
    expect(tailLines(script(3), 4)[0]!.text).toBe('')
    expect(tailLines(script(3), 4)[1]!.text).toBe('Bash(line 0)')
    expect(tailLines(script(3), 4)[2]!.verb).toBe(CONNECTOR)
    expect(tailLines(script(3), 3)[1]!.verb).toBe(CONNECTOR)
    expect(tailLines(script(3), 3)[1]!.text).toBe('line 1')
    expect(tailLines(reasoning(3), 3)[1]!.marker).toBe(MARKER.none)
  })

  test('counts wrapped rows, filling the window with prose but keeping output lines whole', () => {
    // Every line two rows tall. Whole lines alone would leave an odd window a
    // row short, and the composer would bob with each paragraph.
    const tall = () => 2
    const prose = tailLines(answer(6), 6, line => line.text === '' ? 1 : tall())
    expect(prose.map(line => line.text)).toEqual(['', 'answer 3', 'answer 4', 'answer 5'])
    const output = tailLines(script(6), 6, line => line.text === '' ? 1 : tall())
    expect(output.map(line => line.text)).toEqual(['', 'line 4', 'line 5'])
    expect(output[1]!.verb).toBe(CONNECTOR)
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

describe('fittedGroup', () => {
  const bound: ResultBound = { lines: 8, unit: 'lines', more: 'more lines', earlier: 'earlier calls' }
  /** A step of `count` calls; all but the last `running` have ten lines of output. */
  const step = (count: number, running = 1): ToolCallRow[] => Array.from({ length: count }, (_, index) => ({
    kind: 'tool-call', callId: `c${index}`, tool: 'read', input: `f${index}.ts`,
    ...index >= count - running ? {} : { result: { ok: true, text: Array.from({ length: 10 }, (_, line) => `${index}:${line}`).join('\n') } },
  }))
  const texts = (lines: readonly PresentedLine[]): string[] => lines.map(line => line.text)

  test('draws a step that fits exactly as present does', () => {
    const calls = step(2)
    expect(fittedGroup(calls, bound, 40)).toEqual(present({ kind: 'tool-group', calls }, bound))
  })

  test('folds finished calls oldest first, keeping the head and the newest output', () => {
    // Whole. Blank, head, and 10 rows per finished call (head, 8 lines, count) plus the running call.
    const lines = fittedGroup(step(3), bound, 14)
    expect(lines.length).toBeLessThanOrEqual(14)
    expect(isBlank(lines[0]!)).toBe(true)
    expect(lines[1]!.text).toBe('read 3')
    expect(texts(lines)).toContain('Read(f0.ts)')
    expect(texts(lines)).not.toContain('0:0')
    expect(texts(lines)).toContain('1:0')
    expect(lines.at(-1)).toMatchObject({ text: 'Read(f2.ts)', marker: TREE.corner })
  })

  test('folds the oldest calls into one branch once every finished call is a head', () => {
    const lines = fittedGroup(step(12), bound, 8)
    expect(lines).toHaveLength(8)
    expect(lines[1]!.text).toBe('read 12')
    // Blank, head, and the summary leave five rows. The five newest calls.
    expect(lines[2]).toMatchObject({ text: '+7 earlier calls', marker: TREE.branch, tone: 'quiet' })
    expect(texts(lines.slice(3))).toEqual(['Read(f7.ts)', 'Read(f8.ts)', 'Read(f9.ts)', 'Read(f10.ts)', 'Read(f11.ts)'])
  })

  test('on a window too short for the summary and the newest call, keeps the head above all', () => {
    const calls = step(12, 0)
    expect(texts(fittedGroup(calls, bound, 4))).toEqual(['', 'read 12', '+11 earlier calls', 'Read(f11.ts)'])
    expect(texts(fittedGroup(calls, bound, 3))).toEqual(['read 12', '+11 earlier calls', 'Read(f11.ts)'])
    expect(texts(fittedGroup(calls, bound, 2))).toEqual(['read 12', 'Read(f11.ts)'])
    expect(texts(fittedGroup(calls, bound, 1))).toEqual(['read 12'])
  })

  test('without the words for hidden calls, folds every call to its head and hides none', () => {
    const lines = fittedGroup(step(12), { ...bound, earlier: undefined }, 8)
    expect(lines).toHaveLength(14)
  })
})
