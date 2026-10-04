/** Column placement and wrapping of the layout components. */
import React from 'react'
import { renderToString, Text } from 'ink'
import { describe, expect, it, vi } from 'vitest'
import { render } from '../../../tests/render.tsx'
import { budgetFor, CHROME_ROWS, chromeFor, COLUMN, COMPOSER_BUDGET, HINT_MIN_COLUMNS, isRenderable, MARKER, RULE, type FrameStyle } from '../src/layout.ts'
import { ICON } from '../src/icons.ts'
import { present, type PresentedLine, type ResultBound } from '../src/present.ts'
import { Chrome, Completion, Composer, fitStanding, headerLayout, Line, StatusBar, wrappedRows, type ActivityState, type StandingState } from '../src/line.tsx'
import { dictionaries } from '../src/copy.ts'
import type { GitState } from '../src/git.ts'
import { PALETTE } from '../src/palette.ts'
import { statusFields, type StatusInput } from '../src/status-line.ts'
import { FOLD_REST, SPINNER_REST } from '../src/activity.ts'
import stringWidth from 'string-width'
import { CARET, markCaret } from '../../../tests/caret.ts'

/** Plain text, with the caret's reverse-video cell marked as `CARET`. */
const strip = (text: string): string => markCaret(text).replace(/\u001B\[[0-9;]*m/g, '')
const at80 = budgetFor({ columns: 80, rows: 24 })
/** Every result line drawn, so these tests assert placement and nothing else. */
const shown: ResultBound = { lines: Number.MAX_SAFE_INTEGER, unit: 'lines', more: 'more lines' }

/** Render placed lines the way the transcript would. */
const show = (rows: readonly React.ReactElement[], columns = 80): string =>
  strip(renderToString(<>{rows}</>, { columns }))

describe('Line', () => {
  it('starts prose at the rail and output under the verb', () => {
    const said = show(present({ kind: 'user', text: 'hello' }, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} frame="classic" />))
    // The outcome line, then the output under it.
    const result = { kind: 'tool-result' as const, callId: 'c', ok: true, text: 'result' }
    const output = show(present(result, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} frame="classic" />))

    // A blank row opens the turn. Scrollback pays for it, not the dynamic region.
    expect(said).toBe(`\n  ${'-'.repeat(at80.measure)}\n${MARKER.turn} hello`)
    expect(output.split('\n')[1]!.indexOf('result')).toBe(COLUMN.output)
  })

  it('heads a call with its tool and the argument in parentheses', () => {
    const rendered = show(present({ kind: 'tool-call', callId: 'c', tool: 'bash', input: 'rg -n foo' }, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} frame="classic" />))
    // The blank that opens the call's zone renders as an empty first row.
    expect(rendered).toBe(`\n${ICON.other} Bash(rg -n foo)`)
  })

  it('wraps prose against the current terminal width without a fixed measure', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'word '.repeat(80).trim()
    const rendered = show(present({ kind: 'assistant', text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />), 300)
    const longest = Math.max(...rendered.split('\n').map(line => line.trimEnd().length))

    expect(longest).toBeLessThanOrEqual(300)
    expect(longest).toBeGreaterThan(88 + COLUMN.rail)
    expect(rendered.split('\n').length).toBeGreaterThan(1)
  })

  it('gives flush command text the columns its missing rail freed', () => {
    const columns = 120
    const budget = budgetFor({ columns, rows: 24 })
    const rendered = show(present({ kind: 'command', name: 'help', args: ` ${'x'.repeat(columns - 6)}` }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />), columns)
    expect(rendered.split('\n').at(-1)).toHaveLength(columns)
  })

  it('keeps tool output aligned under the verb at a wide width', () => {
    const budget = budgetFor({ columns: 300, rows: 24 })
    const text = 'x'.repeat(200)
    const rendered = show(present({ kind: 'tool-result', callId: 'c', ok: true, text }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />), 300)

    // The outcome, then one line. 200 characters fit after the verb.
    expect(rendered.split('\n')).toHaveLength(2)
  })

  it('remeasures the same line when its verb moves into and out of the body', () => {
    const line: PresentedLine = {
      marker: MARKER.none, verb: 'note', text: 'abcdefgh', column: COLUMN.output, tone: 'plain',
      spans: [{ length: 4, tone: 'strong' }],
    }
    // Both layouts leave four text columns; only the narrow one puts the verb in them.
    for (const columns of [13, 6, 13]) {
      const budget = budgetFor({ columns, rows: 24 })
      const measured = wrappedRows(line, budget)
      expect(measured).toEqual(columns === 6 ? ['note', 'abcd', 'efgh'] : ['abcd', 'efgh'])
      const rendered = show([<Line key="reused" line={line} budget={budget} frame="classic" />], columns)
      expect(rendered).toBe(columns === 6 ? '  note\n  abcd\n  efgh' : '  note   abcd\n         efgh')
      const offset = columns === 6 ? COLUMN.rail : COLUMN.output
      expect(rendered.split('\n').map(row => row.slice(offset))).toEqual(measured)
    }
  })

  it('remeasures the same prose line when only its reading measure changes', () => {
    const line: PresentedLine = {
      marker: MARKER.none, verb: '', text: 'abcd efgh', column: COLUMN.rail, tone: 'plain',
      spans: [{ length: 4, tone: 'strong' }],
    }
    for (const measure of [9, 4, 9]) {
      const budget = { ...budgetFor({ columns: 20, rows: 24 }), measure }
      const measured = wrappedRows(line, budget)
      expect(measured).toEqual(measure === 4 ? ['abcd', 'efgh'] : ['abcd efgh'])
      const rendered = show([<Line key="reused" line={line} budget={budget} frame="classic" />], budget.columns)
      expect(rendered).toBe(measure === 4 ? '  abcd\n  efgh' : '  abcd efgh')
      expect(rendered.split('\n').map(row => row.slice(COLUMN.rail))).toEqual(measured)
    }
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
      .map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />), columns)
    expect(rendered.split('\n').every(line => line.length <= columns)).toBe(true)
    expect(rendered).toContain(`  ${'-'.repeat(budget.measure)}`)
    expect(rendered).toContain('\n  The configuration')
    expect(rendered).not.toContain('< ')
    expect(rendered).not.toContain('Completed')
    expect(Math.max(...rendered.split('\n').map(line => line.length))).toBeLessThanOrEqual(columns)
  })

  it.each(['round', 'classic'] as const)('draws the turn divider with the %s frame rule', frame => {
    const budget = budgetFor({ columns: 40, rows: 24 })
    const rendered = show(present({ kind: 'user', text: 'Hello' }, shown)
      .map((line, index) => <Line key={index} line={line} budget={budget} frame={frame} />), 40)
    expect(rendered).toContain(`  ${RULE[frame].line.repeat(budget.measure)}`)
  })

  it.each([1, 2, 3, 8, 9, 10, 20])('fits response and result text at %i columns without losing labels', columns => {
    const budget = budgetFor({ columns, rows: 24 })
    const rows = [
      { kind: 'assistant' as const, text: '**Answer** ' + 'x'.repeat(24) },
      { kind: 'tool-result' as const, callId: 'c', ok: false, text: 'result-text' },
    ]
    const rendered = show(rows.flatMap(row => present(row, shown))
      .map((line, index) => <Line key={index} line={line} budget={budget} frame="classic" />), columns)
    expect(rendered.split('\n').every(row => row.length <= columns), rendered).toBe(true)
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('Answer' + 'x'.repeat(24))
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('error')
    expect(rendered.replaceAll('\n', '').replaceAll(' ', '')).toContain('result-text')
  })

  it('keeps failed output on the output column, like the result it is', () => {
    // Colour is asserted in present.test.ts. renderToString emits no ANSI
    // outside a TTY, so a colour assertion here would pass for the wrong reason.
    const failed = { kind: 'tool-result' as const, callId: 'c', ok: false, text: 'boom' }
    const rendered = show(present(failed, shown)
      .map((line, index) => <Line key={index} line={line} budget={at80} frame="classic" />))
    expect(rendered.split('\n')[0]).toBe('  error  [c]  1 lines')
    expect(rendered.split('\n')[1]!.indexOf('boom')).toBe(COLUMN.output)
  })
})

