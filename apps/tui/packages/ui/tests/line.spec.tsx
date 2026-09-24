/** Column placement and wrapping of the layout components. */
import React from 'react'
import { renderToString, Text } from 'ink'
import { describe, expect, it } from 'vitest'
import { budgetFor, CHROME_ROWS, chromeFor, COLUMN, COMPOSER_BUDGET, HINT_MIN_COLUMNS, isRenderable, MARKER, type FrameStyle } from '../src/layout.ts'
import { present, type ResultBound } from '../src/present.ts'
import { Chrome, Completion, Composer, Line, ruleRoom, StatusBar, type RuleState } from '../src/line.tsx'
import { dictionaries } from '../src/copy.ts'
import { PALETTE, permissionTone } from '../src/palette.ts'
import { SPINNER_REST } from '../src/activity.ts'
import stringWidth from 'string-width'

const strip = (text: string): string => text.replace(/\u001B\[[0-9;]*m/g, '')
/** The composer's drawn caret, the one glyph in the draft that is not ASCII. */
const CARET = '\u258c'
const at80 = budgetFor({ columns: 80, rows: 24 })
/** Every result line drawn, so these tests assert placement and nothing else. */
const shown: ResultBound = { lines: Number.MAX_SAFE_INTEGER, unit: 'lines', more: 'more lines' }

/** Render placed lines the way the transcript would. */
const show = (rows: readonly React.ReactElement[], columns = 80): string =>
  strip(renderToString(<>{rows}</>, { columns }))

describe('Line', () => {
  it('starts prose at the rail and output under the verb', () => {
    const said = show(present({ kind: 'user', text: 'hello' }, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    // The outcome line, then the output under it.
    const result = { kind: 'tool-result' as const, callId: 'c', ok: true, text: 'result' }
    const output = show(present(result, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} />))

    // A blank row opens the turn, so the eye has somewhere to land when
    // scrolling back; scrollback pays for it, not the dynamic region.
    expect(said).toBe(`\n  ${'-'.repeat(at80.measure)}\n${MARKER.turn} hello`)
    expect(output.split('\n')[1]!.indexOf('result')).toBe(COLUMN.output)
  })

  it('places a call verb in its own column, with the argument beside it', () => {
    const rendered = show(present({ kind: 'tool-call', callId: 'c', tool: 'bash', input: 'rg -n foo' }, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    // The blank that opens the call's zone renders as an empty first row.
    expect(rendered).toBe(`\n${MARKER.action} run    rg -n foo`)
  })

  it('wraps prose against the current terminal width without a fixed measure', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'word '.repeat(80).trim()
    const rendered = show(present({ kind: 'assistant', text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)
    const longest = Math.max(...rendered.split('\n').map(line => line.trimEnd().length))

    expect(longest).toBeLessThanOrEqual(300)
    expect(longest).toBeGreaterThan(88 + COLUMN.rail)
    expect(rendered.split('\n').length).toBeGreaterThan(1)
  })

  it('gives flush command text the columns its missing rail freed', () => {
    const columns = 120
    const budget = budgetFor({ columns, rows: 24 })
    const rendered = show(present({ kind: 'command', name: 'help', args: ` ${'x'.repeat(columns - 6)}` }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} />), columns)
    expect(rendered.split('\n').at(-1)).toHaveLength(columns)
  })

  it('keeps tool output aligned under the verb at a wide width', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'x'.repeat(200)
    const rendered = show(present({ kind: 'tool-result', callId: 'c', ok: true, text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)

    // The outcome, then one line: 200 characters fit after the verb.
    expect(rendered.split('\n')).toHaveLength(2)
  })

  it.each([24, 40, 80, 160])('bounds turn dividers and reasoning at %i columns', columns => {
    const budget = budgetFor({ columns, rows: 24 })
    const rows = [
      { kind: 'user' as const, text: 'Inspect the configuration.' },
      { kind: 'reasoning' as const, text: 'Checking the configuration and its defaults. '.repeat(12) },
      { kind: 'assistant' as const, text: 'The configuration is valid.\n\nNo changes needed.' },
      { kind: 'notice' as const, placement: 'turn-end' as const, tone: 'info' as const, text: 'Completed' },
    ]
    const rendered = show(rows.flatMap(row => present(row, shown))
      .map((line, index) => <Line key={index} line={line} budget={budget} />), columns)
    expect(rendered.split('\n').every(line => line.length <= columns)).toBe(true)
    expect(rendered).toContain(`  ${'-'.repeat(budget.measure)}`)
    expect(rendered).toContain('< The configuration')
    expect(rendered).not.toContain('Completed')
    expect(Math.max(...rendered.split('\n').map(line => line.length))).toBeLessThanOrEqual(columns)
  })

  it.each([1, 2, 3, 8, 9, 10, 20])('fits response and result text at %i columns without losing labels', columns => {
    const budget = budgetFor({ columns, rows: 24 })
    const rows = [
      { kind: 'assistant' as const, text: '**Answer** ' + 'x'.repeat(24) },
      { kind: 'tool-result' as const, callId: 'c', ok: false, text: 'result-text' },
    ]
    const rendered = show(rows.flatMap(row => present(row, shown))
      .map((line, index) => <Line key={index} line={line} budget={budget} />), columns)
    expect(rendered.split('\n').every(row => row.length <= columns), rendered).toBe(true)
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('Answer' + 'x'.repeat(24))
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('error')
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('result-text')
  })

  it('keeps failed output on the output column, like the result it is', () => {
    // Colour is asserted in present.test.ts: renderToString emits no ANSI
    // outside a TTY, so a colour assertion here would pass for the wrong reason.
    const failed = { kind: 'tool-result' as const, callId: 'c', ok: false, text: 'boom' }
    const rendered = show(present(failed, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    expect(rendered.split('\n')[0]).toBe('  error  [c]  1 lines')
    expect(rendered.split('\n')[1]!.indexOf('boom')).toBe(COLUMN.output)
  })
})

describe('StatusBar', () => {
  it.each(['en', 'zh'] as const)('keeps access and an explicit thinking level on one row in %s', async locale => {
    const copy = dictionaries[locale]
    const frames = [120, 60, 40, 38, 24, 1].flatMap(columns =>
      ['workspace-write', 'danger-full-access'].map(permission => {
        const frame = strip(renderToString(<StatusBar
          left={[{ text: `${copy.model}: a-very-long-model-name` }]}
          badge={{ label: copy.permission, value: permission, color: permissionTone(permission) }}
          secondaryBadge={{ label: copy.thinking, value: 'high', color: PALETTE.asking }}
          right={['Context: ~3k/128k (2%)', '/workspace']} columns={columns} />, { columns }))
        expect(frame.split('\n')).toHaveLength(1)
        if (columns >= 38) {
          expect(frame).toContain(`${copy.permission} ${permission}`)
          expect(frame).toContain(`${copy.thinking} high`)
        }
        return `${columns}: ${frame}`
      }))
    await expect(frames.join('\n') + '\n').toMatchFileSnapshot(`./expected/thinking-status.${locale}.txt`)
  })

  it.each(['en', 'zh'] as const)('retains the permission boundary before a long model at narrow widths in %s', async locale => {
    const frames = [120, 60, 38, 24, 12, 1].flatMap(columns =>
      ['read-only', 'workspace-write', 'danger-full-access', 'custom'].map(value => {
        const frame = strip(renderToString(<StatusBar
          left={[{ text: `${dictionaries[locale].model}: a-very-long-model-name (high)` }]}
          badge={{ label: dictionaries[locale].permission, value, color: permissionTone(value) }}
          right={['Context: ~3k/128k (2%)', '/workspace']} columns={columns} />, { columns }))
        expect(frame.split('\n')).toHaveLength(1)
        if (columns >= 38) expect(frame).toContain(`${dictionaries[locale].permission} ${value}`)
        return `${columns}: ${frame}`
      }))
    await expect(frames.join('\n') + '\n').toMatchFileSnapshot(`./expected/permissions.${locale}.txt`)
  })

  it('packs every field to the left, two spaces apart', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['Model: deepseek/chat', 'plan']} right={['ctx 12%', 'turn 3']} columns={60} />,
      { columns: 60 }))

    // The regression this guards: the row justified to both edges, which opened
    // a gap the width of the terminal between the model and the next field.
    expect(rendered.trimEnd()).toBe('Model: deepseek/chat  plan  ctx 12%  turn 3')
  })

  it('truncates rather than wrapping to a second row', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['Model: deepseek/chat']} right={['ctx 12%', 'turn 3', '0f3a9c']} columns={34} />,
      { columns: 34 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('Model: deepseek/chat')
  })

  it('drops a bounded field whole, from the end, rather than cutting it', () => {
    const fields = [{ text: 'Context: ~3k/128k (2%)' }, 'in 12.3k', 'out 1.2k', 'cache hit 85%', '~/workspace']
    const at = (columns: number) => strip(renderToString(
      <StatusBar left={[{ text: 'Model: deepseek-v4-flash' }]} right={fields} columns={columns} />, { columns })).trimEnd()
    expect(at(120)).toBe('Model: deepseek-v4-flash  Context: ~3k/128k (2%)  in 12.3k  out 1.2k  cache hit 85%  ~/workspace')
    // `cache hit 85%` no longer fits beside the others, so it goes whole and
    // the path takes what is left.
    const narrow = at(80)
    expect(narrow).toContain('out 1.2k')
    expect(narrow).not.toContain('cache')
    expect(narrow.endsWith('workspace')).toBe(true)
    // The path is the one field kept at any width, cut from its head.
    expect(at(40)).toBe('Model: deepseek-v4-flash  ~/workspace')
    expect(at(34)).toBe('Model: deepseek-v4-flash  …rkspace')
  })

  it('yields the right cluster before the left, whatever the path costs', () => {
    // The regression this guards: an unbounded right field starving the two
    // fields the row exists to show. A deep temp path is the everyday case.
    const path = '/private/var/folders/jg/zcyzdbb13bnfr5q882y_h8_r0000gn/T/dsh-tui-pty-1B3VG9/workspace'
    const rendered = strip(renderToString(
      <StatusBar left={['Model: tui-picked-model (high)']} right={['Context: ~3k/128k (2%)', path]} columns={120} />,
      { columns: 120 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('Model: tui-picked-model (high)')
    // The meter is bounded, so it is never shortened to fit the path.
    expect(rendered).toContain('Context: ~3k/128k (2%)')
    // The path is ordered last, so it is the field that gives up room, and it
    // keeps the tail that names the workspace rather than the mount point.
    expect(rendered.endsWith('workspace')).toBe(true)
    expect(rendered).not.toContain('/private/var/folders')
  })

  it('clips rather than wrapping once the left cluster alone overruns', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['Model: a-very-long-model-name-indeed']} right={['/deep/path']} columns={12} />,
      { columns: 12 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered.startsWith('Model: a-ve')).toBe(true)
  })

  it('keeps a wide-character line on one row', () => {
    // Each CJK cell is two columns: counting code points puts this past the edge.
    const rendered = strip(renderToString(
      <StatusBar left={['\u6a21\u578b: model', '/workspace']} right={['\u4e0a\u4e0b\u6587: ~500/128k (0%)']} columns={60} />,
      { columns: 60 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('\u4e0a\u4e0b\u6587: ~500/128k (0%)')
  })
})

describe('Chrome', () => {
  const hints = { send: 'enter to send', interrupt: 'esc interrupts', select: 'up/down to select', answer: 'y or n' }
  // No `drafting` here: Chrome reads it from the draft, so the two cannot disagree.
  const idle = { running: false, asking: false, listing: false }
  const working: RuleState = { kind: 'running', word: 'Kneading', phase: 'writing', startedAt: 0, color: PALETTE.running }
  const ended: RuleState = { kind: 'ended', summary: { outcome: 'done', label: 'Completed', details: '9s · ran 1' } }
  const draw = (columns: number, options: {
    readonly text?: string, readonly state?: typeof idle, readonly frame?: FrameStyle
    readonly light?: RuleState, readonly rows?: number, readonly children?: React.ReactNode
  } = {}): string[] => strip(renderToString(
    <Chrome
      left={['Model: deepseek/chat']} right={['ctx 12%']} columns={columns}
      state={options.state ?? idle} before={options.text ?? ''} after="" placeholder="Ask anything" hints={hints}
      frame={options.frame ?? 'round'} light={options.light}
      {...options.rows === undefined ? {} : { layout: chromeFor(columns, options.rows) }}
    >{options.children}</Chrome>, { columns })).split('\n')
  const at = (columns: number, text = '', state = idle): string => draw(columns, { text, state }).join('\n')
  const render = (state: typeof idle, text = ''): string => at(60, text, state)

  it('spends a blank, the rule, the draft, a padding row, and the status line, and nothing else', () => {
    const rows = render(idle).split('\n')
    // CHROME_ROWS is charged against every other region's budget on every
    // frame, so the floor is measured here rather than assumed.
    expect(rows).toHaveLength(CHROME_ROWS)
    // A blank opens the chrome: without it the rule sits directly under the
    // last line of the answer and reads as part of it.
    expect(rows[0]).toBe('')
    expect(rows[1]).toBe('\u2500'.repeat(60))
    expect(rows[2]).toBe(`${MARKER.prompt} ${CARET}Ask anything`)
    // Clean padding keeps the metadata clear of the draft.
    expect(rows[3]).toBe('')
    // The status line starts at the draft's column, as the rule's label does.
    expect(rows[4]).toBe('  Model: deepseek/chat  ctx 12%')
    expect(rows[2]!.indexOf(CARET)).toBe(rows[4]!.indexOf('Model:'))
  })

  it('puts the running work and the last turn in the rule, at the draft column, edge to edge', () => {
    const running = draw(60, { light: working, state: { ...idle, running: true } })
    expect(running[1]).toMatch(new RegExp(`^\u2500 ${SPINNER_REST} Kneading…  writing \u2500+$`))
    expect(running[1]).toHaveLength(60)
    expect(running[1]!.indexOf(SPINNER_REST)).toBe(running[2]!.indexOf(CARET))
    const done = draw(60, { light: ended })
    expect(done[1]).toMatch(/^─ ✓ Completed {2}9s · ran 1 ─+$/)
    expect(done[1]).toHaveLength(60)
    // One row whatever it says, so the input never moves between them.
    expect(running).toHaveLength(CHROME_ROWS)
    expect(done).toHaveLength(CHROME_ROWS)
  })

  it('cuts the label before the line, and drops it rather than leave no line', () => {
    for (const columns of [1, 2, 5, 6, 12, 24, 40]) {
      const [rule] = draw(columns, { light: working }).slice(1)
      expect(stringWidth(rule!), `${columns}`).toBe(columns)
      // A label always leaves a space and two cells of line after it.
      if (columns >= 6) expect(rule, `${columns}`).toMatch(/^─ \S.* ─{2,}$/)
      else expect(rule, `${columns}`).toBe('\u2500'.repeat(columns))
    }
    expect(draw(24, { light: working })[1]).toMatch(/^─ ⠠⠞⠁ Kneading… {2}\S*… ──$/)
  })

  it('draws the rule in ASCII where the terminal cannot draw box characters', () => {
    const rows = draw(60, { frame: 'classic', light: working })
    expect(rows[1]).toMatch(new RegExp(`^- ${SPINNER_REST} Kneading…  writing -+$`))
    expect(isRenderable(draw(60, { frame: 'classic' })[1]!)).toBe(true)
  })

  it('draws panels between the opening blank and the rule', () => {
    const rows = draw(60, { children: <Text>notice</Text> })
    expect(rows[0]).toBe('')
    expect(rows[1]).toBe('notice')
    expect(rows[2]).toBe('\u2500'.repeat(60))
    expect(rows[3]!.startsWith(`${MARKER.prompt} `)).toBe(true)
  })

  it('keeps its height when only the width changes', () => {
    // From the width a five-letter draft fits in without wrapping.
    const heights = [8, 12, 19, 20, 24, 39, 40, 60, 200].map(columns => at(columns, 'hello').split('\n').length)
    expect(new Set(heights)).toEqual(new Set([CHROME_ROWS]))
  })

  it('yields the gap, the padding, the status line, then the rule, and never the draft', () => {
    const at = (rows: number): string[] => draw(60, { text: 'hello', rows, light: ended })
    expect(at(5)).toHaveLength(5)
    expect(at(4)).toEqual([expect.stringMatching(/^─ ✓/), expect.stringContaining('hello'), '', '  Model: deepseek/chat  ctx 12%'])
    expect(at(3)).toEqual([expect.stringMatching(/^─ ✓/), expect.stringContaining('hello'), '  Model: deepseek/chat  ctx 12%'])
    expect(at(2)).toEqual([expect.stringMatching(/^─ ✓/), expect.stringContaining('hello')])
    expect(at(1)).toEqual([expect.stringContaining('hello')])
  })

  it('leaves the right slot empty until something applies', () => {
    expect(render(idle)).not.toContain('enter to send')
    expect(render(idle, 'hello')).toContain('enter to send')
  })

  it('shows the interrupt hint exactly while a turn runs', () => {
    const running = render({ ...idle, running: true }, 'hello')
    expect(running).toContain('esc interrupts')
    expect(running).not.toContain('enter to send')
  })

  it('fills the width it is given and never overruns it', () => {
    for (const columns of [1, 2, 3, 24, 40, 60, 80, 200]) {
      for (const light of [undefined, working, ended]) {
        for (const row of draw(columns, { text: 'hello', ...light === undefined ? {} : { light } })) {
          expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns)
        }
      }
    }
  })

  it('drops the hint rather than truncating it to something that is not help', () => {
    expect(at(HINT_MIN_COLUMNS, 'hello')).toContain('enter to send')
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).not.toContain('enter to send')
    // What is left is still the draft, whole: the hint yields, the input does not.
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).toContain('hello')
  })
})

describe('ruleRoom', () => {
  it('leaves a space and two cells of line after a label, and drops a label with no room', () => {
    expect(ruleRoom(60, 20)).toEqual({ label: 20, tail: 37 })
    expect(ruleRoom(20, 40)).toEqual({ label: 15, tail: 2 })
    expect(ruleRoom(6, 40)).toEqual({ label: 1, tail: 2 })
    expect(ruleRoom(5, 40)).toEqual({ label: 0, tail: 5 })
    expect(ruleRoom(60, 0)).toEqual({ label: 0, tail: 60 })
    expect(ruleRoom(0, 10)).toEqual({ label: 0, tail: 0 })
  })
})

describe('Composer width and wrapping', () => {
  const draft = (text: string, columns: number, maxRows?: number): string[] => strip(renderToString(
    <Composer
      columns={columns} marker={MARKER.prompt} before={text} after="" placeholder="Ask"
      hint="Enter sends" {...maxRows === undefined ? {} : { maxRows }}
    />, { columns })).split('\n')

  it('bounds a single pasted line that wraps to more rows than it is lines', () => {
    // The regression this guards: `maxRows` counted lines, and a line is not a
    // row. One pasted paragraph is one line, so it passed the count and then
    // wrapped to thirty rows — taking the dynamic region with it and putting
    // Ink on the path where it clears the screen on every keystroke.
    expect(draft('x'.repeat(4000), 80).length).toBeLessThanOrEqual(COMPOSER_BUDGET)
    expect(draft('x'.repeat(4000), 80, 2)).toHaveLength(2)
  })

  it('keeps the caret on the last row whichever bound cut the draft', () => {
    // Clipped from the top, because the caret's line is the last one kept and
    // it is the line being typed.
    expect(draft(`${'x'.repeat(4000)}END`, 80).at(-1)).toContain('END')
    expect(draft('one\ntwo\nthree\nfour\nfive\nsix\nEND', 80).at(-1)).toContain('END')
  })

  it('wraps at the width it is given, so a narrower terminal wraps sooner', () => {
    const wide = draft('word '.repeat(60).trim(), 120)
    const narrow = draft('word '.repeat(60).trim(), 60)
    expect(narrow.length).toBeGreaterThan(wide.length)
    for (const rows of [wide, narrow]) expect(rows.length).toBeLessThanOrEqual(COMPOSER_BUDGET)
  })

  it('keeps every row in place as the caret moves beside the hint', () => {
    const text = 'word 你好 '.repeat(20).trim()
    const rows = (cursor: number): string[] => strip(renderToString(
      <Composer
        columns={80} marker={MARKER.prompt} before={text.slice(0, cursor)} after={text.slice(cursor)} placeholder="Ask"
        hint="Enter sends"
      />, { columns: 80 })).split('\n').map(row => row.replace('\u258c', '').replace('Enter sends', '').trimEnd())
    const expected = rows(text.length)
    for (const cursor of [0, 7, 40, 101]) expect(rows(cursor)).toEqual(expected)
  })

  it('lets the caret take the column before the hint when its row is full', () => {
    // 40 columns: two of rail, thirteen of hint and its gap, so 25 of text and the caret.
    const rows = draft('x'.repeat(25), 40)
    expect(rows).toEqual([`> ${'x'.repeat(25)}\u258c Enter sends`])
  })

  it('puts the hint beside the caret, not beside the first row of a wrapped line', () => {
    // Inside the wrapped text the hint lands on row one and notches the
    // paragraph's top right, leaving the caret row running to the full width.
    const rows = draft('word '.repeat(60).trim(), 80)
    expect(rows.at(-1)).toContain('Enter sends')
    expect(rows.slice(0, -1).some(row => row.includes('Enter sends'))).toBe(false)
  })
})

describe('Composer', () => {
  it('grows with a multi-line draft', () => {
    const rendered = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before={'one\ntwo\nthree'} after="" placeholder="Ask" />, { columns: 40 }))
    expect(rendered.split('\n')).toHaveLength(3)
    expect(rendered.split('\n')[0]).toBe('> one')
    expect(rendered.split('\n')[1]).toBe('  two')
  })

  it('windows a long draft from the bottom, where the caret is', () => {
    const text = Array.from({ length: 12 }, (_, index) => `line ${index}`).join('\n')
    const rendered = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before={text} after="" placeholder="Ask" maxRows={5} />, { columns: 40 }))
    const rows = rendered.split('\n')

    expect(rows).toHaveLength(5)
    expect(rows.at(-1)).toContain('line 11')
    // The prompt marker belongs to the draft's first line, which is scrolled off.
    expect(rows[0]!.startsWith('^')).toBe(true)
    expect(rows[0]!.startsWith('>')).toBe(false)
  })

  it('keeps the hint on the last row, beside the caret', () => {
    const rendered = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before={'one\ntwo'} after="" placeholder="Ask" hint="enter to send" />,
      { columns: 40 }))
    const rows = rendered.split('\n')
    expect(rows[0]).not.toContain('enter to send')
    expect(rows[1]).toContain('enter to send')
  })
})

