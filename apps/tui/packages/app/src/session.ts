/** Fresh-session creation and exact persisted-session adoption. */
import { randomUUID } from 'node:crypto'
import type { Context } from '@deepseek-ai/cordis'
import { installModelSelection, type Agent, type AgentHandle, type ModelSelectionRef } from '@deepseek-ai/dsh-agent'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { SessionId } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-agent-presets'
import { SessionAlreadyOwnedError } from '@deepseek-ai/dsh-session-persistence'
import type {} from '@deepseek-ai/dsh-session-projection'
import type {} from '@deepseek-ai/dsh-fs'
import { availableSelection } from './login.ts'

/** Explicit session choices resolved before the renderer mounts. */
export interface SessionOptions {
  readonly resume?: string
  readonly preset?: string
}

/**
 * Whether opening a session failed because another Bake process has it open.
 *
 * `openSession` refuses a live Agent of this process before the open, and the
 * terminal composes no other session writer, so the persistence refusal that
 * remains is the kernel lock another process holds. That lock records no
 * owner, so there is no process id to name.
 * @param error - an `openSession` failure.
 * @returns whether the session's write lock is held elsewhere.
 */
export function openElsewhere(error: unknown): error is SessionAlreadyOwnedError {
  return error instanceof SessionAlreadyOwnedError
}

/**
 * Thrown resuming a session recorded before `agentPresets` existed, or created
 * without one, when the caller supplied no `--preset` to restore it with.
 *
 * Only the command line can supply the missing preset, so a `/resume` picker
 * that reaches this session cannot recover it either; {@link openSession}'s
 * caller decides whether to word that for the reader or filter the session
 * out beforehand.
 */
export class SessionNeedsPresetError extends Error {
  constructor(message: string, readonly sessionId: SessionId) {
    super(message)
    this.name = 'SessionNeedsPresetError'
  }
}

/**
 * Whether resuming a session failed because it has no recorded preset and
 * none was supplied to restore its composition.
 * @param error - an `openSession` failure.
 * @returns whether {@link SessionNeedsPresetError} names the session.
 */
export function needsPreset(error: unknown): error is SessionNeedsPresetError {
  return error instanceof SessionNeedsPresetError
}

/**
 * Create a fresh session or resume the exact recorded composition and workspace.
 * @param ctx - settled application services.
 * @param options - requested identity and optional preset.
 * @param signal - application setup lifetime.
 * @param connect - install observers before the agent is published or driven.
 * @param credentialRefs - keys the default provider reads; a fresh session without them starts on CLIProxyAPI when it is set up.
 * @returns the owned agent handle. The caller must dispose it.
 * @throws {SessionAlreadyOwnedError} when another process has the resumed session open; see {@link openElsewhere}.
 */
export async function openSession(
  ctx: Context, options: SessionOptions, signal: AbortSignal,
  connect: (agent: Agent, selection: ModelSelectionRef) => void, credentialRefs: readonly string[] = [],
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
  const selection = options.resume === undefined
    ? await availableSelection(ctx, credentialRefs, defaults.currentSelection()) : defaults.currentSelection()
  signal.throwIfAborted()
  const initialPreset = presets === undefined || (options.resume !== undefined && options.preset === undefined)
    ? undefined : (await presets.resolve(options.preset)).id
  let setupWork: Promise<void> | undefined
  const setup = (agentCtx: Context, agent: Agent): Promise<void> => (setupWork = (async () => {
    signal.throwIfAborted()
    let preset = initialPreset
    if (options.resume !== undefined) {
      if (agent.session.header.cwd !== cwd) throw new Error(`tui: session belongs to ${agent.session.header.cwd ?? 'an unknown directory'}; open it from that workspace`)
      if (presets !== undefined) {
        const recorded = projections.stateOf(agent.session, 'agentPreset')
        if (recorded === undefined) throw new Error('tui: agentPreset projection is required to resume')
        if (recorded === null && options.preset === undefined) {
          throw new SessionNeedsPresetError('tui: session has no recorded preset; specify --preset to restore its composition', agent.id)
        }
        if (recorded !== null && options.preset !== undefined && recorded !== options.preset) throw new Error(`tui: session uses preset ${recorded}; --preset cannot change it during resume`)
        preset = recorded ?? initialPreset
      } else if (agent.session.header.agentPreset !== undefined) {
        throw new Error('tui: this session requires the agentPresets service')
      }
    }
    const lastRequest = options.resume === undefined ? undefined : agent.session.requestHeader()
    // An effort the adapter materialized as a default stays a default after resume.
    const restored = lastRequest === undefined ? selection : {
      provider: lastRequest.config.provider, model: lastRequest.config.model,
      ...lastRequest.config.reasoningEffort === undefined || lastRequest.adapterDefaults?.reasoningEffort === true
        ? {} : { reasoningEffort: lastRequest.config.reasoningEffort },
    }
    const selectionRef: ModelSelectionRef = { current: restored, assembled: undefined }
    installModelSelection(agentCtx, selectionRef)
    if (presets !== undefined && preset !== undefined) {
      await presets.mount(agentCtx, preset)
      signal.throwIfAborted()
      if (options.resume !== undefined && projections.stateOf(agent.session, 'agentPreset') === null) {
        agent.session.append('agent-preset/selected', { agentPreset: preset })
      }
    }
    signal.throwIfAborted()
    connect(agent, selectionRef)
  })())
  try {
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
  } catch (error) {
    // Preset mounting has no abort parameter. Keep its lifetime through rollback.
    await setupWork?.catch(() => {})
    throw error
  }
}