describe('StatusBar', () => {
  const git: GitState = { branch: 'main', detached: false, ahead: 1, behind: 0, staged: 2, modified: 3, untracked: 1, conflicted: 0 }
  /** The report's session: a model with its level, a context reading, a dirty branch, billed totals with a cache hit, and a path. */
  const session = (overrides: Partial<StatusInput> = {}): StatusInput => ({
    model: 'deepseek-official/deepseek-v4-flash', thinkingLevel: 'high', context: { used: 15_200, window: 128_000 }, git,
    usage: { input: 42_300, output: 3_100, cached: 34_500 }, cwd: '~/bake', glyphs: 'unicode', ...overrides,
  })
  /** The row as the chrome draws it: at the draft's column, two cells in. */
  const at = (columns: number, input: StatusInput = session()): string => strip(renderToString(
    <StatusBar fields={statusFields(input, dictionaries.en)} columns={columns - 2} />, { columns: columns - 2 })).trimEnd()

  it('names the model without a label, and reads each field as a lowercase word beside its value', () => {
    expect(at(120)).toBe('deepseek-v4-flash  think high  ctx ~11% (15.2k/128k)  \u2387 main +2 ~3 ?1 \u21911  in 42.3k  out 3.1k  cache hit 81%  ~/bake')
  })

  it('gives way in its own order: the absolute count, the totals, the path, then the git counts', () => {
    // The report's rows at 80 and 60 columns: the cache hit outlasts the totals.
    expect(at(80)).toBe('deepseek-v4-flash  think high  ctx ~11%  \u2387 main +2 ~3 ?1 \u21911  cache hit 81%')
    expect(at(60)).toBe('deepseek-v4-flash  think high  ctx ~11%  \u2387 main  ~/bake')
    // Then the cache hit, the branch, and the thinking level; the model is cut last, and never below eight cells.
    expect(at(44)).toBe('deepseek-v4-flash  think high  ctx ~11%')
    expect(at(33)).toBe('deepseek-v4-flash  ctx ~11%')
    expect(at(22)).toBe('deepseek-\u2026  ctx ~11%')
    expect(at(20)).toBe('deepsee\u2026  ctx ~11%')
    for (const columns of [200, 120, 100, 80, 60, 44, 40, 33, 22, 12, 3]) {
      const row = at(columns)
      expect(row.split('\n'), `${columns}`).toHaveLength(1)
      expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns - 2)
    }
  })

  it('keeps a filling context\'s absolute count, and from 90% takes cells from the model for it', () => {
    const warm = session({ context: { used: 99_000, window: 128_000 } })
    // At 77% the count holds past the git counts and the cache hit.
    expect(at(80, warm)).toBe('deepseek-v4-flash  think high  ctx ~77% (99k/128k)  \u2387 main  cache hit 81%')
    expect(at(46, warm)).toBe('deepseek-v4-flash  ctx ~77% (99k/128k)')
    const full = session({ context: { used: 121_400, window: 128_000 } })
    expect(at(80, full)).toBe('deepseek-v4-flash  think high  ctx ~94% (121.4k/128k)  \u2387 main  cache hit 81%')
    // The model is cut, as far as its floor, before the count goes.
    expect(at(38, full)).toBe('deepseek-v4\u2026  ctx ~94% (121.4k/128k)')
    expect(at(34, full)).toBe('deepsee\u2026  ctx ~94% (121.4k/128k)')
    expect(at(33, full)).toBe('deepseek-v4-flash  ctx ~94%')
  })

  it('marks where automatic compaction starts, narrowing the mark before dropping it', () => {
    const marked = session({ context: { used: 79_000, window: 128_000, compactAt: 102_400 } })
    expect(at(140, marked)).toBe('deepseek-v4-flash  think high  ctx ~61% (79k/128k) \u00b7 compacts at 80%  \u2387 main +2 ~3 ?1 \u21911  in 42.3k  out 3.1k  cache hit 81%  ~/bake')
    // The absolute count, then the totals go before the mark's words do.
    expect(at(120, marked)).toBe('deepseek-v4-flash  think high  ctx ~61% \u00b7 compacts at 80%  \u2387 main +2 ~3 ?1 \u21911  cache hit 81%  ~/bake')
    expect(at(80, marked)).toBe('deepseek-v4-flash  think high  ctx ~61% \u25b880%  \u2387 main  cache hit 81%  ~/bake')
    expect(at(60, marked)).toBe('deepseek-v4-flash  think high  ctx ~61% \u25b880%  \u2387 main')
    expect(at(50, marked)).toBe('deepseek-v4-flash  think high  ctx ~61%  \u2387 main')
    // Past the mark, the next request compacts first.
    expect(at(140, session({ context: { used: 104_000, window: 128_000, compactAt: 102_400 } })))
      .toContain('ctx ~81% (104k/128k) \u00b7 compacts next')
    // Without a threshold there is no mark.
    expect(at(140)).not.toContain('\u25b8')
  })

  it('lets the totals go whole when the provider reports no cache traffic', () => {
    const uncached = session({ usage: { input: 900, output: 100 } })
    expect(at(120, uncached)).toBe('deepseek-v4-flash  think high  ctx ~11% (15.2k/128k)  \u2387 main +2 ~3 ?1 \u21911  in 900  out 100  ~/bake')
    expect(at(80, uncached)).not.toContain(' in ')
  })

  it.each(['en'] as const)('keeps one row whatever the width in %s', async locale => {
    const scenes: readonly [string, StatusInput][] = [
      ['idle', session()],
      ['update', session({ update: { version: '0.2.0', installed: false } })],
      ['filling', session({ context: { used: 99_000, window: 128_000, compactAt: 102_400 } })],
      ['full', session({ context: { used: 121_400, window: 128_000 } })],
      ['fresh', { model: 'deepseek-official/deepseek-v4-flash', thinkingLevel: 'high', cwd: '/tmp/bake-ui-audit/ws', glyphs: 'unicode' }],
    ]
    const frames = scenes.flatMap(([name, input]) => [120, 80, 60, 40, 24, 12].map(columns => {
      const row = at(columns, input)
      expect(row.split('\n'), `${name} ${columns}`).toHaveLength(1)
      expect(stringWidth(row), `${name} ${columns}`).toBeLessThanOrEqual(columns - 2)
      return `${name} ${columns}: ${row}`
    }))
    await expect(frames.join('\n') + '\n').toMatchFileSnapshot(`./expected/status.${locale}.txt`)
  })

  it('fills what is left with the path, cut from its start to keep the workspace, and leaves out a tail too short to name it', () => {
    const path = '/private/var/folders/jg/zcyzdbb13bnfr5q882y_h8_r0000gn/T/dsh-tui-pty-1B3VG9/workspace'
    const input = session({ git: undefined, usage: undefined, cwd: path })
    const wide = at(120, input)
    expect(wide.endsWith('workspace')).toBe(true)
    expect(wide).not.toContain('/private/var/folders')
    // A deep path costs nothing else while a few cells of it fit: it is cut instead.
    expect(wide).toMatch(/^deepseek-v4-flash {2}think high {2}ctx ~11% \(15\.2k\/128k\) {2}\u2026/)
    expect(at(70, input)).toBe('deepseek-v4-flash  think high  ctx ~11% (15.2k/128k)  \u2026VG9/workspace')
    // Below that, the readings ranked before it give way to keep its few cells, then it goes.
    expect(at(48, input)).toBe('deepseek-v4-flash  think high  ctx ~11%')
    expect(at(50, input)).toBe('deepseek-v4-flash  think high  ctx ~11%  \u2026kspace')
  })

  it('narrows the git field to its branch before dropping it, and never cuts it', () => {
    const branch = { ...git, branch: 'feature/status' }
    const input = session({ git: branch, thinkingLevel: undefined, context: undefined, usage: undefined, cwd: '~/workspace' })
    expect(at(80, input)).toBe('deepseek-v4-flash  \u2387 feature/status +2 ~3 ?1 \u21911  ~/workspace')
    // The path is a filler once it yields: cut from its start into what is left.
    expect(at(60, input)).toBe('deepseek-v4-flash  \u2387 feature/status +2 ~3 ?1 \u21911  \u2026orkspace')
    expect(at(50, input)).toBe('deepseek-v4-flash  \u2387 feature/status +2 ~3 ?1 \u21911')
    // The counts qualify the branch, so they go first, together, and the branch stays.
    expect(at(44, input)).toBe('deepseek-v4-flash  \u2387 feature/status')
    expect(at(30, input)).toBe('deepseek-v4-flash  \u2026orkspace')
  })

})

