/** Command discovery through the harness registry. */
import { afterEach, describe, expect, it } from 'vitest'
import { formatRow, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import { harness } from './harness.ts'
import { bakeVersion } from '../src/release.ts'

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

/** The catalog as it lands in the transcript, which is where /help puts it. */
const listed = (controller: SessionController): string =>
  transcriptRows(controller.view.committed).map(formatRow).join('\n')

describe('/help', () => {
  it('lists every registered command with its description', async () => {
    const { controller } = await connected()
    controller.submit('/help')
    await controller.drain()

    // The transcript, not the notice region. The region is bounded by the
    // terminal's height and would cut the catalog to fit, while scrollback
    // holds the whole list and can scroll it.
    expect(listed(controller)).toContain('/help — List available commands')
    expect(listed(controller)).toContain('/login — Sign in or set up CLIProxyAPI')
    expect(controller.view.notice).toBeUndefined()
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
      expect(listed(controller)).toContain('/elsewhere — Contributed by another plugin')
    } finally {
      void dispose()
    }
  })
})

describe('/changelog', () => {
  it('commits the running version\'s entry to the transcript', async () => {
    const { controller } = await connected()
    controller.submit('/changelog')
    await controller.drain()
    expect(listed(controller)).toContain(`## [${bakeVersion()}]`)
    expect(controller.view.notice).toBeUndefined()
  })

  it('is listed by /help and rejects arguments', async () => {
    const { controller } = await connected()
    controller.submit('/help')
    await controller.drain()
    expect(listed(controller)).toContain('/changelog — Show what changed in this Bake version')
    controller.submit('/changelog extra')
    await controller.drain()
    expect(listed(controller)).toContain('Usage: /changelog')
  })
})
