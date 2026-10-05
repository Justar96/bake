/** A tool a preset mounts in an agent's own layer presents that agent's calls through its own card. */
import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, expect, it } from 'vitest'
import { createUserMessage, ToolCallId, type StreamChunk } from '@deepseek-ai/dsh-llm'
import { SessionId } from 'bake-session'
import SubagentRuntime, { SUBAGENT_DESCRIPTOR_VERSION } from '@deepseek-ai/dsh-subagent'
import { transcriptRows, type ToolCallRow } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

/**
 * A plugin the preset composition mounts, as the shipped presets mount every
 * model-facing tool: registered through the agent's context, so it lives in
 * that agent's layer and not in the global view.
 */
const PROBE_PLUGIN = `
export const name = 'probe-tool'
export const inject = ['tools']
export function apply(ctx) {
  ctx.tools.register({
    name: 'probe_tool',
    description: 'Probe a target.',
    parameters: { type: 'object', properties: { target: { type: 'string' }, notes: { type: 'string' } }, required: ['target'] },
    output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
    execute: async args => JSON.stringify({ probed: args.target, notes: args.notes }),
    presentCall: args => ({ card: 'generic', title: 'Probe ' + args.target }),
    presentResult: (args, result) => result.isError ? undefined
      : { card: 'generic', content: [{ type: 'text', text: 'probed ' + args.target }] },
  })
}
`

/** One model step that calls the probe, then the answer after its result. */
function probeTurn(args: string): () => AsyncIterable<StreamChunk> {
  let step = 0
  return async function* () {
    if (step++ > 0) { yield* textResponse('Probed.'); return }
    const call = { type: 'tool-call' as const, id: ToolCallId('call-probe-1'), name: 'probe_tool', arguments: args }
    const chunks: StreamChunk[] = [
      { type: 'block-start', index: 0, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: args },
      { type: 'block-end', index: 0, block: call },
      { type: 'finish', reason: { kind: 'tool-calls' } },
    ]
    yield* chunks
  }
}

it('presents an agent-layer tool\'s call and result through its presenters, not its raw arguments', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const preset = join(fixture.root, 'presets', 'audit')
  await writeFile(join(preset, 'probe-tool.mjs'), PROBE_PLUGIN)
  await writeFile(join(preset, 'agent.cordis.yml'), '- id: probe\n  name: ./probe-tool.mjs\n')
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, { refs: [] }, () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  // The composition this guards: visible to the agent, absent from the global view.
  expect(fixture.ctx.tools.get('probe_tool')).toBeUndefined()
  expect(fixture.ctx.tools.get('probe_tool', handle.agent)).toBeDefined()

  const args = JSON.stringify({ target: 'alpha', notes: 'a long note the headline has no room for '.repeat(4) })
  fixture.model.response = probeTurn(args)
  controller.submit('Probe alpha.')
  await controller.drain()
  await controller.agent.whenIdle()

  const call = transcriptRows(controller.view.committed).find((row): row is ToolCallRow => row.kind === 'tool-call')
  expect(call).toMatchObject({
    tool: 'probe_tool', input: 'Probe alpha', result: { ok: true, text: '', detail: [{ text: 'probed alpha' }] },
  })
  expect(transcriptRows(controller.view.committed)).toContainEqual({ kind: 'assistant', text: 'Probed.' })
})

it('presents an inspected child\'s agent-layer tool calls through the child\'s presenters', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const preset = join(fixture.root, 'presets', 'audit')
  await writeFile(join(preset, 'probe-tool.mjs'), PROBE_PLUGIN)
  await writeFile(join(preset, 'agent.cordis.yml'), '- id: probe\n  name: ./probe-tool.mjs\n')
  await fixture.ctx.plugin(SubagentRuntime)
  let controller!: SessionController
  const parent = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    controller = new SessionController(fixture.ctx, agent, dictionaries.en, { refs: [] }, () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await parent.dispose() })
  await controller.replay(new AbortController().signal)
  const child = await fixture.ctx.agents.create({ sessionId: SessionId('probe-child'),
    meta: { parentSession: parent.agent.id, origin: 'subagent' }, agentOptions: { provider: 'mock', model: 'model' } })
  cleanup.push(() => child.dispose())
  child.agent.session.append('subagent/descriptor', {
    version: SUBAGENT_DESCRIPTOR_VERSION, mode: 'continuable', provider: 'spawn', label: 'Probe beta',
  })
  // `agents.create` composes no preset, so the plugin is applied through the
  // child's own context, which is where a preset composition registers it.
  const { apply } = await import(pathToFileURL(join(preset, 'probe-tool.mjs')).href) as { apply: (ctx: unknown) => void }
  apply(child.agent.ctx)
  expect(fixture.ctx.tools.get('probe_tool')).toBeUndefined()
  expect(fixture.ctx.tools.get('probe_tool', child.agent)).toBeDefined()
  fixture.model.response = probeTurn(JSON.stringify({ target: 'beta' }))
  child.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Probe beta.' }], source: { kind: 'user' } }))
  await child.agent.whenIdle()

  controller.submit(`/agents ${child.agent.id}`)
  await controller.drain()
  const committed = controller.view.inspection?.committed
  expect(committed).toBeDefined()
  const call = transcriptRows(committed!).find((row): row is ToolCallRow => row.kind === 'tool-call')
  expect(call).toMatchObject({ tool: 'probe_tool', input: 'Probe beta', result: { ok: true, text: '', detail: [{ text: 'probed beta' }] } })
})
