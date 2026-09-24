/** The opening block a fresh session prints above its first prompt. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderToString } from 'ink'
import stringWidth from 'string-width'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'
import { Welcome, WELCOME_WIDTH } from '../src/welcome.tsx'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test', version: '1.2.3',
    copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

/** Rows between the welcome card's bottom edge and the rule over the composer. */
function gapUnderCard(frame: string): number | undefined {
  const rows = frame.split('\n')
  const card = rows.findIndex(line => line.startsWith('╭'))
  const border = rows.findIndex((line, index) => index > card && line.startsWith('╰'))
  const prompt = rows.findIndex((line, index) => index > border && line.startsWith('> '))
  return border < 0 || prompt < 0 ? undefined : prompt - 1 - border - 1
}

describe('welcome block', () => {
  it.each(['en', 'zh'] as const)('opens a fresh session with the version, session, and example commands in %s', async locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy })} />)
    const frame = ui.lastFrame() ?? ''
    expect(frame).toContain('BAKE  v1.2.3')
    expect(frame).toContain(`${copy.session}: session-test`)
    expect(frame).toMatch(/\/help +\S/)
    expect(frame).toMatch(/\/changelog +\S/)
    // Inside the block, so the heading is not printed a second time above it.
    expect(frame.indexOf('BAKE')).toBeLessThan(frame.indexOf(`${copy.session}: session-test`))
    // The block replaces the heading rather than preceding it: the chrome's one
    // gap row is all that separates the card from the composer.
    expect(gapUnderCard(frame)).toBe(1)
    await expect(frame + '\n').toMatchFileSnapshot(`./expected/welcome.${locale}.txt`)
  })

  it('stays out of a session that opens with history', () => {
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Earlier prompt' }])
    const frame = render(<App {...props({ committed })} />).lastFrame() ?? ''
    expect(frame).not.toContain('BAKE')
    expect(frame).toContain('Session: session-test')
  })

  it('stays out when the application supplies no version', () => {
    const frame = render(<App {...props({ version: undefined })} />).lastFrame() ?? ''
    expect(frame).not.toContain('BAKE')
    expect(frame).toContain('Session: session-test')
  })

  it('prints once while the first turn commits under it', async () => {
    const ui = render(<App {...props()} />)
    ui.rerender(<App {...props({ committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'First prompt' }]) })} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('First prompt'))
    const frame = ui.lastFrame() ?? ''
    expect(frame.split('BAKE').length - 1).toBe(1)
    expect(frame.split('Session: session-test').length - 1).toBe(1)
    expect(frame.indexOf('Session: session-test')).toBeLessThan(frame.indexOf('First prompt'))
  })

  it('opens another welcome block when switching to a new empty session', async () => {
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Earlier prompt' }])
    const ui = render(<App {...props({ committed })} />)
    ui.rerender(<App {...props({ sessionId: 'session-new' })} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Session: session-new'))
    expect(ui.lastFrame()).toContain('BAKE  v1.2.3')
  })

  it('does not introduce a welcome block into an empty child inspection', () => {
    const frame = render(<App {...props({ inspectionParent: 'parent', inputBlocked: true })} />).lastFrame() ?? ''
    expect(frame).not.toContain('BAKE')
    expect(frame).toContain('Parent: parent > session-test')
  })

  it('uses the ASCII frame when requested', () => {
    const frame = renderToString(<Welcome version="1.2.3" heading="Session: session-test" copy={dictionaries.en} frame="classic" columns={64} />, { columns: 64 })
    expect(frame).not.toContain('╭')
    expect(frame.split('\n')[0]).toMatch(/^\+-+\+$/)
    expect(frame).toContain('BAKE  v1.2.3')
  })

  it.each(['en', 'zh'] as const)('keeps Box borders and command columns aligned in %s', locale => {
    for (const columns of [8, 12, 24, 40, 64, 100]) {
      const copy = dictionaries[locale]
      const frame = renderToString(<Welcome version="1.2.3" heading={`${copy.session}: session-test`} copy={copy} frame="round" columns={columns} />, { columns })
      const width = Math.min(columns, WELCOME_WIDTH)
      const lines = frame.split('\n')
      for (const line of lines) expect(stringWidth(line)).toBeLessThanOrEqual(width)
      if (columns >= 40) {
        for (const line of lines.filter(line => line.startsWith('│'))) {
          expect(line.endsWith('│')).toBe(true)
          expect(stringWidth(line)).toBe(width)
        }
        const help = lines.find(line => line.includes('/help'))!
        const changes = lines.find(line => line.includes('/changelog'))!
        expect(help.indexOf(copy.welcomeHelp)).toBe(changes.indexOf(copy.welcomeChangelog.slice(0, 3)))
      }
    }
  })

  it('keeps a fixed height, drawing its frame only where the composer does', () => {
    const wide = renderToString(<Welcome version="1.2.3" heading="Session: session-test" copy={dictionaries.en} frame="round" columns={120} />, { columns: 120 })
    const narrow = renderToString(<Welcome version="1.2.3" heading="Session: session-test" copy={dictionaries.en} frame="round" columns={30} />, { columns: 30 })
    const wideRows = wide.split('\n')
    expect(wideRows[0]).toMatch(/^╭─+╮$/)
    expect(wideRows[0]!.length).toBe(WELCOME_WIDTH)
    for (const row of wideRows.filter(row => row.startsWith('│'))) expect(row.length).toBe(WELCOME_WIDTH)
    expect(narrow).not.toContain('╭')
    // Same content rows, less the two border rows.
    expect(narrow.split('\n').length).toBe(wideRows.length - 2)
    for (const row of narrow.split('\n')) expect(row.length).toBeLessThanOrEqual(30)
  })
})
