/** Command discovery through the harness registry. */
import { afterEach, describe, expect, it } from 'vitest'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, selection)
  })
  cleanup.push(async () => { controller.close(); await handle.dispose(); await controller.drain() })
  await controller.replay(new AbortController().signal)
  return { ...fixture, handle, controller }
}

describe('/help', () => {
  it('lists every registered command with its description', async () => {
    const { controller } = await connected()
    controller.submit('/help')
    await controller.drain()

    const notice = controller.view.notice ?? ''
    expect(notice).toContain('/help — List available commands')
    expect(notice).toContain('/login — Sign in to a provider')
  })

  it('lists commands this surface never registered', async () => {
    // The registry is the list. A command contributed by any plugin in the
    // composition appears without the terminal knowing it exists, which is the
    // reason discovery reads the registry instead of a hand-kept table.
    const { ctx, handle, controller } = await connected()
    const dispose = handle.agent.ctx.effect(() => ctx.get('commands')!.register({
      name: 'elsewhere', description: 'Contributed by another plugin', recordInput: false,
      handler: () => ({ kind: 'success' }),
    }))
    try {
      controller.submit('/help')
      await controller.drain()
      expect(controller.view.notice ?? '').toContain('/elsewhere — Contributed by another plugin')
    } finally {
      void dispose()
    }
  })
})
