/** Plan status follows the Harness projection through command and session state. */
import { afterEach, expect, it, vi } from 'vitest'
import PlanMode from '@deepseek-ai/dsh-plan-mode'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

it('reflects a logged plan change and repaints when its projection changes', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(PlanMode, { section: 'Plan before acting.' })
  const changed = vi.fn()
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], changed,
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  expect(controller.view.plan).toEqual({ active: false, pending: false })

  changed.mockClear()
  expect(fixture.ctx.planMode.set(handle.agent, true)).toBe('committed')
  expect(controller.view.plan).toEqual({ active: true, pending: false })
  expect(changed).toHaveBeenCalled()
})

it('shows a mode change waiting for the next accepted step', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(PlanMode, { section: 'Plan before acting.' })
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  const entered = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  fixture.model.response = async function* () {
    entered.resolve()
    await release.promise
    yield* textResponse('Done')
  }
  controller.submit('Start')
  await entered.promise
  try {
    expect(controller.submit('/plan')).toBe(true)
    await controller.drain()
    expect(controller.view.plan).toEqual({ active: false, pending: true })
  } finally { release.resolve() }
  await handle.agent.whenIdle()
})
