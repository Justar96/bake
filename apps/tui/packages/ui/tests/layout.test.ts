/** Region budgets and the render vocabulary. */
import { describe, expect, test } from 'bun:test'
import {
  budgetFor, CHROME_ROWS, COLUMN, isRenderable, LIVE_BUDGET, LIVE_SHARE, MARKER, selectionWindow, tailOf, VERB, windowOf,
} from '../src/layout.ts'

describe('budgetFor', () => {
  test('leaves the viewport row Ink needs to avoid clearing the screen', () => {
    // At viewport height Ink replays every committed row on each frame.
    expect(budgetFor({ columns: 80, rows: 24 }).dynamic).toBe(23)
    expect(budgetFor({ columns: 80, rows: 10 }).dynamic).toBe(9)
  })

  test('shrinks the live region on a short window instead of overrunning', () => {
    expect(budgetFor({ columns: 80, rows: 24 }).live).toBe(LIVE_BUDGET)
    // A tall terminal grows the window with its height, never to all of it.
    expect(budgetFor({ columns: 80, rows: 40 }).live).toBe(Math.floor(39 * LIVE_SHARE))
    expect(budgetFor({ columns: 80, rows: 80 }).live).toBe(Math.floor(79 * LIVE_SHARE))
    expect(budgetFor({ columns: 80, rows: 12 }).live).toBe(12 - 1 - CHROME_ROWS)
    expect(budgetFor({ columns: 80, rows: CHROME_ROWS + 3 }).live).toBe(2)
    // Below the height where chrome and one live row both fit, the live region
    // is the one-row floor. DESIGN-LAYOUT.md §3 scopes the priority order to
    // windows that can honour it and names the degraded mode for the rest.
    // interaction-or-composer plus status, and nothing else.
    expect(budgetFor({ columns: 80, rows: 3 }).live).toBe(1)
  })

  test('leaves the chrome its rows, since a running turn reserves the whole live budget', () => {
    // The live region holds its budget for the length of every turn, so a live
    // budget that does not leave the chrome its rows is an L1 violation held
    // for the length of every turn instead of a transient one. It holds from
    // the smallest window that can seat the chrome and a row above it.
    for (const rows of [CHROME_ROWS + 2, 12, 24, 40, 120]) {
      const budget = budgetFor({ columns: 80, rows })
      expect(budget.live + CHROME_ROWS).toBeLessThanOrEqual(budget.dynamic)
    }
  })

  test('keeps every budget positive at sizes no one should use', () => {
    const tiny = budgetFor({ columns: 1, rows: 1 })
    expect(tiny.dynamic).toBeGreaterThan(0)
    expect(tiny.live).toBeGreaterThan(0)
    expect(tiny.items).toBeGreaterThan(0)
    expect(tiny.measure).toBeGreaterThan(0)
    expect(tiny.output).toBeGreaterThan(0)
  })

  test('derives the item limit from height, and charges a header a row', () => {
    // Stated against the chrome floor, not as a number. An overlay may
    // use every row the viewport has left once chrome and its title are paid.
    const rowsLeft = (rows: number): number => rows - 1 - CHROME_ROWS
    expect(budgetFor({ columns: 80, rows: 24 }).items).toBe(rowsLeft(24))
    expect(budgetFor({ columns: 80, rows: 24 }, { header: true }).items).toBe(rowsLeft(24) - 1)
    expect(budgetFor({ columns: 80, rows: 10 }, { header: true }).items).toBe(rowsLeft(10) - 1)
  })

  test('uses the current width for prose, including after a resize', () => {
    for (const columns of [1, 8, 20, 40, 80, 300]) {
      const budget = budgetFor({ columns, rows: 24 })
      expect(budget.columns).toBe(columns)
      expect(budget.measure).toBe(Math.max(1, columns - COLUMN.rail))
    }
  })

  test('gives tool output the full width, since a wrapped log loses its alignment', () => {
    const wide = budgetFor({ columns: 300, rows: 24 })
    expect(wide.output).toBe(300 - COLUMN.output)
    expect(wide.output).toBeLessThan(wide.measure)
  })
})

