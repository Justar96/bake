/** The goal rule under the composer follows the goal service through `/goal` and its lifecycle. */
import { afterEach, expect, it, vi } from 'vitest'
import Goals from '@deepseek-ai/dsh-goal'
import * as CommandGoal from '@deepseek-ai/dsh-command-goal'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { goalState } from '@dsh-tui/ui'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

it('shows a goal activating from /goal, then paused and done, repainting on every change', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(Goals)
  await fixture.ctx.plugin(CommandGoal)
  const changed = vi.fn()
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], changed,
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  expect(controller.view.goal).toBeUndefined()
  expect(goalState(controller.view.goal, dictionaries.en)).toBeUndefined()

  changed.mockClear()
  controller.submit('/goal Ship the patch')
  await controller.drain()
  expect(changed).toHaveBeenCalled()
  expect(controller.view.goal).toEqual({ objective: 'Ship the patch', phase: 'active', armed: true, rounds: 0, maxRounds: 256 })
  expect(goalState(controller.view.goal, dictionaries.en)).toMatchObject({
    label: dictionaries.en.goalActive, details: 'round 0/256 \u00b7 Ship the patch',
  })

  const goals = fixture.ctx.goals
  const ref = () => { const goal = goals.get(handle.agent)!; return { id: goal.id, revision: goal.revision } }
  changed.mockClear()
  goals.pause(handle.agent, ref())
  expect(changed).toHaveBeenCalled()
  expect(controller.view.goal).toMatchObject({ phase: 'paused', armed: false })
  expect(goalState(controller.view.goal, dictionaries.en)).toMatchObject({ label: dictionaries.en.goalPaused })

  goals.resume(handle.agent, ref())
  expect(controller.view.goal).toMatchObject({ phase: 'active', armed: true })
  changed.mockClear()
  goals.complete(handle.agent, ref())
  expect(changed).toHaveBeenCalled()
  expect(controller.view.goal).toMatchObject({ phase: 'complete' })
  expect(goalState(controller.view.goal, dictionaries.en)).toMatchObject({ label: dictionaries.en.goalComplete })

  goals.clear(handle.agent, ref())
  expect(controller.view.goal).toBeUndefined()
})
