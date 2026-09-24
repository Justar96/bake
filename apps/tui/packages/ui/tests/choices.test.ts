/** Picker filtering and scrolling. */

import { describe, expect, it } from 'bun:test'
import { filterChoices, scrollTo } from '../src/choices.ts'

const models = [
  { value: 'deepseek/deepseek-v4-flash', label: 'deepseek/deepseek-v4-flash' },
  { value: 'deepseek/deepseek-v4-pro', label: 'deepseek/deepseek-v4-pro', description: 'Reasoning' },
  { value: 'mock/model', label: 'mock/model' },
]

describe('filterChoices', () => {
  it('keeps every choice, in order, for an empty query', () => {
    expect(filterChoices(models, '  ').map(match => match.choice.value)).toEqual(models.map(model => model.value))
    expect(filterChoices(models, '').every(match => match.ranges.length === 0)).toBe(true)
  })

  it('requires every word, in any order, and ignores case', () => {
    expect(filterChoices(models, 'FLASH v4').map(match => match.choice.value)).toEqual(['deepseek/deepseek-v4-flash'])
    expect(filterChoices(models, 'v4 mock')).toEqual([])
  })

  it('matches the value and description without marking the label', () => {
    const sessions = [{ value: '514e6406-a7c8', label: 'Fix the build', description: '3d ago' }]
    expect(filterChoices(sessions, '514e')).toEqual([{ choice: sessions[0]!, ranges: [] }])
    expect(filterChoices(models, 'reasoning').map(match => match.choice.value)).toEqual(['deepseek/deepseek-v4-pro'])
  })

  it('marks the first occurrence of each word, merging ranges that touch', () => {
    expect(filterChoices(models, 'mock').at(0)?.ranges).toEqual([[0, 4]])
    expect(filterChoices(models, 'mo del').at(0)?.ranges).toEqual([[0, 2], [7, 10]])
    expect(filterChoices(models, 'deep seek').at(0)?.ranges).toEqual([[0, 8]])
  })
})

describe('scrollTo', () => {
  it('shows a list that fits whole', () => {
    expect(scrollTo(3, 4, 5, 5)).toEqual({ top: 0, count: 5, above: 0, below: 0 })
  })

  it('spends a row on each edge that hides choices', () => {
    expect(scrollTo(0, 0, 20, 6)).toEqual({ top: 0, count: 5, above: 0, below: 15 })
    expect(scrollTo(0, 5, 20, 6)).toEqual({ top: 2, count: 4, above: 2, below: 14 })
    expect(scrollTo(0, 19, 20, 6)).toEqual({ top: 15, count: 5, above: 15, below: 0 })
  })

  it('always shows the selection and never exceeds its rows', () => {
    for (let rows = 1; rows <= 8; rows++) {
      for (let previous = 0; previous < 20; previous++) {
        for (let selected = 0; selected < 20; selected++) {
          const scroll = scrollTo(previous, selected, 20, rows)
          expect(selected).toBeGreaterThanOrEqual(scroll.top)
          expect(selected).toBeLessThan(scroll.top + scroll.count)
          expect(scroll.count + (scroll.above > 0 ? 1 : 0) + (scroll.below > 0 ? 1 : 0)).toBeLessThanOrEqual(rows)
          // Two rows or fewer have no room for an edge row, so none is counted.
          if (rows > 2) expect(scroll.above + scroll.count + scroll.below).toBe(20)
        }
      }
    }
  })

  it('holds still while the selection moves inside the window', () => {
    const first = scrollTo(0, 7, 20, 6)
    expect(scrollTo(first.top, first.top, 20, 6).top).toBe(first.top)
    expect(scrollTo(first.top, first.top + first.count - 1, 20, 6).top).toBe(first.top)
  })

  it('moves only as far as the selection leaves the window', () => {
    const at = scrollTo(0, 7, 20, 6)
    expect(scrollTo(at.top, at.top - 1, 20, 6).top).toBe(at.top - 1)
    expect(scrollTo(at.top, at.top + at.count, 20, 6).top).toBe(at.top + 1)
  })

  it('draws nothing selected from the previous top', () => {
    expect(scrollTo(4, -1, 20, 6)).toEqual({ top: 4, count: 4, above: 4, below: 12 })
  })
})
