/** Real harness services with a deterministic model and private durable storage. */
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Include from '@deepseek-ai/cordis-plugin-include'
import { mountAgentLoopTestDependencies } from '@deepseek-ai/dsh-agent-loop-testkit'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import Defaults from '@deepseek-ai/dsh-agent-default-model'
import Presets from '@deepseek-ai/dsh-agent-presets'
import Persistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import Query, { type SessionEventSearchRequest } from '@deepseek-ai/dsh-session-query'
import Commands from '@deepseek-ai/dsh-commands'
import Approval from '@deepseek-ai/dsh-user-approval'
import Questions from '@deepseek-ai/dsh-user-questions'
import { LlmAdapter, type GenerateOptions, type StreamChunk } from '@deepseek-ai/dsh-llm'

/** Exact queries use the production implementation; ranked search is not exercised. */
class ExactQuery extends Query {
  override searchSessions() { return Promise.resolve({ items: [] }) }
  override async searchEvents(request: SessionEventSearchRequest) {
    return { session: (await this.readSurface(request.sessionId)).session, items: [] }
  }
}

/** Only the external model is scripted. */
export class ScriptedModel extends LlmAdapter {
  readonly requests: GenerateOptions[] = []
  response: (options: GenerateOptions) => AsyncIterable<StreamChunk> = async function* () { yield* textResponse('Recorded answer') }
  override providerInfo(provider: string) { return { id: provider, name: provider } }
  override async resolveModel(provider: string, model: string) {
    return { provider, id: model, name: model, inputModalities: ['text'] as const, context: { contextWindow: 8192 } }
  }
  override async *stream(options: GenerateOptions) {
    this.requests.push(options)
    yield* this.response(options)
  }
}

/** @returns one complete model reply with an exact compact-stream representation. */
export function textResponse(text: string): StreamChunk[] {
  return [
    { type: 'block-start', index: 0, blockType: 'text' },
    { type: 'text-delta', index: 0, text },
    { type: 'block-end', index: 0, block: { type: 'text', text } },
    { type: 'finish', reason: { kind: 'stop' } },
  ]
}

/** @returns isolated services and cleanup that drains all agents before removing files. */
export async function harness() {
  const root = await mkdtemp(join(tmpdir(), 'dsh-tui-test-'))
  const ctx = new Context()
  const dispose = async () => { await ctx.fiber.dispose(); await rm(root, { recursive: true, force: true }) }
  try {
    const presets = join(root, 'presets')
    for (const id of ['audit', 'other']) {
      await mkdir(join(presets, id), { recursive: true })
      await writeFile(join(presets, id, 'agent.cordis.yml'), '[]\n')
    }
    ctx.baseUrl = pathToFileURL(root).href + '/'
    await ctx.plugin(Loader)
    ctx.loader.builtins.include = Include
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(Persistence, { root: join(root, 'sessions'), compression: 'none' })
    await ctx.plugin(ExactQuery)
    await ctx.plugin(AgentLoop, { agents: [] })
    await ctx.plugin(Defaults, { provider: 'mock', model: 'model' })
    await ctx.plugin(Presets, { default: 'audit', roots: [{ path: presets, trust: 'system' }], includeShippedRoot: false, includeUserRoot: false })
    await ctx.plugin(Commands)
    await ctx.plugin(Approval, {})
    await ctx.plugin(Questions)
    const model = new ScriptedModel()
    ctx.llm.registerAdapter(['mock'], model)
    return { ctx, model, root, dispose }
  } catch (error) { await dispose(); throw error }
}
