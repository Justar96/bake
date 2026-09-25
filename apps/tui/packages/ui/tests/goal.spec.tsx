/** The goal block: its phases, its wrapping, and what it drops when rows run short. */
import React from 'react'
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { Goal, goalRows, type GoalEntry } from '../src/goal.tsx'
import { dictionaries } from '../src/copy.ts'

afterEach(cleanup)

const copy = dictionaries.en
const active: GoalEntry = { objective: 'Ship it', phase: 'active', armed: true, rounds: 2, maxRounds: 8 }
const draw = (goal: GoalEntry, limit: number, columns = 60): string[] =>
  render(<Goal goal={goal} copy={copy} columns={columns} limit={limit} />).lastFrame()!.split('\n').map(row => row.trimEnd())

describe('goal block', () => {
  it('names the phase with its rounds, hangs the objective, and lists the actions that apply', () => {
    expect(draw(active, 4)).toEqual([
      `● ${copy.goalActive}  ${copy.goalRound} 2/8`,
      '└ Ship it',
      `  ${copy.goalKeysActive}`,
    ])
    expect(goalRows(active, copy, 60)).toBe(3)
  })

  it('offers resume for a held or paused goal', () => {
    // Held is still active, so it keeps counting rounds; paused does not.
    expect(draw({ ...active, armed: false }, 4, 100)[0]).toBe(`○ ${copy.goalHeld}  ${copy.goalRound} 2/8`)
    expect(draw({ ...active, phase: 'paused' }, 4, 100)[0]).toBe(`○ ${copy.goalPaused}`)
    for (const goal of [{ ...active, armed: false }, { ...active, phase: 'paused' as const }]) {
      expect(draw(goal, 4, 100)[2]).toBe(`  ${copy.goalKeysHeld}`)
    }
  })

  it('says why a blocked goal stopped above the objective', () => {
    const rows = draw({ ...active, phase: 'blocked', blocked: 'Round limit reached' }, 5, 100)
    expect(rows).toEqual([
      `✗ ${copy.goalBlocked}`,
      '├ Round limit reached',
      '└ Ship it',
      `  ${copy.goalKeysBlocked}`,
    ])
  })

  it('wraps a long objective to two rows and marks what it cut', () => {
    const goal = { ...active, objective: 'Port the release pipeline to signed bundles for every platform and verify each checksum twice before publishing anything' }
    const rows = draw(goal, 6, 40)
    expect(rows).toHaveLength(4)
    expect(rows[1]!.startsWith('└ Port the release')).toBe(true)
    expect(rows[2]!.startsWith('  ')).toBe(true)
    expect(rows[2]!.endsWith('…')).toBe(true)
    expect(goalRows(goal, copy, 40)).toBe(4)
  })

  it('drops the actions, then the second objective row, before the head', () => {
    const goal = { ...active, objective: 'a '.repeat(40).trim() }
    expect(draw(goal, 3, 40)).toHaveLength(3)
    expect(draw(goal, 3, 40).at(-1)).not.toContain('/goal')
    const two = draw(goal, 2, 40)
    expect(two[0]).toContain(copy.goalActive)
    expect(two[1]!.endsWith('…')).toBe(true)
    expect(draw(goal, 1, 40)).toEqual([`● ${copy.goalActive}  ${copy.goalRound} 2/8`])
    expect(render(<Goal goal={goal} copy={copy} columns={40} limit={0} />).lastFrame()).toBe('')
  })
})