describe('Completion', () => {
  const items = [
    { name: '/model', description: 'List or select the model' },
    { name: '/compact', description: 'Compact the conversation' },
  ]
  const show = (selected: number, hidden = 0): string => strip(renderToString(
    <Completion items={items} selected={selected} hidden={hidden} more={`+${hidden} more`} />,
    { columns: 60 }))

  it('marks the selection without reusing the composer prompt', () => {
    const rows = show(0).split('\n')
    expect(rows[0]!.startsWith(`${MARKER.selected} `)).toBe(true)
    expect(rows[0]!.startsWith(MARKER.prompt)).toBe(false)
    expect(rows[1]!.startsWith('  ')).toBe(true)
  })

  it('moves the marker with the selection', () => {
    expect(show(1).split('\n')[1]!.startsWith(`${MARKER.selected} `)).toBe(true)
  })

  it('reports what the window omitted', () => {
    expect(show(0, 6)).toContain('+6 more')
  })

  it('renders nothing when there is nothing to offer', () => {
    expect(strip(renderToString(
      <Completion items={[]} selected={0} hidden={0} more="" />, { columns: 40 }))).toBe('')
  })
})

describe('Composer placeholder', () => {
  it('shows the placeholder until there is a draft', () => {
    const empty = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before="" after="" placeholder="Ask anything" />, { columns: 40 }))
    const typed = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before="/model" after="" placeholder="Ask anything" />, { columns: 40 }))

    expect(empty).toContain('Ask anything')
    expect(typed).toContain('/model')
    expect(typed).not.toContain('Ask anything')
  })

  it('omits the right slot when there is nothing contextual to say', () => {
    const without = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before="y" after="" placeholder="Ask" />, { columns: 40 }))
    const with_ = strip(renderToString(
      <Composer columns={40} marker={MARKER.prompt} before="y" after="" placeholder="Ask" hint="enter to send" />, { columns: 40 }))

    expect(without.trimEnd()).toBe('> y\u258c')
    expect(with_).toContain('enter to send')
  })
})
