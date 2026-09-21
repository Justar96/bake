/** Session-scoped command and skill discovery; Harness providers own catalog contents. */
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { isUserInvocable } from '@deepseek-ai/dsh-skill'
import type { Completion, CompletionCatalog } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import type {} from '@deepseek-ai/dsh-agent-presets'
import type {} from '@deepseek-ai/dsh-commands'

/** Cancelable catalog observations belonging to one terminal session. */
export class InputCatalog {
  private state: CompletionCatalog = { entries: [], loading: false, error: undefined }
  private abort: AbortController | undefined
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
    this.off = [ctx.on('commands/change', () => this.refresh()), ctx.on('skills/change', () => this.refresh())]
  }

  /** Current metadata; skill bodies are never loaded for completion. */
  get view(): CompletionCatalog { return this.state }

  /** Replace the current observation; superseded and closed reads cannot publish results. */
  refresh(): void {
    if (this.closed) return
    this.abort?.abort()
    const abort = this.abort = new AbortController()
    const commands = this.ctx.get('commands')
    if (commands === undefined) throw new Error('tui: commands service is required')
    const entries: Completion[] = commands.list(this.agent).map(command => ({ ...command, kind: 'command' }))
    const skills = this.ctx.get('agentPresets')?.serviceFor(this.agent, 'skills') ?? this.ctx.get('skills')
    this.state = { entries, loading: skills !== undefined, error: undefined }
    this.changed()
    if (skills === undefined) return
    const done = Promise.resolve().then(async () => {
      const catalog = await skills.snapshot({ cwd: this.agent.session.header.cwd, scope: this.agent, signal: abort.signal })
      abort.signal.throwIfAborted()
      const names = new Set(entries.map(entry => entry.name))
      this.state = {
        entries: [...entries, ...catalog.skills.filter(skill => isUserInvocable(skill) && !names.has(skill.name))
          .map(skill => ({ name: skill.name, description: skill.description, kind: 'skill' as const }))],
        loading: false, error: catalog.complete ? undefined : this.copy.catalogIncomplete,
      }
      this.changed()
    }).catch((error: unknown) => {
      if (abort.signal.aborted) return
      this.state = { entries, loading: false, error: error instanceof Error ? error.message : String(error) }
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
  }

  /** @returns after all owned discovery reads have settled. */
  async drain(): Promise<void> { await Promise.all(this.pending) }
}
