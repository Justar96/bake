/**
 * Basic replay-aware compaction backend. Its pressure policy resolves over the
 * optional `compaction-basic` user-settings section, layered above the
 * composition entry, so a changed threshold, retention, or route policy
 * reaches the next pressure check without a restart:
 *
 * ```yaml
 * # settings.yaml
 * compaction-basic:
 *   modelPolicies:
 *     # Every model on the cliproxyapi route compacts at 150k tokens.
 *     - provider: cliproxyapi
 *       thresholdTokens: 150000
 *       retainTokens: 30000
 *     # One model on that route keeps the ratio form instead.
 *     - provider: cliproxyapi
 *       model: gpt-5-codex
 *       thresholdRatio: 0.7
 * ```
 *
 * @module @deepseek-ai/dsh-compaction-basic
 */

import { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { CompactionEngine, ManualCompactionError } from '@deepseek-ai/dsh-compaction'
import type { CompactionResult, CompactionTrigger } from '@deepseek-ai/dsh-compaction'
import type { Session, SessionSeq } from '@deepseek-ai/dsh-session'
import { CONTEXT_WINDOW_EXCEEDED_CODE } from '@deepseek-ai/dsh-llm'
import type { LlmCallConfig } from '@deepseek-ai/dsh-llm'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { Agent, PreStepDecision } from '@deepseek-ai/dsh-agent'
import type { CommandId } from '@deepseek-ai/dsh-commands/brand'
import type {} from '@deepseek-ai/dsh-settings'
// Type-only: makes the optional sibling service available to `ctx.get()`.
import type {} from '@deepseek-ai/dsh-compaction-tool-result-pruner'
import {
  resolveCompactSpec,
  resolveConfig,
  resolveTargetPolicy,
  TargetPressureConfigError,
} from './config.ts'
import {
  assertNoActiveCompaction,
  compactSurfaceRegion,
  selectCompactableRange,
} from './region.ts'
import { summarizeWithLlm, summaryTarget } from './summarizer.ts'
import type { SummarizationInput, SummaryResult } from './summarizer.ts'
import type { SummaryRetryPlan } from './summary-retry.ts'
import type {
  BasicCompactionConfig,
  ModelCompactPolicyConfig,
  ResolvedCompactSpec,
  ResolvedConfig,
} from './types.ts'

export type {
  BasicCompactionConfig,
  CompactionPolicyConfig,
  ModelCompactPolicyConfig,
  ResolvedCompactSpec,
  ResolvedConfig,
  ResolvedRetention,
  ResolvedTargetPolicy,
  ResolvedThreshold,
} from './types.ts'

/** User-settings namespace layered over the composition entry. */
export const COMPACTION_BASIC_SETTINGS_NAMESPACE = 'compaction-basic'

/** Resolve the exact provider/model durably routed for the latest request. */
function routedTarget(
  session: Session,
): Pick<LlmCallConfig, 'provider' | 'model'> | undefined {
  const config = session.requestHeader()?.config
  if (config === undefined || config.provider.length === 0 || config.model.length === 0) {
    return undefined
  }
  return { provider: config.provider, model: config.model }
}

/**
 * Highest post-prune size that settles pressure without a summary: halfway
 * between the retained tail and the threshold. A summary lands near the
 * retained tail, so this demands at least half of a summary's headroom from
 * a prune before it may stand alone.
 * @param spec - pressure and retention budgets for the routed model.
 * @returns the exclusive token ceiling for a prune-only pass.
 */
function pruneOnlyCeiling(spec: Pick<ResolvedCompactSpec, 'thresholdTokens' | 'retainTokens'>): number {
  return spec.retainTokens + Math.floor((spec.thresholdTokens - spec.retainTokens) / 2)
}

/** Resolve the conversation target used to select an optional policy override. */
function conversationTarget(
  agent: Agent,
): Pick<LlmCallConfig, 'provider' | 'model'> | undefined {
  const routed = routedTarget(agent.session)
  if (routed !== undefined) return routed
  if (agent.options.provider === undefined || agent.options.provider.length === 0
    || agent.options.model === undefined || agent.options.model.length === 0) return undefined
  return { provider: agent.options.provider, model: agent.options.model }
}

const thresholdRatioSchema = z.number()
const thresholdTokensSchema = z.number().step(1).min(1)
const retainRatioSchema = z.number()
const retainTokensSchema = z.number().step(1).min(0)
const summarizationProviderSchema = z.string()
const summarizationModelSchema = z.string()
const maxTokensSchema = z.number().step(1).min(1)
const compactionRetriesSchema = z.number().step(1).min(0)
const maxOverflowRetriesSchema = z.number().step(1).min(0)

const modelPolicy: z<ModelCompactPolicyConfig> = z.object({
  provider: z.string().required(),
  model: z.string(),
  thresholdRatio: thresholdRatioSchema,
  thresholdTokens: thresholdTokensSchema,
  retainRatio: retainRatioSchema,
  retainTokens: retainTokensSchema,
  summarizationProvider: summarizationProviderSchema,
  summarizationModel: summarizationModelSchema,
  maxTokens: maxTokensSchema,
  compactionRetries: compactionRetriesSchema,
  maxOverflowRetries: maxOverflowRetriesSchema,
})

/** Fields shared by the composition entry and the settings section. */
const SETTINGS_FIELDS = {
  thresholdRatio: thresholdRatioSchema,
  thresholdTokens: thresholdTokensSchema,
  retainRatio: retainRatioSchema,
  retainTokens: retainTokensSchema,
  summarizationProvider: summarizationProviderSchema,
  summarizationModel: summarizationModelSchema,
  maxTokens: maxTokensSchema,
  compactionRetries: compactionRetriesSchema,
  maxOverflowRetries: maxOverflowRetriesSchema,
  modelPolicies: z.array(modelPolicy),
}

/** Schema of the `compaction-basic` settings section: every policy field except `auto`. */
const SETTINGS_SCHEMA: z<BasicCompactionConfig> = z.object(SETTINGS_FIELDS)

/** Top-level field pairs of which one layer may set only one. */
const EXCLUSIVE_FORMS = [
  ['thresholdRatio', 'thresholdTokens'],
  ['retainRatio', 'retainTokens'],
] as const

/**
 * Resolve one settings section with the composition's `auto`.
 *
 * The settings service merges the user section over the composition entry
 * key by key, so a user `thresholdTokens` over a composed `thresholdRatio`
 * arrives as both. The form the user set replaces the composed one, as a
 * more specific layer's form does inside `modelPolicies`; both forms set in
 * the user section itself still fail. `modelPolicies` replaces wholesale, so
 * no pair inside it spans layers. The schema passes unknown keys through, so
 * `auto` in the section is the user's and is refused rather than ignored.
 * @param section - composition entry overlaid by the user section.
 * @param entry - composition entry the section was layered over.
 * @param auto - composition-owned automatic-compaction switch.
 * @returns the validated configuration the engine serves.
 */
function resolveSettings(
  section: BasicCompactionConfig,
  entry: BasicCompactionConfig,
  auto: boolean,
): ResolvedConfig {
  const name = `settings "${COMPACTION_BASIC_SETTINGS_NAMESPACE}"`
  if ('auto' in section) {
    throw new Error(`${name}: "auto" is fixed by the plugin's composition config; remove it from settings`)
  }
  const yielded = new Set<string>()
  for (const [first, second] of EXCLUSIVE_FORMS) {
    if (section[first] === undefined || section[second] === undefined) continue
    // Only a key the composition alone set yields: equal to the entry's value
    // while the entry lacks the other form.
    if (entry[second] === undefined && section[first] === entry[first]) yielded.add(first)
    else if (entry[first] === undefined && section[second] === entry[second]) yielded.add(second)
  }
  const layered = Object.fromEntries(
    Object.entries(section).filter(([key]) => !yielded.has(key)),
  ) as BasicCompactionConfig
  return resolveConfig({ ...layered, auto }, name)
}

/**
 * Dependency-light compaction backend using `ctx.tokenMeter` for pressure,
 * retention, cited source events, and summary-convergence pricing.
 *
 * `summarize()` is the sole subclass customization hook; the replay and durable
 * mutation strategy stays fixed so every pricing decision uses the singleton
 * token meter.
 */
export class BasicCompactionEngine extends CompactionEngine {
  static inject = ['llm', 'tokenMeter', 'sessions']

  // Spelled out for the generated config catalog; the settings section
  // carries the same fields minus `auto` through `SETTINGS_FIELDS`.
  static Config: z<BasicCompactionConfig> = z.object({
    thresholdRatio: SETTINGS_FIELDS.thresholdRatio,
    thresholdTokens: SETTINGS_FIELDS.thresholdTokens,
    retainRatio: SETTINGS_FIELDS.retainRatio,
    retainTokens: SETTINGS_FIELDS.retainTokens,
    summarizationProvider: SETTINGS_FIELDS.summarizationProvider,
    summarizationModel: SETTINGS_FIELDS.summarizationModel,
    maxTokens: SETTINGS_FIELDS.maxTokens,
    compactionRetries: SETTINGS_FIELDS.compactionRetries,
    maxOverflowRetries: SETTINGS_FIELDS.maxOverflowRetries,
    modelPolicies: SETTINGS_FIELDS.modelPolicies,
    auto: z.boolean(),
  })

  private resolved: ResolvedConfig
  private readonly warnedPressureConfigTargets = new Set<string>()
  private readonly overflowRetries = new WeakMap<Agent, number>()
  private readonly overflowAgents = new WeakMap<Session, Agent>()
  /** Aborts summary retry waits when the plugin is disposed. */
  private readonly lifetime = new AbortController()

  constructor(ctx: Context, config: BasicCompactionConfig = {}) {
    super(ctx)
    ctx.effect(() => () => {
      this.lifetime.abort(new Error('compaction-basic disposed'))
    }, 'compaction-basic: cancel summary retry waits')
    this.resolved = resolveConfig(config)
    const { auto } = this.resolved
    // `auto` decides which listeners exist, so it stays composition-only; the
    // section carries every field that is read per pressure check.
    const { auto: _auto, ...entry } = config
    ctx.inject(['settings'], (settingsCtx) => {
      let source: () => BasicCompactionConfig = () => entry
      let registering = true
      settingsCtx.settings.installSection(ctx, COMPACTION_BASIC_SETTINGS_NAMESPACE, SETTINGS_SCHEMA, entry, {
        validate: (value) => {
          // A stored section that fails at registration would leave the
          // namespace unregistered, so its repair could only land after a
          // restart. Admit it; `onChange` keeps serving the composition
          // policy. Afterwards a refusal keeps the previous policy serving:
          // the settings service warns with this message and retains its
          // last good value.
          if (!registering) resolveSettings(value, entry, auto)
        },
        setSource: (current) => {
          source = current
        },
        onChange: () => {
          try {
            this.resolved = resolveSettings(source(), entry, auto)
          } catch (error) {
            const message = error instanceof Error ? error.message : String(error)
            ctx.logger.warn(`compaction-basic: keeping the previous compaction policy: ${message}`)
            return
          }
          // A repaired or replaced policy deserves a fresh warning per route.
          this.warnedPressureConfigTargets.clear()
        },
      })
      registering = false
    })
    if (auto) this._registerAutomaticCompaction()
  }

  /**
   * Resolved and validated compaction configuration: the composition entry,
   * overlaid by the `compaction-basic` settings section while a settings
   * service is mounted. Read afresh at every pressure check and summary.
   */
  get config(): ResolvedConfig {
    return this.resolved
  }

  /**
   * Register automatic between-step pressure and model-request overflow
   * recovery. `compactIfNeeded` stays dynamically dispatched so subclass
   * overrides are honored at event time.
   */
  private _registerAutomaticCompaction(): void {
    const { ctx } = this
    const logResult = (result: CompactionResult, trigger: string): void => {
      ctx.logger.info(
        `compaction (${trigger}): shadowed ${result.shadowedSeqs.length} surface nodes `
        + `(seqs ${result.shadowedRange.start}-${result.shadowedRange.end}, `
        + `~${result.shadowedTokenCount} tokens)`,
      )
    }

    ctx.on('agent/pre-step', async (
      { agent, signal },
      next,
    ): Promise<PreStepDecision> => {
      if (!signal.aborted) {
        try {
          const result = await this.compactIfNeeded(agent, 'pressure', signal)
          if (result !== null) logResult(result, 'step pressure')
        } catch (error: unknown) {
          if (error instanceof TargetPressureConfigError) {
            if (this.warnedPressureConfigTargets.has(error.targetKey)) return next()
            this.warnedPressureConfigTargets.add(error.targetKey)
          }
          const message = error instanceof Error ? error.message : String(error)
          ctx.logger.warn(`step compaction failed: ${message}; continuing the turn`)
        }
      }
      return next()
    })

    ctx.on('agent/status', ({ agent, status }) => {
      if (status === 'idle') this.overflowRetries.delete(agent)
    })

    // A successful response starts a fresh overflow-recovery sequence even
    // when tool calls continue the same turn into another request.
    ctx.on('session/event', (session, event) => {
      if (event.type !== 'assistant/message') return
      const agent = this.overflowAgents.get(session)
      if (agent !== undefined) this.overflowRetries.delete(agent)
    })

    ctx.on('agent/request-error', async (
      { agent, failure, signal },
      next,
    ) => {
      if (failure.code !== CONTEXT_WINDOW_EXCEEDED_CODE || signal.aborted) return next()
      this.overflowAgents.set(agent.session, agent)
      const target = routedTarget(agent.session)
      if (target === undefined) return next()
      const policy = resolveTargetPolicy(this.config, target)
      const retries = this.overflowRetries.get(agent) ?? 0
      if (retries >= policy.maxOverflowRetries) return next()

      const generation = agent.session.surface.replaceGeneration
      let result: CompactionResult | null
      try {
        result = await this.compactIfNeeded(agent, 'context-overflow', signal)
      } catch (recoveryError: unknown) {
        const message = recoveryError instanceof Error ? recoveryError.message : String(recoveryError)
        // A model-free prune can land before later summary work fails. That
        // durable reduction is sufficient retry proof; do not discard it just
        // because the optional second phase threw. Cancellation still wins.
        // oxlint-disable-next-line typescript/no-unnecessary-condition -- the signal can abort while recovery is awaited.
        if (!signal.aborted && agent.session.surface.replaceGeneration > generation) {
          ctx.logger.warn(
            `context-overflow compaction failed after durable surface progress: ${message}; `
            + 'retrying from the replacement surface',
          )
          this.overflowRetries.set(agent, retries + 1)
          return { kind: 'retry' }
        }
        ctx.logger.warn(
          // oxlint-disable-next-line typescript/no-unnecessary-condition -- the signal can abort while recovery is awaited.
          `context-overflow compaction failed: ${message}; ${signal.aborted
            ? 'cancellation prevents retry'
            : 'preserving the original request error'}`,
        )
        return next()
      }
      // oxlint-disable-next-line typescript/no-unnecessary-condition -- the signal can abort while compaction is awaited.
      if (signal.aborted
        || agent.session.surface.replaceGeneration <= generation) return next()
      if (result !== null) logResult(result, 'context overflow recovery')
      this.overflowRetries.set(agent, retries + 1)
      return { kind: 'retry' }
    })
  }

  /**
   * Summarize the replayed conversation region through a direct one-shot
   * `ctx.llm.stream()` call whose prefix reuses the conversation's own system
   * prompt, tools, and messages so the provider's KV cache is not invalidated.
   * Override this sole hook for a template or remote summarizer.
   * @param input - replayed conversation prefix (system, tools, and leading messages) to condense.
   * @param agent - supplies routed-model history, fallback model, and session id.
   * @param signal - optional cancellation forwarded to the adapter.
   * @returns safe text summary blocks and the exact auxiliary call envelope and output.
   */
  protected async summarize(
    input: SummarizationInput,
    agent: Agent,
    signal?: AbortSignal,
  ): Promise<SummaryResult> {
    const target = conversationTarget(agent)
    const config = target === undefined
      ? this.config
      : resolveTargetPolicy(this.config, target)
    return summarizeWithLlm(this.ctx, config, input, agent, signal)
  }

  /**
   * Compact for replayed step-boundary pressure or one provider-confirmed context
   * overflow. Both triggers price the latest durable routed request envelope;
   * overflow bypasses the normal threshold and retained-tail policy so it can
   * force one useful balanced reduction.
   * @param agent - agent whose latest durable routed request is measured.
   * @param trigger - normal step-boundary pressure or context-overflow recovery.
   * @param signal - live turn cancellation signal forwarded to summarization.
   * @returns the latest summary compaction result, or `null` when no summary ran.
   */
  override async compactIfNeeded(
    agent: Agent,
    trigger: CompactionTrigger,
    signal: AbortSignal,
  ): Promise<CompactionResult | null> {
    const target = routedTarget(agent.session)
    if (target === undefined) return null
    const policy = resolveTargetPolicy(this.config, target)
    const meter = this.ctx.tokenMeter
    let measurement = meter.measure(agent.session)
    switch (trigger) {
      case 'context-overflow':
        break
      case 'pressure':
        break
      /* v8 ignore next -- closed-union exhaustiveness guard */
      default:
        assertNever(trigger, 'compaction trigger')
    }

    // Pruning is optional so compaction-basic remains independently composable.
    // Overflow always qualifies; pressure first resolves the routed model's
    // capacity and checks its target-specific threshold.
    const prune = this.ctx.get('toolResultPruner')

    if (trigger === 'context-overflow') {
      if (prune !== undefined) {
        prune.pruneSession(agent.session)
        measurement = meter.measure(agent.session)
      }
      const range = selectCompactableRange(agent.session, measurement, 0)
      if (range === null) return null
      // The failed request is not retried until this pass lands, so its
      // summary call retries transient failures itself.
      return this.compactRegion(range.start, range.end, agent, signal, { retrySummary: true })
    }

    const context = (await this.ctx.llm.resolveModelInfo(target.provider, target.model, signal)).context
    assertNoActiveCompaction(agent.session, 'automatic pressure compaction')
    const targetKey = `${target.provider}/${target.model}`
    if (context === undefined) {
      throw new TargetPressureConfigError(
        targetKey,
        `compaction-basic: no context capacity for ${targetKey}; `
        + 'configure contextWindow on that adapter model',
      )
    }
    const spec = resolveCompactSpec(policy, context.contextWindow)
    if (measurement.totalTokens < spec.thresholdTokens) return null

    // Once pressure qualifies, land the model-free pass before choosing a
    // summary range, then remeasure through the singleton replay fold.
    if (prune !== undefined) {
      prune.pruneSession(agent.session)
      measurement = meter.measure(agent.session)
    }
    // Any surface rewrite invalidates the provider's prompt cache from the
    // first rewritten node onward, so a prune that only just clears the
    // threshold buys a few steps before the next pass rewrites history again.
    // A prune-only pass must therefore leave real headroom; otherwise the
    // summary lands in this same pass and the cache is rebuilt once.
    if (measurement.totalTokens < pruneOnlyCeiling(spec)) return null

    let result: CompactionResult | null = null
    for (let attempt = 0; attempt <= spec.compactionRetries; attempt += 1) {
      const range = selectCompactableRange(agent.session, measurement, spec.retainTokens)
      if (range === null) {
        /* v8 ignore else -- concrete replacement preserves a compactable checkpoint; subclass hooks cannot mutate it. */
        if (result === null) return null
        /* v8 ignore next -- paired with the defensive post-success branch above. */
        break
      }
      result = await this.compactRegion(range.start, range.end, agent, signal)
      measurement = meter.measure(agent.session)
      if (measurement.totalTokens < spec.thresholdTokens) return result
    }

    throw new Error(
      `compaction still above threshold after ${spec.compactionRetries + 1} compaction attempts `
      + `(${measurement.totalTokens} estimated tokens >= threshold ${spec.thresholdTokens})`,
    )
  }

  /**
   * Compact one inclusive positional range from the agent-owned surface using
   * the effective token meter for all retention and shrink pricing.
   * @param start - inclusive first surface-node seq.
   * @param end - inclusive last surface-node seq.
   * @param agent - owner of the target session, used by the summarizer.
   * @param signal - optional summarization cancellation signal.
   * @param options - `retrySummary` retries a transient summary failure under
   *   the summarizing provider's retry policy; pressure compaction leaves it
   *   off because its next step checks pressure again.
   * @returns the successful durable compaction result.
   */
  override async compactRegion(
    start: SessionSeq,
    end: SessionSeq,
    agent: Agent,
    signal?: AbortSignal,
    options: { readonly retrySummary?: boolean } = {},
  ): Promise<CompactionResult> {
    return compactSurfaceRegion(
      this.regionDependencies(),
      agent.session,
      start,
      end,
      agent,
      {
        owner: 'current-turn',
        stability: 'whole-surface',
        ...options.retrySummary === true ? this.summaryRetry(agent) : {},
      },
      signal,
    )
  }

  /**
   * Force one useful idle-session compaction below the pressure threshold, and
   * resolve only after its standalone marker pair is durably checkpointed.
   * @param agent - idle agent whose next-turn admission this call reserves.
   * @param signal - cancellation scoped to this compaction request.
   * @param sourceCommandId - initiating command identity for presentation correlation.
   * @returns the committed result, or `null` when no safe useful range exists.
   */
  override compactNow(
    agent: Agent,
    signal: AbortSignal,
    sourceCommandId?: CommandId,
  ): Promise<CompactionResult | null> {
    signal.throwIfAborted()
    try {
      return agent.runMaintenance(async (agentSignal) => {
        const operationSignal = AbortSignal.any([agentSignal, signal])
        try {
          operationSignal.throwIfAborted()
          const range = selectCompactableRange(
            agent.session,
            this.ctx.tokenMeter.measure(agent.session),
            0,
          )
          if (range === null) return null
          return await compactSurfaceRegion(
            this.regionDependencies(),
            agent.session,
            range.start,
            range.end,
            agent,
            {
              owner: null,
              stability: 'selected-span',
              ...sourceCommandId === undefined ? {} : { sourceCommandId },
              ...this.summaryRetry(agent),
              flush: async () => {
                await this.ctx.sessions.flush(agent.session)
              },
            },
            operationSignal,
          )
        } catch (error: unknown) {
          if (agentSignal.aborted && operationSignal.reason === agentSignal.reason) {
            throw new ManualCompactionError(
              'cancelled',
              'manual compaction was cancelled',
              { cause: error },
            )
          }
          operationSignal.throwIfAborted()
          throw error
        }
      })
    } catch (error: unknown) {
      throw new ManualCompactionError(
        'busy',
        'manual compaction requires an idle agent with no waking queued work',
        { cause: error },
      )
    }
  }

  /**
   * Resolve the exact route's merged policy the way the `agent/pre-step`
   * listener does: `thresholdTokens` when the policy sets it, otherwise
   * `floor(contextWindow × thresholdRatio)`, the figure it compares with the
   * token meter's measurement. `auto: false` installs no
   * listener, and an empty route is never compacted, so both answer
   * `undefined`. So does a capacity, absolute threshold, or absolute retention
   * that the listener would reject with a once-per-target warning instead of
   * compacting.
   * @param route - exact provider/model whose overrides, if any, apply.
   * @param contextWindow - that route's adapter-owned capacity in tokens.
   * @returns the pressure threshold in tokens, or `undefined` when automatic pressure cannot compact that route.
   */
  override pressureThreshold(
    route: { readonly provider: string; readonly model: string },
    contextWindow: number,
  ): number | undefined {
    if (!this.config.auto || route.provider.length === 0 || route.model.length === 0) return undefined
    try {
      return resolveCompactSpec(resolveTargetPolicy(this.config, route), contextWindow).thresholdTokens
    } catch (error: unknown) {
      if (error instanceof TargetPressureConfigError) return undefined
      throw error
    }
  }

  /**
   * The summarizing provider's request-retry policy for one transaction, or
   * nothing when no summarization target or provider registration resolves.
   * @param agent - supplies the routed target the summarizer would use.
   * @returns the transaction's retry plan, when one applies.
   */
  private summaryRetry(agent: Agent): { retry?: SummaryRetryPlan } {
    try {
      const conversation = conversationTarget(agent)
      const target = summaryTarget(
        conversation === undefined ? this.config : resolveTargetPolicy(this.config, conversation),
        agent,
      )
      if (target === undefined) return {}
      return { retry: { policy: this.ctx.llm.providerRetryPolicy(target.provider), lifetime: this.lifetime.signal } }
    } catch {
      return {}
    }
  }

  /** Bind the effective token meter and dynamically dispatched summarizer hook. */
  private regionDependencies(): Parameters<typeof compactSurfaceRegion>[0] {
    return {
      meter: this.ctx.tokenMeter,
      log: (message) => {
        this.ctx.logger.warn(message)
      },
      summarize: (input, owner, abort) => this.summarize(input, owner, abort),
      recover: (error, agent, sourceEventSeqs, signal) => this.ctx.waterfall('compaction/summary-error', {
        session: agent.session,
        sourceEventSeqs,
        error,
        ...signal === undefined ? {} : { signal },
      }, () => false),
    }
  }
}

export default BasicCompactionEngine
