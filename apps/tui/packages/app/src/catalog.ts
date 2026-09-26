/** Session-scoped command and skill discovery. Harness providers own catalog contents. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { CommandArgumentChoice } from '@deepseek-ai/dsh-commands/types'
import { isUserInvocable } from '@deepseek-ai/dsh-skill'
import type { Completion, CompletionCatalog } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import type {} from '@deepseek-ai/dsh-agent-presets'
import type {} from '@deepseek-ai/dsh-commands'

/** Cancelable catalog observations belonging to one terminal session. */
export class InputCatalog {
  private state: CompletionCatalog = { entries: [], loading: false, error: undefined }
  private abort: AbortController | undefined
  private argumentAbort: AbortController | undefined
  private argumentKey: string | undefined
  private argumentCache: { name: string; entries: readonly CommandArgumentChoice[] } | undefined
  private closed = false
  private readonly pending = new Set<Promise<void>>()
  private readonly off: (() => void)[]

  /**
   * Subscribe to registry invalidation without starting discovery.
   * @param ctx - terminal context with commands and optional preset/skill services.
   * @param agent - exact session whose scoped registrations are visible.
   * @param copy - localized catalog diagnostics.
   * @param changed - notify the renderer after a new observation.
   */
  constructor(private readonly ctx: Context, private readonly agent: Agent, private readonly copy: TuiCopy, private readonly changed: () => void) {
    this.off = [ctx.on('commands/change', () => {
      const query = this.state.argument
      this.searchArgument(undefined)
      this.refresh()
      if (query !== undefined) this.searchArgument(query)
    }), ctx.on('skills/change', () => this.refresh())]
  }

  /** Current metadata. Skill bodies are never loaded for completion. */
  get view(): CompletionCatalog { return this.state }

  /** Observe one command's first argument. Leaving the menu drops its session cache. */
  searchArgument(query: { name: string; partial: string } | undefined): void {
    if (this.closed) return
    const key = query === undefined ? undefined : `${query.name}\u0000${query.partial}`
    if (key === this.argumentKey) return
    this.argumentKey = key
    this.argumentAbort?.abort()
    if (query === undefined) {
      this.argumentCache = undefined
      this.state = { ...this.state, argument: undefined }
      this.changed()
      return
    }
    if (this.argumentCache?.name !== query.name) this.argumentCache = undefined
    const cached = this.argumentCache?.entries
    this.state = { ...this.state, argument: { ...query, entries: cached ?? [], loading: cached === undefined, error: undefined } }
    this.changed()
    if (cached !== undefined) return
    const abort = this.argumentAbort = new AbortController()
    const done = Promise.resolve().then(async () => {
      const commands = this.ctx.get('commands')
      if (commands === undefined) throw new Error('tui: commands service is required')
      const entries = await commands.choices(this.agent, query.name, query.partial, abort.signal)
      abort.signal.throwIfAborted()
      this.argumentCache = { name: query.name, entries }
      this.state = { ...this.state, argument: { ...query, entries, loading: false, error: undefined } }
      this.changed()
    }).catch((error: unknown) => {
      if (abort.signal.aborted) return
      this.state = { ...this.state, argument: { ...query, entries: [], loading: false, error: error instanceof Error ? error.message : String(error) } }
      this.changed()
    }).finally(() => { this.pending.delete(done) })
    this.pending.add(done)
  }

  /** Replace the current observation. A superseded or closed read cannot publish results. */
  refresh(): void {
    if (this.closed) return
    this.abort?.abort()
    const abort = this.abort = new AbortController()
    const commands = this.ctx.get('commands')
    if (commands === undefined) throw new Error('tui: commands service is required')
    const entries: Completion[] = commands.list(this.agent).map(command => ({
      name: command.name, description: command.description, kind: 'command',
      ...command.input === undefined ? {} : { hint: command.input.hint },
      ...command.input?.choices === undefined ? {} : { choices: true },
    }))
    const skills = this.ctx.get('agentPresets')?.serviceFor(this.agent, 'skills') ?? this.ctx.get('skills')
    this.state = { ...this.state, entries, loading: skills !== undefined, error: undefined }
    this.changed()
    if (skills === undefined) return
    const done = Promise.resolve().then(async () => {
      const catalog = await skills.snapshot({ cwd: this.agent.session.header.cwd, scope: this.agent, signal: abort.signal })
      abort.signal.throwIfAborted()
      const names = new Set(entries.map(entry => entry.name))
      this.state = { ...this.state,
        entries: [...entries, ...catalog.skills.filter(skill => isUserInvocable(skill) && !names.has(skill.name))
          .map(skill => ({ name: skill.name, description: skill.description, kind: 'skill' as const }))],
        loading: false, error: catalog.complete ? undefined : this.copy.catalogIncomplete,
      }
      this.changed()
    }).catch((error: unknown) => {
      if (abort.signal.aborted) return
      this.state = { ...this.state, entries, loading: false, error: error instanceof Error ? error.message : String(error) }
      this.changed()
    }).finally(() => { this.pending.delete(done) })
    this.pending.add(done)
  }

  /** Unsubscribe and abort discovery before terminal release. */
  close(): void {
    if (this.closed) return
    this.closed = true
    for (const off of this.off) off()
    this.abort?.abort()
    this.argumentAbort?.abort()
  }

  /** @returns after all owned discovery reads have settled. */
  async drain(): Promise<void> { await Promise.all(this.pending) }
}