describe('windowOf', () => {
  test('shows everything when it fits', () => {
    expect(windowOf(['a', 'b'], 5)).toEqual({ shown: ['a', 'b'], hidden: 0 })
    expect(windowOf(['a', 'b', 'c'], 3)).toEqual({ shown: ['a', 'b', 'c'], hidden: 0 })
  })

  test('reserves the footer row, so one item too many hides two', () => {
    // The footer costs a row. Four items in three rows shows two and hides two.
    expect(windowOf(['a', 'b', 'c', 'd'], 3)).toEqual({ shown: ['a', 'b'], hidden: 2 })
  })

  test('survives a limit of one, where only the footer fits', () => {
    expect(windowOf(['a', 'b'], 1)).toEqual({ shown: [], hidden: 2 })
    expect(windowOf(['a', 'b'], 0)).toEqual({ shown: [], hidden: 2 })
  })
})

describe('selectionWindow', () => {
  const items = Array.from({ length: 12 }, (_, index) => index)

  test('keeps every selection visible after the terminal reduces the row limit', () => {
    for (const limit of [1, 2, 4, 8]) {
      for (const selected of items) {
        const window = selectionWindow(items, selected, limit)
        expect(window.shown[window.selected]).toBe(selected)
        expect(window.shown.length + (window.hidden > 0 && limit > 1 ? 1 : 0)).toBeLessThanOrEqual(limit)
        expect(window.hidden).toBe(items.length - window.shown.length)
      }
    }
  })

  test('counts omitted entries above and below the selected window', () => {
    expect(selectionWindow(items, 11, 4)).toEqual({ shown: [9, 10, 11], selected: 2, hidden: 9 })
    expect(selectionWindow(items, 5, 4)).toEqual({ shown: [3, 4, 5], selected: 2, hidden: 9 })
  })

  test('honors the configured candidate limit even when the terminal has room for more', () => {
    expect(selectionWindow([0, 1, 2], 0, 3, 2)).toEqual({ shown: [0, 1], selected: 0, hidden: 1 })
    expect(selectionWindow([0, 1, 2], 2, 3, 2)).toEqual({ shown: [1, 2], selected: 1, hidden: 1 })
  })

  test('shows the complete list when it fits', () => {
    expect(selectionWindow([0, 1], 1, 4)).toEqual({ shown: [0, 1], selected: 1, hidden: 0 })
    expect(selectionWindow([], 0, 4)).toEqual({ shown: [], selected: 0, hidden: 0 })
  })
})

describe('tailOf', () => {
  test('keeps the most recent lines', () => {
    expect(tailOf([1, 2, 3, 4], 2)).toEqual([3, 4])
    expect(tailOf([1, 2], 5)).toEqual([1, 2])
  })

  test('returns nothing rather than everything when no rows are available', () => {
    expect(tailOf([1, 2], 0)).toEqual([])
    expect(tailOf([1, 2], -1)).toEqual([])
  })
})

describe('vocabulary', () => {
  test('every verb is ASCII, since verbs sit in a line with other text', () => {
    for (const verb of Object.values(VERB)) expect(isRenderable(verb)).toBe(true)
  })

  test('every marker is a single cell as measured', () => {
    // A marker may be non-ASCII. It sits alone in a fixed-width rail, so a
    // terminal drawing it wider than measured shifts that row and no other.
    for (const marker of Object.values(MARKER)) expect([...marker]).toHaveLength(1)
  })

  test('the prompt and the selection marker stay distinct', () => {
    // Two identical markers a row apart look like one list.
    expect(MARKER.selected).not.toBe(MARKER.prompt)
  })

  test('history and live input are marked differently', () => {
    // A turn in scrollback is not an invitation to type; the prompt is.
    expect(MARKER.turn).not.toBe(MARKER.prompt)
    expect(MARKER.turn).not.toBe(MARKER.selected)
  })

  test('every verb fits its column with a gap', () => {
    for (const verb of Object.values(VERB)) expect(verb.length).toBeLessThan(COLUMN.verb)
  })

  test('rejects a glyph a terminal and string-width can disagree about', () => {
    expect(isRenderable('run')).toBe(true)
    expect(isRenderable('\u2699')).toBe(false)
    expect(isRenderable('\u2699\uFE0F')).toBe(false)
    expect(isRenderable('\u203a')).toBe(false)
  })
})
