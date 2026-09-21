/** Column placement and wrapping of the layout components. */
import React from 'react'
import { renderToString } from 'ink'
import { describe, expect, it } from 'vitest'
import { budgetFor, COLUMN, MARKER, PROSE_MEASURE } from '../src/layout.ts'
import { present } from '../src/present.ts'
import { Chrome, Composer, Line, StatusBar } from '../src/line.tsx'

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

    expect(said).toBe(`${MARKER.prompt} hello`)
    expect(output.indexOf('result')).toBe(COLUMN.output)
  })

  it('places a call verb in its own column, with the argument beside it', () => {
    const rendered = show(present({ kind: 'tool-call', callId: 'c', tool: 'bash', input: 'rg -n foo' })
      .map((line, index) => <Line key={index} line={line} budget={at80} />))
    expect(rendered).toBe(`  run    rg -n foo`)
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
    expect(rendered.startsWith('ready')).toBe(true)
    expect(rendered.endsWith('turn 3')).toBe(true)
  })

  it('drops supporting fields rather than wrapping to a second row', () => {
    const rendered = strip(renderToString(
      <StatusBar left={['ready', 'deepseek/chat']} right={['ctx 12%', 'turn 3', '0f3a9c']} columns={34} />,
      { columns: 34 }))

    expect(rendered.split('\n')).toHaveLength(1)
    expect(rendered).toContain('ready')
    expect(rendered).not.toContain('0f3a9c')
  })
})

describe('Chrome', () => {
  const hints = { send: 'enter to send', interrupt: 'esc interrupts', select: 'up/down to select', answer: 'y or n' }
  // No `drafting` here: Chrome reads it from the draft, so the two cannot disagree.
  const idle = { running: false, asking: false, listing: false }
  const render = (state: typeof idle, text?: string): string => strip(renderToString(
    <Chrome
      left={['ready', 'deepseek/chat']} right={['ctx 12%']} columns={60}
      state={state} text={text} placeholder="Ask anything" hints={hints}
    />, { columns: 60 }))

  it('spends two rows, not five', () => {
    // The shipped surface carries a status row, a session row and two standing
    // hint rows. Three of those are charged to the live region for the whole
    // session; only state belongs here.
    expect(render(idle).split('\n')).toHaveLength(2)
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
  it('shows the placeholder until there is a draft', () => {
    const empty = strip(renderToString(
      <Composer marker={MARKER.prompt} text={undefined} placeholder="Ask anything" />, { columns: 40 }))
    const typed = strip(renderToString(
      <Composer marker={MARKER.prompt} text="/model" placeholder="Ask anything" />, { columns: 40 }))

    expect(empty).toContain('Ask anything')
    expect(typed).toContain('/model')
    expect(typed).not.toContain('Ask anything')
  })

  it('omits the right slot when there is nothing contextual to say', () => {
    const without = strip(renderToString(
      <Composer marker={MARKER.prompt} text="y" placeholder="Ask" />, { columns: 40 }))
    const with_ = strip(renderToString(
      <Composer marker={MARKER.prompt} text="y" placeholder="Ask" hint="enter to send" />, { columns: 40 }))

    expect(without.trimEnd()).toBe('> y')
    expect(with_).toContain('enter to send')
  })
})
