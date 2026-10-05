/**
 * Default model selection for an Agent without a session-specific selection.
 *
 * @module bake-agent-default-model
 */

import { Context, Service } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import type { ModelSelection } from 'bake-agent'
import { ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-settings'

declare module '@deepseek-ai/cordis' {
  interface Context {
    /** Default model selection for Agents created without an explicit model. */
    agentDefaultModel: AgentDefaultModelConfig
  }
}

/** Settings namespace carrying the default model selection for future Agents. */
export const AGENT_DEFAULT_MODEL_SETTINGS_NAMESPACE = 'agent-default-model'

/**
 * Stored and composed default model selection. Both halves or neither: a
 * section without a provider and a model selects nothing.
 */
export interface AgentDefaultModelSettings {
  /** Registered provider route. */
  provider?: string
  /** Provider-owned model id. */
  model?: string
  /** Adapter-owned reasoning effort, or provider/default behavior when absent. */
  reasoningEffort?: string
}

/** Schema of the default Agent model settings section. */
export const AGENT_DEFAULT_MODEL_SETTINGS_SCHEMA: z<AgentDefaultModelSettings> = z.object({
  provider: z.string(),
  model: z.string(),
  reasoningEffort: z.string(),
})

/**
 * Composition entry for the default model selection. Absent, no provider is
 * the default: entry points start without a model until the user picks one,
 * and a saved selection is the only default there is.
 */
export interface Config {
  /** Registered provider route. */
  provider?: string
  /** Provider-owned model id. */
  model?: string
}

/**
 * Project stored settings onto the Agent-facing selection type.
 * @param settings - the resolved section.
 * @returns the selection, or undefined when it names no provider and model.
 */
function selection(settings: AgentDefaultModelSettings): ModelSelection | undefined {
  if (settings.provider === undefined || settings.provider === '' || settings.model === undefined || settings.model === '') {
    return undefined
  }
  return {
    provider: settings.provider,
    model: settings.model,
    ...settings.reasoningEffort === undefined
      ? {}
      : { reasoningEffort: ReasoningEffortId(settings.reasoningEffort) },
  }
}

/**
 * Owns the default model selection independently of any Host or transport.
 * The composition entry remains usable without a settings provider; when one
 * is mounted, its user layer is read live.
 */
export class AgentDefaultModelConfig extends Service {
  static Config: z<Config> = z.object({
    provider: z.string(),
    model: z.string(),
  })

  private source: () => AgentDefaultModelSettings

  constructor(ctx: Context, config: Config) {
    super(ctx, 'agentDefaultModel')
    if ((config.provider === undefined) !== (config.model === undefined)) {
      throw new TypeError('agent-default-model: provider and model are configured together or not at all')
    }
    const entry: AgentDefaultModelSettings = config.provider === undefined || config.model === undefined
      ? {} : { provider: config.provider, model: config.model }
    this.source = () => entry
    ctx.inject(['settings'], (settingsCtx) => {
      settingsCtx.settings.installSection(ctx, AGENT_DEFAULT_MODEL_SETTINGS_NAMESPACE, AGENT_DEFAULT_MODEL_SETTINGS_SCHEMA, entry, {
        setSource: (current) => { this.source = current },
        // Every consumer reads through currentSelection(), so no registration-level fact
        // needs rebuilding when the settings document changes.
        onChange: () => {},
      })
    })
  }

  /**
   * Read the current default model selection.
   * @returns a detached provider, model, and optional reasoning selection, or
   *   undefined when neither the composition nor the user saved one.
   */
  currentSelection(): ModelSelection | undefined {
    return selection(this.source())
  }

  /**
   * Save the complete default model selection. A deployment without a settings
   * provider keeps its composition entry.
   * @param next - resolved selection accepted by an entry point.
   * @returns fulfillment after the optional settings write settles.
   */
  async saveSelection(next: ModelSelection): Promise<void> {
    await this.ctx.get('settings')?.replace(AGENT_DEFAULT_MODEL_SETTINGS_NAMESPACE, {
      provider: next.provider,
      model: next.model,
      ...next.reasoningEffort === undefined ? {} : { reasoningEffort: String(next.reasoningEffort) },
    })
  }
}

/**
 * The message an entry point gives when it must start a turn and no model is
 * selected. One wording for every surface that cannot pick a model itself.
 */
export const NO_DEFAULT_MODEL_MESSAGE = 'no model is selected: start `bake`, sign in with /login, and choose one with /model;'
  + ' the choice is saved as the default for new sessions'

export default AgentDefaultModelConfig