describe('Chrome', () => {
  const hints = { send: 'enter to send', interrupt: 'esc interrupts', select: 'up/down to select', answer: 'y or n' }
  // No `drafting` here. Chrome reads it from the draft, so the two cannot disagree.
  const idle = { running: false, asking: false, listing: false }
  const working: ActivityState = { kind: 'running', word: 'Kneading', phase: 'writing', startedAt: 0, color: PALETTE.running }
  const ended: ActivityState = { kind: 'ended', summary: { outcome: 'done', label: 'Completed', details: '9s · ran 1', brief: '9s' } }
  const goal: StandingState = { glyph: '\u25cf', label: 'Goal', count: '2/8', details: '', compact: '2/8', note: 'Ship it', color: PALETTE.running }
  const draw = (columns: number, options: {
    readonly text?: string, readonly state?: typeof idle, readonly frame?: FrameStyle
    readonly activity?: ActivityState, readonly standing?: StandingState, readonly rows?: number, readonly children?: React.ReactNode
  } = {}): string[] => strip(renderToString(
    <Chrome
      status={statusFields({ model: 'deepseek/chat', context: { used: 15_360, window: 128_000 }, cwd: '', glyphs: 'unicode' }, dictionaries.en)} columns={columns}
      state={options.state ?? idle} before={options.text ?? ''} after="" placeholder="Ask anything" hints={hints}
      frame={options.frame ?? 'round'} activity={options.activity} standing={options.standing}
      {...options.rows === undefined ? {} : { layout: chromeFor(columns, options.rows) }}
    >{options.children}</Chrome>, { columns })).split('\n')
  const at = (columns: number, text = '', state = idle): string => draw(columns, { text, state }).join('\n')
  const render = (state: typeof idle, text = ''): string => at(60, text, state)
  const line = (columns: number) => '\u2500'.repeat(columns)

  it('spends a blank, the header, the framed draft, and the status line, and nothing else', () => {
    const rows = render(idle).split('\n')
    // CHROME_ROWS is charged against every other region's budget on every
    // frame, so the floor is measured here instead of assumed.
    expect(rows).toHaveLength(CHROME_ROWS)
    // A blank opens the chrome. Without it the header sits directly under the
    // last line of the answer and looks like part of that answer.
    expect(rows[0]).toBe('')
    // With nothing to say the header keeps its row, blank.
    expect(rows[1]!.trim()).toBe('')
    // Two bare rules frame the draft, and the status line sits directly under
    // the lower one, with no blank row between them.
    expect(rows[2]).toBe(line(60))
    expect(rows[3]).toBe(`${MARKER.prompt} ${CARET}Ask anything`)
    expect(rows[4]).toBe(line(60))
    expect(rows[5]).toBe('  chat  ctx ~12% (15.4k/128k)')
    expect(rows[3]!.indexOf(CARET)).toBe(rows[5]!.indexOf('chat'))
  })

  it('puts the running work and the last turn in the header, from the rail’s first column', () => {
    const running = draw(60, { activity: working, state: { ...idle, running: true } })
    expect(running[1]).toBe(`${SPINNER_REST} Kneading…  writing`)
    // Level with an action's marker, not the draft's column.
    expect(running[1]!.indexOf(SPINNER_REST)).toBe(0)
    const done = draw(60, { activity: ended })
    expect(done[1]).toBe('✓ Completed  9s · ran 1')
    // The rules stay bare whatever the header says.
    for (const rows of [running, done]) expect([rows[2], rows[4]]).toEqual([line(60), line(60)])
    // One row whatever it says, so the input never moves between them.
    expect(running).toHaveLength(CHROME_ROWS)
    expect(done).toHaveLength(CHROME_ROWS)
  })

  it('puts compaction in the same cells, resting on its folded block instead of the kneaded ball', () => {
    const compacting: ActivityState = {
      kind: 'running', word: 'Compacting history', phase: 'summarizing', startedAt: 0, color: PALETTE.compacting, spinner: 'fold',
    }
    const rows = draw(60, { activity: compacting, state: { ...idle, running: true } })
    expect(rows[1]).toBe(`${FOLD_REST} Compacting history…  summarizing`)
    expect(rows[1]!.indexOf(FOLD_REST)).toBe(0)
    // The word starts where the turn's does, so neither moves when one replaces the other.
    const turn = draw(60, { activity: working, state: { ...idle, running: true } })[1]!
    expect(rows[1]!.indexOf('Compacting')).toBe(turn.indexOf('Kneading'))
    expect(rows).toHaveLength(CHROME_ROWS)
    // A state that names no dough kneads.
    expect(draw(60, { activity: { kind: 'running', word: 'Compacting history', phase: undefined, startedAt: 0, color: PALETTE.compacting } })[1])
      .toMatch(new RegExp(`^${SPINNER_REST} `))
  })

  it('puts the goal at the header\'s right edge, beside the turn', () => {
    const alone = draw(60, { standing: goal })[1]!
    expect(alone).toMatch(/^ +● Goal 2\/8 {2}Ship it$/)
    expect(stringWidth(alone)).toBe(60)
    const both = draw(80, { activity: working, standing: goal })[1]!
    expect(both).toMatch(new RegExp(`^${SPINNER_REST} Kneading… {2}writing +● Goal 2/8 {2}Ship it$`))
    expect(stringWidth(both)).toBe(80)
  })

  it('narrows the goal part by part, never mid-word, before the turn\'s word', () => {
    const long = { ...goal, note: 'Refactor the persistence layer' }
    // Only the note is cut with an ellipsis.
    expect(draw(60, { activity: working, standing: long })[1]).toMatch(/writing {2}● Goal 2\/8 {2}Refactor.*…$/)
    expect(draw(44, { activity: working, standing: long })[1]).toMatch(/writing +● Goal 2\/8$/)
    // The turn's phase yields before the goal loses its label; the count stays with the label.
    expect(draw(30, { activity: working, standing: goal })[1]).toMatch(new RegExp(`^${SPINNER_REST} Kneading… +● Goal 2/8$`))
    expect(draw(22, { activity: working, standing: goal })[1]).toMatch(new RegExp(`^${SPINNER_REST} Kneading… +● 2/8$`))
    expect(draw(16, { activity: working, standing: goal })[1]).toMatch(new RegExp(`^${SPINNER_REST} Kneading… +●$`))
    for (const columns of [1, 2, 5, 12, 16, 24, 40, 80]) {
      expect(stringWidth(draw(columns, { activity: working, standing: long })[1]!), `${columns}`).toBeLessThanOrEqual(columns)
    }
  })

  it('gives up the goal\'s shortcut after its note and before the turn\'s phase, and below 60 columns', () => {
    const keyed = { ...goal, key: 'Ctrl+O' }
    expect(draw(80, { activity: working, standing: keyed })[1]).toMatch(/writing +Ctrl\+O ● Goal 2\/8 {2}Ship it$/)
    expect(draw(72, { activity: working, standing: { ...keyed, note: 'Refactor the persistence layer' } })[1])
      .toMatch(/writing {2}Ctrl\+O ● Goal 2\/8 {2}Refactor .*…$/)
    const long: ActivityState = { ...working, phase: 'running a-rather-long-tool-name' }
    expect(draw(66, { activity: long, standing: keyed })[1]).toMatch(/a-rather-long-tool-name {2,}Ctrl\+O ● Goal 2\/8$/)
    expect(draw(62, { activity: long, standing: keyed })[1]).toMatch(/a-rather-long-tool-name {2,}● Goal 2\/8$/)
    // Below the composer hint's width the key goes whatever room is left, as every standing row's does.
    expect(draw(60, { activity: working, standing: keyed })[1]).toContain('Ctrl+O')
    expect(draw(59, { activity: working, standing: keyed })[1]).toMatch(/writing +● Goal 2\/8 {2}Ship it$/)
  })

  it('gives up an ended turn\'s counts and rate first, and its time before the goal\'s name', () => {
    const summary: ActivityState = { kind: 'ended', summary: {
      outcome: 'done', label: 'Completed', details: '1m 12s · edited 2 · ran 3 · read 4 · 1 failed · 42 tok/s', brief: '1m 12s' } }
    const { note: _note, ...quiet } = goal
    const keyed = { ...quiet, key: 'Ctrl+O' }
    const header = (columns: number): string => draw(columns, { activity: summary, standing: keyed })[1]!
    expect(header(90)).toMatch(/^✓ Completed {2}1m 12s · edited 2 · ran 3 · read 4 · 1 failed · 42 tok\/s +Ctrl\+O ● Goal 2\/8$/)
    expect(header(80)).toMatch(/^✓ Completed {2}1m 12s +Ctrl\+O ● Goal 2\/8$/)
    expect(header(31)).toMatch(/^✓ Completed {2}1m 12s +● Goal 2\/8$/)
    expect(header(30)).toMatch(/^✓ Completed +● Goal 2\/8$/)
    expect(header(20)).toMatch(/^✓ Completed +● 2\/8$/)
    expect(header(14)).toBe('✓ Completed  ●')
    // A held goal has no count to keep, so its words go before it does.
    const held: StandingState = { glyph: '○', label: 'Goal on hold', details: '/goal resume continues', color: PALETTE.waiting, key: 'Ctrl+O' }
    const waiting = (columns: number): string => draw(columns, { activity: summary, standing: held })[1]!
    expect(waiting(70)).toMatch(/^✓ Completed {2}1m 12s +Ctrl\+O ○ Goal on hold {2}\/goal resume continues$/)
    expect(waiting(40)).toMatch(/^✓ Completed +○ Goal on hold$/)
    // Too narrow for its name, the held goal goes whole and the turn takes the row.
    expect(waiting(24)).toBe('✓ Completed  1m 12s · e…')
  })

  it('draws the rules in ASCII where the terminal cannot draw box characters', () => {
    const rows = draw(60, { frame: 'classic', activity: working })
    expect([rows[2], rows[4]]).toEqual(['-'.repeat(60), '-'.repeat(60)])
    expect(isRenderable(rows[2]!) && isRenderable(rows[4]!)).toBe(true)
  })

  it('draws panels between the opening blank and the header', () => {
    const rows = draw(60, { children: <Text>notice</Text>, activity: ended })
    expect(rows[0]).toBe('')
    expect(rows[1]).toBe('notice')
    expect(rows[2]).toMatch(/^✓ Completed/)
    expect(rows[3]).toBe(line(60))
    expect(rows[4]!.startsWith(`${MARKER.prompt} `)).toBe(true)
  })

  it('keeps its height when only the width changes', () => {
    // From the width a five-letter draft fits in without wrapping.
    const heights = [8, 12, 19, 20, 24, 39, 40, 60, 200].map(columns => at(columns, 'hello').split('\n').length)
    expect(new Set(heights)).toEqual(new Set([CHROME_ROWS]))
  })

  it('yields the gap, the base rule, the rule, the status line, then the header, and never the draft', () => {
    const at = (rows: number): string[] => draw(60, { text: 'hello', rows, activity: ended })
    const header = expect.stringMatching(/^✓/)
    const draft = expect.stringContaining('hello')
    const status = '  chat  ctx ~12% (15.4k/128k)'
    expect(at(6)).toHaveLength(6)
    expect(at(5)).toEqual([header, line(60), draft, line(60), status])
    expect(at(4)).toEqual([header, line(60), draft, status])
    expect(at(3)).toEqual([header, draft, status])
    expect(at(2)).toEqual([header, draft])
    expect(at(1)).toEqual([draft])
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
      for (const activity of [undefined, working, ended]) {
        for (const row of draw(columns, { text: 'hello', standing: goal, ...activity === undefined ? {} : { activity } })) {
          expect(stringWidth(row), `${columns}`).toBeLessThanOrEqual(columns)
        }
      }
    }
  })

  it('drops the hint rather than truncating it to something that is not help', () => {
    expect(at(HINT_MIN_COLUMNS, 'hello')).toContain('enter to send')
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).not.toContain('enter to send')
    // What is left is still the draft, whole. The hint yields, the input does not.
    expect(at(HINT_MIN_COLUMNS - 1, 'hello')).toContain('hello')
  })
})

