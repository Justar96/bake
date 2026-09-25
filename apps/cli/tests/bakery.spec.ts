/** Terminal progress owns its clock and leaves diagnostics on a cleared row. */
import { afterEach, expect, it, vi } from 'vitest'
import { startBakery } from '../src/bakery.ts'

afterEach(() => vi.useRealTimers())

it('animates in place, changes stage, and stops all output after finishing', () => {
  vi.useFakeTimers()
  const writes: string[] = []
  const bakery = startBakery({ isTTY: true, columns: 80, write: text => writes.push(text) }, {}, 'Downloading...')
  vi.advanceTimersByTime(540)
  expect(new Set(writes).size).toBeGreaterThan(1)
  expect(writes.every(text => text.startsWith('\r\x1b[2K') && !text.includes('\n'))).toBe(true)
  bakery.stage('Baking...')
  vi.advanceTimersByTime(180)
  expect(writes.at(-1)).toContain('Baking...')
  bakery.finish(true)
  expect(writes.at(-1)).toContain('Freshly baked.')
  const finished = writes.join('')
  bakery.finish()
  bakery.stage('Late callback')
  vi.advanceTimersByTime(2000)
  expect(writes.join('')).toBe(finished)
  expect(vi.getTimerCount()).toBe(0)
})

it.each([{ TERM: 'dumb' }, { CI: '1' }, { BAKE_NO_ANIMATION: '1' }])('respects a static terminal preference %j', (env) => {
  vi.useFakeTimers()
  const write = vi.fn()
  const bakery = startBakery({ isTTY: true, write }, env, 'Baking...')
  bakery.finish(true)
  expect(write).not.toHaveBeenCalled()
  expect(vi.getTimerCount()).toBe(0)
})

it('keeps redirected output free of escape sequences and animation text', () => {
  const write = vi.fn()
  startBakery({ isTTY: false, write }, {}, 'Baking...').finish(true)
  expect(write).not.toHaveBeenCalled()
})

it('fits a narrow terminal after resize and clears failures without a success message', () => {
  vi.useFakeTimers()
  const writes: string[] = []
  const terminal = { isTTY: true, columns: 22, write: (text: string) => writes.push(text) }
  const bakery = startBakery(terminal, { NO_COLOR: '1' }, 'Baking...')
  terminal.columns = 12
  vi.advanceTimersByTime(180)
  expect((writes.at(-1) ?? '').replace('\r\x1b[2K', '').length).toBeLessThan(12)
  expect(writes.join('')).not.toContain('[38;')
  bakery.finish()
  expect(writes.at(-1)).toBe('\r\x1b[2K')
  expect(writes.join('')).not.toContain('Freshly baked')
  expect(vi.getTimerCount()).toBe(0)
})

it('draws at a default width when the terminal reports zero columns', () => {
  const writes: string[] = []
  startBakery({ isTTY: true, columns: 0, write: text => writes.push(text) }, { NO_COLOR: '1' }, 'Baking...').finish(true)
  expect(writes[0]).toContain('Baking...')
  expect(writes.at(-1)).toContain('Freshly baked.')
})
