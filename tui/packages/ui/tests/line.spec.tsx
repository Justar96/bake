/** Column placement and wrapping of the layout components. */
import React from 'react'
import { renderToString, Text } from 'ink'
import { describe, expect, it } from 'vitest'
import { budgetFor, CHROME_ROWS, COLUMN, COMPOSER_BUDGET, FRAME_MIN_COLUMNS, HINT_MIN_COLUMNS, isRenderable, MARKER, PROSE_MEASURE, type FrameStyle } from '../src/layout.ts'
import { present, type ResultBound } from '../src/present.ts'
import { Chrome, Completion, Composer, Line, StatusBar } from '../src/line.tsx'

const strip = (text: string): string => text.replace(/\u001B\[[0-9;]*m/g, '')
/** The composer's drawn caret, the one glyph inside the frame that is not ASCII. */
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

  it('wraps prose at the measure however wide the terminal is', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'word '.repeat(80).trim()
    const rendered = show(present({ kind: 'assistant', text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)
    const longest = Math.max(...rendered.split('\n').map(line => line.trimEnd().length))

    expect(longest).toBeLessThanOrEqual(PROSE_MEASURE + COLUMN.rail)
    expect(rendered.split('\n').length).toBeGreaterThan(1)
  })

  it('gives tool output the full width rather than the prose measure', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'x'.repeat(200)
    const rendered = show(present({ kind: 'tool-result', callId: 'c', ok: true, text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)

    // The outcome, then one line: 200 characters fit the output width but
    // would wrap at the measure.
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
    if (columns > PROSE_MEASURE + COLUMN.output) {
      expect(Math.max(...rendered.split('\n').map(line => line.length))).toBeLessThanOrEqual(PROSE_MEASURE + COLUMN.output)
    }
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
  it('packs every field to the left, two spaces apart', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['ready', 'deepseek/chat']} right={['ctx 12%', 'turn 3']} columns={60} />,
      { columns: 60 }))

    // The regression this guards: the row justified to both edges, which opened
    // a gap the width of the terminal between the model and the next field.
    expect(rendered.trimEnd()).toBe('ready  deepseek/chat  ctx 12%  turn 3')
  })

  it('truncates rather than wrapping to a second row', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['ready', 'deepseek/chat']} right={['ctx 12%', 'turn 3', '0f3a9c']} columns={34} />,
      { columns: 34 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('ready')
  })

  it('yields the right cluster before the left, whatever the path costs', () => {
    // The regression this guards: an unbounded right field starving the two
    // fields the row exists to show. A deep temp path is the everyday case.
    const path = '/private/var/folders/jg/zcyzdbb13bnfr5q882y_h8_r0000gn/T/dsh-tui-pty-1B3VG9/workspace'
    const rendered = strip(renderToString(
      <StatusBar left={['Ready', 'tui-picked-model (high)']} right={['Context: ~3k/128k (2%)', path]} columns={120} />,
      { columns: 120 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('Ready')
    expect(rendered).toContain('tui-picked-model (high)')
    // The meter is bounded, so it is never shortened to fit the path.
    expect(rendered).toContain('Context: ~3k/128k (2%)')
    // The path is ordered last, so it is the field that gives up room, and it
    // keeps the tail that names the workspace rather than the mount point.
    expect(rendered.endsWith('workspace')).toBe(true)
    expect(rendered).not.toContain('/private/var/folders')
  })

  it('clips rather than wrapping once the left cluster alone overruns', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['Ready', 'a-very-long-model-name-indeed']} right={['/deep/path']} columns={12} />,
      { columns: 12 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered.startsWith('Ready')).toBe(true)
  })

  it('keeps a wide-character line on one row', () => {
    // Each CJK cell is two columns: counting code points puts this past the edge.
    const rendered = strip(renderToString(
      <StatusBar left={['\u5c31\u7eea', 'model', '/workspace']} right={['\u4e0a\u4e0b\u6587: ~500/128k (0%)']} columns={60} />,
      { columns: 60 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('\u4e0a\u4e0b\u6587: ~500/128k (0%)')
  })
})

describe('Chrome', () => {
  const hints = { send: 'enter to send', interrupt: 'esc interrupts', select: 'up/down to select', answer: 'y or n' }
  // No `drafting` here: Chrome reads it from the draft, so the two cannot disagree.
  const idle = { running: false, asking: false, listing: false }
  const at = (columns: number, text = '', state = idle, frame: FrameStyle = 'round'): string => strip(renderToString(
    <Chrome
      left={['ready', 'deepseek/chat']} right={['ctx 12%']} columns={columns}
      state={state} before={text} after="" placeholder="Ask anything" hints={hints} frame={frame}
    />, { columns }))
  const render = (state: typeof idle, text = ''): string => at(60, text, state)

  it('spends a framed composer and the status row under it, and nothing else', () => {
    const rows = render(idle).split('\n')
    // CHROME_ROWS is charged against every other region's budget on every
    // frame, so the floor is measured here rather than assumed.
    expect(rows).toHaveLength(CHROME_ROWS)
    // A blank opens the chrome: without it the frame's top edge sits directly
    // under the last line of the answer and the input reads as more output.
    expect(rows[0]!.trim()).toBe('')
    expect(rows[2]).toContain('Ask anything')
    // The status line is the box's footer, indented to the prompt inside it.
    expect(rows[4]!.startsWith('  ready')).toBe(true)
    expect(rows[4]).toContain('ctx 12%')
    expect(rows[2]!.indexOf(MARKER.prompt)).toBe(rows[4]!.indexOf('ready'))
  })

  it('draws panels between the opening blank and the frame', () => {
    const rows = strip(renderToString(
      <Chrome
        left={['ready']} right={[]} columns={60} state={idle} before="" after=""
        placeholder="Ask anything" hints={hints} frame="round"
      ><Text>notice</Text></Chrome>, { columns: 60 })).split('\n')
    expect(rows[0]!.trim()).toBe('')
    expect(rows[1]).toBe('notice')
    expect(rows[2]!.startsWith('\u256d')).toBe(true)
  })

  it('frames the composer edge to edge, in ASCII when it has to fall back', () => {
    const rows = at(60, '', idle, 'classic').split('\n')

    // The fallback exists for a terminal that cannot draw box characters —
    // not encoding UTF-8, or drawing East Asian Ambiguous two cells wide and
    // so doubling every row of a full-width frame. It has to be ASCII to be
    // worth having.
    expect(rows.slice(1).every(row => isRenderable(row.replace(CARET, '')))).toBe(true)
    expect(rows[1]).toBe(`+${'-'.repeat(58)}+`)
    expect(rows[3]).toBe(rows[1])
    expect(rows[2]!.startsWith('| ')).toBe(true)
    expect(rows[2]!.endsWith(' |')).toBe(true)

    // Edge to edge either way: the rows around it are laid out against the
    // same width whichever characters the frame is made of.
    const round = at(60, '', idle, 'round').split('\n')
    expect(round[1]).toBe(`\u256d${'\u2500'.repeat(58)}\u256e`)
    expect(round[3]).toBe(`\u2570${'\u2500'.repeat(58)}\u256f`)
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
    // ASCII content only, so a code-point count is the display width here;
    // wide cells are Yoga's arithmetic and are asserted in StatusBar above.
    for (const columns of [24, 40, 60, 80, 200]) {
      for (const row of at(columns, 'hello').split('\n')) {
        expect(row.length).toBeLessThanOrEqual(columns)
      }
    }
  })

  it('draws the frame the terminal was resolved to be able to draw', () => {
    // Which one is the application's decision, resolved from the environment
    // once (see `@dsh-tui/app/frame`); this layer only draws what it is told.
    expect(at(60, 'hello', idle, 'round')).toContain('\u256d')
    expect(at(60, 'hello', idle, 'round')).not.toContain('+--')
    expect(at(60, 'hello', idle, 'classic')).toContain('+--')
    expect(at(60, 'hello', idle, 'classic')).not.toContain('\u256d')
  })

  it('spends the same rows and columns whichever frame it draws', () => {
    // A terminal that falls back to ASCII must not also get a different
    // layout: CHROME_ROWS is one number for every terminal.
    for (const columns of [40, 60, 120]) {
      const round = at(columns, 'hello', idle, 'round').split('\n')
      const classic = at(columns, 'hello', idle, 'classic').split('\n')
      expect(round.length).toBe(classic.length)
      expect(round.map(row => row.length)).toEqual(classic.map(row => row.length))
    }
  })

  it('drops the frame below the width where it would cost a tenth of the line', () => {
    // Four columns of border and padding is structure, not content, and below
    // the supported minimum the draft needs them more than the box does.
    expect(at(FRAME_MIN_COLUMNS, 'hello')).toContain('\u2500')
    expect(at(FRAME_MIN_COLUMNS - 1, 'hello')).not.toContain('\u2500')
    // Dropping it costs rows, never adds them.
    expect(at(FRAME_MIN_COLUMNS - 1, 'hello').split('\n').length)
      .toBeLessThan(at(FRAME_MIN_COLUMNS, 'hello').split('\n').length)
  })

  it('drops the hint rather than truncating it to something that is not help', () => {
    expect(at(HINT_MIN_COLUMNS, 'hello')).toContain('enter to send')
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).not.toContain('enter to send')
    // What is left is still the draft, whole: the hint yields, the input does not.
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).toContain('hello')
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
