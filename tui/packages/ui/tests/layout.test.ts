/** Region budgets and the render vocabulary. */
import { describe, expect, test } from 'bun:test'
import {
  budgetFor, CHROME_ROWS, COLUMN, isRenderable, LIVE_BUDGET, MARKER, PROSE_MEASURE, tailOf, VERB, windowOf,
} from '../src/layout.ts'

describe('budgetFor', () => {
  test('leaves the viewport row Ink needs to avoid clearing the screen', () => {
    // At viewport height Ink replays every committed row on each frame.
    expect(budgetFor({ columns: 80, rows: 24 }).dynamic).toBe(23)
    expect(budgetFor({ columns: 80, rows: 10 }).dynamic).toBe(9)
  })

  test('shrinks the live region on a short window instead of overrunning', () => {
    expect(budgetFor({ columns: 80, rows: 40 }).live).toBe(LIVE_BUDGET)
    expect(budgetFor({ columns: 80, rows: 10 }).live).toBe(10 - 1 - CHROME_ROWS)
    expect(budgetFor({ columns: 80, rows: 6 }).live).toBe(6 - 1 - CHROME_ROWS)
    // Below this height the live region is the one-row floor: chrome still fits,
    // and the viewport row Ink needs is still left free.
    expect(budgetFor({ columns: 80, rows: 3 }).live).toBe(1)
  })

  test('leaves the chrome its rows, since a running turn reserves the whole live budget', () => {
    // The live region holds its budget for the length of every turn, so a live
    // budget that does not leave the chrome its rows is an L1 violation held
    // for the length of every turn rather than a transient one.
    for (const rows of [6, 10, 24, 40, 120]) {
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
    expect(budgetFor({ columns: 80, rows: 24 }).items).toBe(21)
    expect(budgetFor({ columns: 80, rows: 24 }, { header: true }).items).toBe(20)
    expect(budgetFor({ columns: 80, rows: 10 }, { header: true }).items).toBe(6)
  })

  test('caps prose at the measure however wide the terminal is', () => {
    expect(budgetFor({ columns: 300, rows: 24 }).measure).toBe(PROSE_MEASURE)
    expect(budgetFor({ columns: 40, rows: 24 }).measure).toBe(40 - COLUMN.rail)
  })

  test('gives tool output the full width, since a wrapped log loses its alignment', () => {
    const wide = budgetFor({ columns: 300, rows: 24 })
    expect(wide.output).toBe(300 - COLUMN.output)
    expect(wide.output).toBeGreaterThan(wide.measure)
  })
})

describe('windowOf', () => {
  test('shows everything when it fits', () => {
    expect(windowOf(['a', 'b'], 5)).toEqual({ shown: ['a', 'b'], hidden: 0 })
    expect(windowOf(['a', 'b', 'c'], 3)).toEqual({ shown: ['a', 'b', 'c'], hidden: 0 })
  })

  test('reserves the footer row, so one item too many hides two', () => {
    // The footer costs a row: four items in three rows shows two and hides two.
    expect(windowOf(['a', 'b', 'c', 'd'], 3)).toEqual({ shown: ['a', 'b'], hidden: 2 })
  })

  test('survives a limit of one, where only the footer fits', () => {
    expect(windowOf(['a', 'b'], 1)).toEqual({ shown: [], hidden: 2 })
    expect(windowOf(['a', 'b'], 0)).toEqual({ shown: [], hidden: 2 })
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
    // A marker may be non-ASCII: it sits alone in a fixed-width rail, so a
    // terminal drawing it wider than measured shifts that row and no other.
    for (const marker of Object.values(MARKER)) expect([...marker]).toHaveLength(1)
  })

  test('the prompt and the selection marker stay distinct', () => {
    // Two identical markers a row apart read as one list.
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