describe('header fitting', () => {
  // `K ` is 2 cells, `● Goal` 6, ` 1/2` 4, `  objective text` 16.
  const state: StandingState = { glyph: '●', label: 'Goal', count: '1/2', details: '', compact: '1/2', note: 'objective text', color: PALETTE.running, key: 'K' }
  const shape = (room: number, standing = state) => {
    const fit = fitStanding(room, standing)
    return fit === undefined ? undefined : [fit.key, fit.label, fit.compact, fit.details, fit.note, fit.width]
  }

  it('drops the standing state\'s parts in order and never leaves a fragment', () => {
    expect(shape(28)).toEqual([true, true, false, true, true, 28])
    // Cut while enough of the note would still read.
    expect(shape(23)).toEqual([true, true, false, true, true, 23])
    expect(shape(22)).toEqual([true, true, false, true, false, 12])
    // The count goes with the label, never before it.
    expect(shape(11)).toEqual([false, true, false, true, false, 10])
    expect(shape(9)).toEqual([false, false, true, false, false, 5])
    expect(shape(4)).toEqual([false, false, false, false, false, 1])
    expect(shape(0)).toBeUndefined()
    // A state with words but no count keeps its name, then goes whole.
    const held: StandingState = { glyph: '○', label: 'Held', details: 'resume', color: PALETTE.waiting }
    expect(shape(14, held)).toEqual([true, true, false, true, false, 14])
    expect(shape(13, held)).toEqual([false, true, false, false, false, 6])
    expect(shape(5, held)).toBeUndefined()
  })

  it('keeps the turn\'s word first, giving up its counts before the shortcut and its phase before the label', () => {
    const turn = { full: 20, brief: 15, head: 10 }
    expect(headerLayout(40, turn, undefined)).toEqual({ left: 20, level: 'full', right: undefined })
    const at = (columns: number) => {
      const layout = headerLayout(columns, turn, state)
      return [layout.left, layout.level, layout.right?.key, layout.right?.label, layout.right?.compact]
    }
    expect(at(40)).toEqual([20, 'full', true, true, false])
    expect(at(30)).toEqual([15, 'brief', true, true, false])
    expect(at(26)).toEqual([10, 'head', false, true, false])
    expect(at(17)).toEqual([10, 'head', false, false, true])
    expect(at(16)).toEqual([10, 'head', false, false, false])
    expect(headerLayout(8, turn, state)).toEqual({ left: 8, level: 'full', right: undefined })
  })
})

