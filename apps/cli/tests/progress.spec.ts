/** Terminal install progress: a log of finished steps under one live row, owning its clock and leaving diagnostics readable. */
import { afterEach, expect, it, vi } from 'vitest'
import { PROGRESS_FRAME_MS } from 'bake-tui-ui/install-progress.ts'
import { startProgress } from '../src/progress.ts'

afterEach(() => vi.useRealTimers())

const UNICODE = { LANG: 'en_US.UTF-8', TERM: 'xterm-256color' }
const plain = (text: string): string => text.replace(/\u001b\[[\d;]*m/gu, '')
/** What the terminal shows: each carriage return redraws its row, and each newline keeps it. */
const screen = (writes: readonly string[]): string[] => {
  const rows: string[] = ['']
  for (const chunk of plain(writes.join('')).split(/(\r\u001b\[2K|\n)/u)) {
    if (chunk === '\r\u001b[2K') rows[rows.length - 1] = ''
    else if (chunk === '\n') rows.push('')
    else rows[rows.length - 1] += chunk
  }
  return rows
}

it('opens with a heading, prints each finished step once under the live row, and closes with the summary', () => {
  vi.useFakeTimers()
  const writes: string[] = []
  const progress = startProgress({ isTTY: true, columns: 100, write: text => writes.push(text) }, UNICODE, ['installer', 'linux-x64'])
  expect(progress.animated).toBe(true)
  progress.step('Verifying release', 'Verified release')
  vi.advanceTimersByTime(PROGRESS_FRAME_MS * 3)
  // The live row is redrawn in place, never with a newline.
  const live = writes.slice(1)
  expect(live.length).toBeGreaterThan(2)
  expect(live.every(text => text.startsWith('\r\u001b[2K') && !text.includes('\n'))).toBe(true)
  progress.note('v0.2.0', 'signed')
  progress.step('Downloading', 'Downloaded')
  progress.progress(0.5, '9.2 / 18.4 MB')
  vi.advanceTimersByTime(PROGRESS_FRAME_MS)
  expect(plain(writes.at(-1)!)).toMatch(/Downloading {9}[\u2501\u2578\u2500]+ {3}50% {2}9\.2 \/ 18\.4 MB/u)
  progress.note('18.4 MB')
  progress.finish({ title: 'Bake 0.2.0 installed', next: ['Run: bake'] })
  expect(screen(writes)).toEqual([
    '  BAKE  installer \u00b7 linux-x64', '',
    '  \u2713  Verified release    v0.2.0 \u00b7 signed \u00b7 0.2s',
    '  \u2713  Downloaded          18.4 MB \u00b7 <0.1s',
    '', '  Bake 0.2.0 installed in 0.3s', '  Run: bake', '',
  ])
  // Nothing after the end, from any call or the clock.
  const finished = writes.join('')
  progress.step('Late', 'Late')
  progress.fail()
  progress.stop()
  vi.advanceTimersByTime(2000)
  expect(writes.join('')).toBe(finished)
  expect(vi.getTimerCount()).toBe(0)
})

it('marks the running step failed before the caller\'s diagnostics, with no summary', () => {
  vi.useFakeTimers()
  const writes: string[] = []
  const progress = startProgress({ isTTY: true, columns: 80, write: text => writes.push(text) }, { ...UNICODE, NO_COLOR: '1' }, ['installer'])
  progress.step('Checking SHA-256', 'Checked SHA-256')
  vi.advanceTimersByTime(PROGRESS_FRAME_MS)
  progress.fail()
  expect(screen(writes).at(-2)).toBe('  \u2717  Checking SHA-256')
  expect(writes.join('')).not.toContain('\u001b[3')
  expect(writes.join('')).not.toContain('installed')
  expect(vi.getTimerCount()).toBe(0)
})

it('clears only the live row on stop', () => {
  const writes: string[] = []
  const progress = startProgress({ isTTY: true, columns: 80, write: text => writes.push(text) }, UNICODE, ['update'])
  progress.step('Unpacking', 'Unpacked')
  progress.stop()
  expect(writes.at(-1)).toBe('\r\u001b[2K')
  expect(screen(writes).join('\n')).not.toContain('\u2713')
})

it.each([{ TERM: 'dumb' }, { CI: '1' }, { BAKE_NO_ANIMATION: '1' }])('draws nothing for a static terminal preference %j', (env) => {
  vi.useFakeTimers()
  const write = vi.fn()
  const progress = startProgress({ isTTY: true, write }, env, ['installer'])
  expect(progress.animated).toBe(false)
  progress.step('Downloading', 'Downloaded')
  progress.finish({ title: 'Bake 0.2.0 installed' })
  expect(write).not.toHaveBeenCalled()
  expect(vi.getTimerCount()).toBe(0)
})

it('keeps redirected output free of escape sequences and progress text', () => {
  const write = vi.fn()
  const progress = startProgress({ isTTY: false, write }, UNICODE, ['installer'])
  progress.step('Downloading', 'Downloaded')
  progress.finish({ title: 'Bake 0.2.0 installed' })
  expect(write).not.toHaveBeenCalled()
})

it('fits a narrow terminal after a resize, and draws at a default width when it reports zero columns', () => {
  vi.useFakeTimers()
  const writes: string[] = []
  const terminal = { isTTY: true, columns: 60, write: (text: string) => writes.push(text) }
  const progress = startProgress(terminal, UNICODE, ['installer', 'linux-x64'])
  progress.step('Downloading', 'Downloaded')
  progress.progress(0.3, '5.5 / 18.4 MB')
  terminal.columns = 24
  vi.advanceTimersByTime(PROGRESS_FRAME_MS)
  expect([...plain(writes.at(-1)!).replace('\r\u001b[2K', '')].length).toBeLessThan(24)
  progress.stop()
  const wide: string[] = []
  const zero = startProgress({ isTTY: true, columns: 0, write: text => wide.push(text) }, UNICODE, ['installer'])
  zero.step('Downloading', 'Downloaded')
  zero.stop()
  expect(plain(wide.join(''))).toMatch(/Downloading {9}[\u2500\u2501]{10,}/u)
})

it('draws ASCII and 256 colours on a terminal that does not declare more', () => {
  const writes: string[] = []
  const progress = startProgress({ isTTY: true, columns: 80, write: text => writes.push(text) }, { TERM: 'xterm' }, ['installer'])
  progress.step('Downloading', 'Downloaded')
  progress.progress(0.5)
  progress.finish({ title: 'Bake 0.2.0 installed' })
  const text = writes.join('')
  expect(plain(text)).toMatch(/^[\x00-\x7f]*$/u)
  expect(plain(text)).toContain('+  Downloaded')
  expect(text).toMatch(/\u001b\[[\d;]*38;5;\d+m/u)
  expect(text).not.toContain('38;2;')
})
