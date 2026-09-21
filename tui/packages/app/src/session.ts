/** Fresh-session creation and exact persisted-session adoption. */
import { randomUUID } from 'node:crypto'
import type { Context } from '@deepseek-ai/cordis'
import { installModelSelection, type Agent, type AgentHandle } from '@deepseek-ai/dsh-agent'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { SessionId } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-agent-presets'
import type {} from '@deepseek-ai/dsh-session-persistence'
import type {} from '@deepseek-ai/dsh-session-projection'
import type {} from '@deepseek-ai/dsh-fs'

/** Explicit session choices resolved before the renderer mounts. */
export interface SessionOptions {
  readonly resume?: string
  readonly preset?: string
}

/**
 * Create a fresh session or resume the exact recorded composition and workspace.
 * @param ctx - settled application services.
 * @param options - requested identity and optional preset.
 * @param signal - application setup lifetime.
 * @param connect - install observers before the agent is published or driven.
 * @returns the owned agent handle; the caller must dispose it.
 */
export async function openSession(
  ctx: Context, options: SessionOptions, signal: AbortSignal, connect: (agent: Agent) => void,
): Promise<AgentHandle> {
  const agents = ctx.get('agents')
  const defaults = ctx.get('agentDefaultModel')
  const projections = ctx.get('sessionProjections')
  if (agents === undefined || defaults === undefined || projections === undefined) {
    throw new Error('tui: agents, agentDefaultModel, and sessionProjections are required')
  }
  for (const [name, value] of Object.entries({ resume: options.resume, preset: options.preset })) {
    if (value !== undefined && value.trim() === '') throw new Error(`tui: ${name} must not be blank`)
  }
  const presets = ctx.get('agentPresets')
  if (options.preset !== undefined && presets === undefined) throw new Error('tui: --preset requires agentPresets')
  const fs = ctx.get('fs')
  const cwd = fs === undefined ? process.cwd() : fs.processPath(await fs.resolve('.'))
  const selection = defaults.currentSelection()
  const initialPreset = presets === undefined || (options.resume !== undefined && options.preset === undefined)
    ? undefined : (await presets.resolve(options.preset)).id
  const setup = async (agentCtx: Context, agent: Agent): Promise<void> => {
    signal.throwIfAborted()
    let preset = initialPreset
    if (options.resume !== undefined) {
      if (agent.session.header.cwd !== cwd) throw new Error(`tui: session belongs to ${agent.session.header.cwd ?? 'an unknown directory'}; open it from that workspace`)
      if (presets !== undefined) {
        const recorded = projections.stateOf(agent.session, 'agentPreset')
        if (recorded === undefined) throw new Error('tui: agentPreset projection is required to resume')
        if (recorded === null && options.preset === undefined) throw new Error('tui: session has no recorded preset; specify --preset to restore its composition')
        if (recorded !== null && options.preset !== undefined && recorded !== options.preset) throw new Error(`tui: session uses preset ${recorded}; --preset cannot change it during resume`)
        preset = recorded ?? initialPreset
      } else if (agent.session.header.agentPreset !== undefined) {
        throw new Error('tui: this session requires the agentPresets service')
      }
    }
    installModelSelection(agentCtx, { current: selection, assembled: undefined })
    if (presets !== undefined && preset !== undefined) {
      await presets.mount(agentCtx, preset)
      if (options.resume !== undefined && projections.stateOf(agent.session, 'agentPreset') === null) {
        agent.session.append('agent-preset/selected', { agentPreset: preset })
      }
    }
    connect(agent)
  }
  if (options.resume !== undefined) {
    if (ctx.get('sessionPersistence') === undefined) throw new Error('tui: --resume requires sessionPersistence')
    const id = brandString<SessionId>(options.resume)
    if (agents.get(id) !== undefined) throw new Error(`tui: session ${id} already has a live owner`)
    return await agents.resume({ resumeSessionId: id, agentOptions: selection, signal, setup })
  }
  return await agents.create({
    sessionId: brandString<SessionId>(`session-${randomUUID()}`),
    meta: { cwd, ...initialPreset === undefined ? {} : { agentPreset: initialPreset } },
    agentOptions: selection, signal, setup,
  })
}