describe('Composer width and wrapping', () => {
  const draft = (text: string, columns: number, maxRows?: number): string[] => strip(renderToString(
    <Composer
      columns={columns} marker={MARKER.prompt} before={text} after="" placeholder="Ask"
      hint="Enter sends" {...maxRows === undefined ? {} : { maxRows }}
    />, { columns })).split('\n')

  it('bounds a single pasted line that wraps to more rows than it is lines', () => {
    // The regression this guards. `maxRows` counted lines, and a line is not a
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
    // The caret is only an attribute on a cell, so without styling every row reads the same.
    const rows = (cursor: number): string[] => renderToString(
      <Composer
        columns={80} marker={MARKER.prompt} before={text.slice(0, cursor)} after={text.slice(cursor)} placeholder="Ask"
        hint="Enter sends"
      />, { columns: 80 }).replace(/\u001B\[[0-9;]*m/g, '').split('\n').map(row => row.replace('Enter sends', '').trimEnd())
    const expected = rows(text.length)
    for (const cursor of [0, 7, 40, 101]) expect(rows(cursor)).toEqual(expected)
  })

  it('lets the caret take the column before the hint when its row is full', () => {
    // 40 columns. Two of rail, thirteen of hint and its gap, so 25 of text and the caret.
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
  it.each(['น้ำ', 'ກຳ'])('keeps the hint aligned after the two-cell cluster %s', cluster => {
    const rendered = strip(renderToString(
      <Composer columns={16} marker={MARKER.prompt} before={cluster.repeat(5)} after="" placeholder="Ask" hint="hint" />,
      { columns: 16 }))
    expect(rendered.split('\n')).toEqual([`> ${cluster.repeat(4)}`, `  ${cluster}${CARET}       hint`])
  })

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

it.each([['น้ำใจ', 'น้ำ'], ['ກຳລາ', 'ກຳ']])('truncates %s by terminal cells without splitting a grapheme', (text, cluster) => {
  for (const columns of [1, 2, 3]) {
    expect(strip(renderToString(<Text wrap="truncate-end" bold>{text}</Text>, { columns })))
      .toBe(columns < 3 ? '…' : `${cluster}…`)
  }
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

describe('Composer window markers', () => {
  const overflow = { above: 'above', below: 'below' }
  const text = Array.from({ length: 12 }, (_, index) => `line ${index}`).join('\n')
  const draw = (cursor: number, columns = 80, hint: string | null = 'Enter sends'): string[] => strip(renderToString(
    <Composer columns={columns} marker={MARKER.prompt} before={text.slice(0, cursor)} after={text.slice(cursor)}
      placeholder="Ask" maxRows={5} overflow={overflow} {...hint === null ? {} : { hint }} />, { columns })).split('\n')

  it('marks rows hidden above with ^ and counts them where the hint is not', () => {
    const rows = draw(text.length)
    expect(rows).toHaveLength(5)
    expect(rows[0]).toMatch(/^\^ line 7 +\+7 above$/)
    // The caret's row carries the hint, and nothing is hidden below it.
    expect(rows.at(-1)).toMatch(/^ {2}line 11▌ +Enter sends$/)
    // The count ends where the hint does.
    expect(stringWidth(rows[0]!)).toBe(80)
    expect(rows.some(row => row.startsWith('v'))).toBe(false)
  })

  it('marks rows hidden below with v, and counts them where the hint is not', () => {
    const top = draw(0)
    expect(top[0]).toMatch(/^> ▌line 0 +Enter sends$/)
    expect(top.at(-1)).toMatch(/^v line 4 +\+7 below$/)
    // The mark stands before the character the caret covers and takes no cell on screen.
    for (const row of top) expect(stringWidth(row.replace(CARET, ''))).toBeLessThanOrEqual(80)
  })

  it('moves the window only when the caret would leave it', async () => {
    const at = (cursor: number) => <Composer columns={80} marker={MARKER.prompt} before={text.slice(0, cursor)}
      after={text.slice(cursor)} placeholder="Ask" maxRows={5} overflow={overflow} hint="Enter sends" />
    const ui = render(at(text.length))
    const rows = () => strip(ui.lastFrame() ?? '').split('\n')
    expect(rows()[0]).toMatch(/^\^ line 7 /)
    // Up to line 9 stays inside the window, which holds still.
    ui.rerender(at(text.indexOf('line 9')))
    await vi.waitFor(() => expect(rows()[2]).toMatch(/^ {2}▌line 9 +Enter sends$/))
    expect(rows()[0]).toMatch(/^\^ line 7 +\+7 above$/)
    // Past its top the window follows, and now hides rows both ways.
    ui.rerender(at(text.indexOf('line 5')))
    await vi.waitFor(() => expect(rows()[0]).toMatch(/^\^ ▌line 5 +Enter sends$/))
    expect(rows().at(-1)).toMatch(/^v line 9 +\+2 below$/)
    ui.unmount()
  })

  it('keeps the rail markers and leaves out a count that does not fit the slot', () => {
    // The Chrome drops the hint below HINT_MIN_COLUMNS, and the counts go with its slot.
    const bare = draw(0, 40, null)
    expect(bare[0]).toBe('> ▌line 0')
    expect(bare.at(-1)).toBe('v line 4')
    expect(draw(text.length, 40, null)[0]).toBe('^ line 7')
    // A count wider than the hint would push the row past the terminal's width.
    const narrow = draw(text.length, 80, 'go')
    expect(narrow[0]).toBe('^ line 7')
    expect(narrow.at(-1)).toMatch(/^ {2}line 11▌ +go$/)
  })

  it('counts rows of a wrapped paragraph, not lines', () => {
    const paragraph = 'word '.repeat(120).trim()
    const rows = strip(renderToString(<Composer columns={60} marker={MARKER.prompt} before="" after={paragraph}
      placeholder="Ask" hint="Enter sends" overflow={overflow} />, { columns: 60 })).split('\n')
    expect(rows).toHaveLength(COMPOSER_BUDGET)
    expect(rows[0]!.startsWith(`${MARKER.prompt} ${CARET}word`)).toBe(true)
    expect(rows.at(-1)).toMatch(/^v word.* \+\d+ below$/)
  })
})

describe('Composer placeholder parts', () => {
  const parts = [dictionaries.en.prompt, dictionaries.en.promptCommands, dictionaries.en.promptFiles]
  const empty = (columns: number, placeholder: string | readonly string[] = parts): string => strip(renderToString(
    <Composer columns={columns} marker={MARKER.prompt} before="" after="" placeholder={placeholder} />, { columns })).trimEnd()

  it('names commands and files beside the prompt on an ordinary terminal', () => {
    expect(empty(80)).toBe(`${MARKER.prompt} ${CARET}Ask anything · / commands · @ files`)
    expect(empty(HINT_MIN_COLUMNS)).toBe(`${MARKER.prompt} ${CARET}Ask anything · / commands · @ files`)
  })

  it('drops the later parts whole below the hint threshold, or where they do not fit', () => {
    expect(empty(HINT_MIN_COLUMNS - 1)).toBe(`${MARKER.prompt} ${CARET}Ask anything`)
    expect(empty(40)).toBe(`${MARKER.prompt} ${CARET}Ask anything`)
    // 77 cells are free after the rail and the caret; the second part would take 78.
    expect(empty(80, ['Ask', 'x'.repeat(72), 'short'])).toBe(`${MARKER.prompt} ${CARET}Ask`)
    expect(empty(80, ['Ask', 'x'.repeat(71)])).toBe(`${MARKER.prompt} ${CARET}Ask · ${'x'.repeat(71)}`)
    // A plain string is drawn as it is, as the running and blocked placeholders are.
    expect(empty(80, 'Enter steers the next step')).toBe(`${MARKER.prompt} ${CARET}Enter steers the next step`)
  })
})
