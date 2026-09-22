/** Column placement and wrapping of the layout components. */
import React from 'react'
import { renderToString } from 'ink'
import { describe, expect, it } from 'vitest'
import { budgetFor, COLUMN, MARKER, PROSE_MEASURE } from '../src/layout.ts'
import { present } from '../src/present.ts'
import { Chrome, Completion, Composer, Line, StatusBar } from '../src/line.tsx'

const strip = (text: string): string => text.replace(/\u001B\[[0-9;]*m/g, '')
const at80 = budgetFor({ columns: 80, rows: 24 })

/** Render placed lines the way the transcript would. */
const show = (rows: readonly React.ReactElement[], columns = 80): string =>
  strip(renderToString(<>{rows}</>, { columns }))

describe('Line', () => {
  it('starts prose at the rail and output under the verb', () => {
    const said = show(present({ kind: 'user', text: 'hello' })
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    const output = show(present({ kind: 'tool-result', callId: 'c', ok: true, text: 'result' })
      .map((line, index) => <Line key={index} line={line} budget={at80} />))

    // A blank row opens the turn, so the eye has somewhere to land when
    // scrolling back; scrollback pays for it, not the dynamic region.
    expect(said).toBe(`\n${MARKER.turn} hello`)
    expect(output.indexOf('result')).toBe(COLUMN.output)
  })

  it('places a call verb in its own column, with the argument beside it', () => {
    const rendered = show(present({ kind: 'tool-call', callId: 'c', tool: 'bash', input: 'rg -n foo' })
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    // The blank that opens the call's zone renders as an empty first row.
    expect(rendered).toBe(`\n  run    rg -n foo`)
  })

  it('wraps prose at the measure however wide the terminal is', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'word '.repeat(80).trim()
    const rendered = show(present({ kind: 'assistant', text })
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)
    const longest = Math.max(...rendered.split('\n').map(line => line.trimEnd().length))

    expect(longest).toBeLessThanOrEqual(PROSE_MEASURE + COLUMN.rail)
    expect(rendered.split('\n').length).toBeGreaterThan(1)
  })

  it('gives tool output the full width rather than the prose measure', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'x'.repeat(200)
    const rendered = show(present({ kind: 'tool-result', callId: 'c', ok: true, text })
      .map((line, index) => <Line key={index} line={line} budget={budget} />), 300)

    // One line: 200 characters fit the output width but would wrap at the measure.
    expect(rendered.split('\n')).toHaveLength(1)
  })

  it('keeps failed output on the output column, like the result it is', () => {
    // Colour is asserted in present.test.ts: renderToString emits no ANSI
    // outside a TTY, so a colour assertion here would pass for the wrong reason.
    const rendered = show(present({ kind: 'tool-result', callId: 'c', ok: false, text: 'boom' })
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    expect(rendered.indexOf('boom')).toBe(COLUMN.output)
  })
})

describe('StatusBar', () => {
  it('pushes the clusters to both edges so the row reads as chrome', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['ready', 'deepseek/chat']} right={['ctx 12%', 'turn 3']} columns={60} />,
      { columns: 60 }))

    expect(rendered).toHaveLength(60)
    // The state word opens the row. The right cluster is what makes it a bar.
    expect(rendered.startsWith('ready')).toBe(true)
    expect(rendered.endsWith('turn 3')).toBe(true)
    expect(rendered).toContain('deepseek/chat')
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
  const render = (state: typeof idle, text = ''): string => strip(renderToString(
    <Chrome
      left={['ready', 'deepseek/chat']} right={['ctx 12%']} columns={60}
      state={state} before={text} after="" placeholder="Ask anything" hints={hints}
    />, { columns: 60 }))

  it('spends the status row and the composer, and nothing between them', () => {
    const rows = render(idle).split('\n')
    expect(rows).toHaveLength(2)
    expect(rows[0]).toContain('ready')
    expect(rows[0]).toContain('ctx 12%')
    expect(rows[1]).toContain('Ask anything')
    expect(rows.join('\n')).not.toContain('---')
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
})

describe('Composer', () => {
  it('grows with a multi-line draft', () => {
    const rendered = strip(renderToString(
      <Composer marker={MARKER.prompt} before={'one\ntwo\nthree'} after="" placeholder="Ask" />, { columns: 40 }))
    expect(rendered.split('\n')).toHaveLength(3)
    expect(rendered.split('\n')[0]).toBe('> one')
    expect(rendered.split('\n')[1]).toBe('  two')
  })

  it('windows a long draft from the bottom, where the caret is', () => {
    const text = Array.from({ length: 12 }, (_, index) => `line ${index}`).join('\n')
    const rendered = strip(renderToString(
      <Composer marker={MARKER.prompt} before={text} after="" placeholder="Ask" maxRows={5} />, { columns: 40 }))
    const rows = rendered.split('\n')

    expect(rows).toHaveLength(5)
    expect(rows.at(-1)).toContain('line 11')
    // The prompt marker belongs to the draft's first line, which is scrolled off.
    expect(rows[0]!.startsWith('^')).toBe(true)
    expect(rows[0]!.startsWith('>')).toBe(false)
  })

  it('keeps the hint on the last row, beside the caret', () => {
    const rendered = strip(renderToString(
      <Composer marker={MARKER.prompt} before={'one\ntwo'} after="" placeholder="Ask" hint="enter to send" />,
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
      <Composer marker={MARKER.prompt} before="" after="" placeholder="Ask anything" />, { columns: 40 }))
    const typed = strip(renderToString(
      <Composer marker={MARKER.prompt} before="/model" after="" placeholder="Ask anything" />, { columns: 40 }))

    expect(empty).toContain('Ask anything')
    expect(typed).toContain('/model')
    expect(typed).not.toContain('Ask anything')
  })

  it('omits the right slot when there is nothing contextual to say', () => {
    const without = strip(renderToString(
      <Composer marker={MARKER.prompt} before="y" after="" placeholder="Ask" />, { columns: 40 }))
    const with_ = strip(renderToString(
      <Composer marker={MARKER.prompt} before="y" after="" placeholder="Ask" hint="enter to send" />, { columns: 40 }))

    expect(without.trimEnd()).toBe('> y\u258c')
    expect(with_).toContain('enter to send')
  })
})
