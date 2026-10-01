/**
 * Model-facing delegation through one configured `ctx.subagents` provider.
 * Provider lifecycle controls tool registration and context-sensitive schema
 * wording. Foreground calls always dispose the run after collection.
 * Background policy is selected by this plugin's configuration: one-shot
 * calls own a plain Task, while continuable calls use
 * `ctx.subagents.startContinuable()`.
 * @module @deepseek-ai/dsh-tool-subagent
 */

import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { scopeChainOf, scopeOf } from '@deepseek-ai/dsh-scope'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { Agent, AgentOptions } from '@deepseek-ai/dsh-agent'
import { ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import type { ContentBlock, LlmCallConfig } from '@deepseek-ai/dsh-llm'
import type { JsonValue } from '@deepseek-ai/dsh-util-values'
import { SessionSeq } from '@deepseek-ai/dsh-session'
import type { Session, SessionId } from '@deepseek-ai/dsh-session'
import {
  assertSubagentMaxDepth,
  parentAgentOptionsForDelegation,
  settleRun,
} from '@deepseek-ai/dsh-subagent'
import type { SubagentProvider, SubagentResult, SubagentRun } from '@deepseek-ai/dsh-subagent'
import type { JobOutcome } from '@deepseek-ai/dsh-jobs'
import {
  assertAllowedModelSelection,
  hasConfiguredLlmSelection,
  hasDelegationModelRequest,
  preflightChildLlmRoute,
  requestedAgentOptions,
  withoutBlankRoute,
} from './model-selection.ts'
import type { DelegationModelRequest, ModelSelectionPolicy } from './model-selection.ts'
import { registerListSubagentModels } from './list-models.ts'
import { routeDelegation, RouterFallback } from './auto-route.ts'
import { presentDelegationCall } from './presentation.ts'
import type {} from './model-selection-settings.ts'
import {
  recordSubagentModelSelection,
  subagentModelSelectionProjectionDefinition,
  subagentModelSelectionPolicy,
} from './model-selection-state.ts'
import { recordSubagentRoutingDecision, subagentRoutingProjectionDefinition } from './routing-state.ts'
import type { SubagentRouterDecision, SubagentRoutingDecision } from './types.ts'

export type {} from './routing-state.ts'
export type { SubagentRoutingAssessment, SubagentRouterDecision, SubagentRoutingDecision } from './types.ts'

export const name = 'tool-subagent'
export const inject = ['tools', 'subagents', 'sessionProjections']

/** Config: which registered provider this tool delegates to, plus child defaults. */
export interface Config {
  /** The `ctx.subagents` provider name to start runs on (e.g. `spawn`, `acp`). */
  provider: string
  /**
   * Model-facing tool name (default `subagent`). Each loaded instance must use
   * a distinct name.
   */
  toolName?: string
  /**
   * Sample the Host `subagent-model-selection` setting for each new top-level
   * Session and inherit that decision in its child Sessions.
   */
  modelSelectionSettings?: boolean
  /**
   * Expose `run_in_background` (default true). Disabled instances omit the
   * parameter and reject forced background calls.
   */
  enableRunInBackground?: boolean
  /**
   * Background execution policy (default `one-shot`). `one-shot` defaults calls
   * to foreground; `continuable` defaults them to background, requires a provider
   * with the `prepareContinuable` capability, and returns the durable child id.
   * Follow-up adapters remain independently optional.
   */
  backgroundMode?: 'one-shot' | 'continuable'
  /**
   * Agent options applied to every child; omitted fields use child-loop defaults.
   */
  agentOptions?: AgentOptions
  /**
   * Per-child persona that shadows `deployment:persona-prefix`. Requires the
   * provider's `persona` capability; omission preserves the deployment persona.
   */
  persona?: string
  /**
   * Tool filter applied to every child. Filtered tools disappear from its
   * prompt and reject execution. Requires the provider's `toolFilter`
   * capability; unknown names fail startup.
   */
  toolFilter?: {
    /** Global tool names the child keeps; everything else is removed. */
    allow?: string[]
    /** Global tool names removed from the child. */
    deny?: string[]
  }
  /**
   * Maximum child depth: a non-negative safe integer (`0` forbids delegation),
   * or `'provider-managed'` to send no cap. A numeric cap
   * requires the provider's `depthLimit` capability (mount fails loud
   * otherwise). The provider checks the calling agent's current depth at every
   * start; the tool remains model-visible so runtime policy owns rejection.
   * `'provider-managed'` is for an out-of-process provider whose recursion
   * budget belongs to the child runtime or its own deployment. Omission reads
   * the current Host subagent depth setting (default `1`) at each delegation.
   */
  maxDepth?: number | 'provider-managed'
}

export const Config: z<Config> = z.object({
  provider: z.string().required(),
  toolName: z.string().default('subagent'),
  modelSelectionSettings: z.boolean().default(false),
  enableRunInBackground: z.boolean().default(true),
  backgroundMode: z.union(['one-shot', 'continuable'] as const).default('one-shot'),
  // Prevent Schemastery from materializing omitted agentOptions as `{}`.
  agentOptions: z.object({
    provider: z.string(),
    model: z.string(),
    reasoningEffort: z.string().min(1) as z<ReturnType<typeof ReasoningEffortId>>,
    maxTokens: z.number().step(1).min(1).max(Number.MAX_SAFE_INTEGER),
  }).default(undefined as unknown as {
    provider: string
    model: string
    reasoningEffort: ReturnType<typeof ReasoningEffortId>
    maxTokens: number
  }),
  persona: z.string(),
  // Preserve omission; Schemastery's `{ allow: [] }` default would deny every tool.
  toolFilter: z.object({
    allow: z.array(z.string()).default(undefined as unknown as string[]),
    deny: z.array(z.string()).default(undefined as unknown as string[]),
  }).default(undefined as unknown as { allow: string[]; deny: string[] }),
  maxDepth: z.union([z.natural().max(Number.MAX_SAFE_INTEGER), z.const('provider-managed' as const)]),
})

/** Render text blocks from the canonical JSON block array without trusting arbitrary values. */
function outputValueText(values: JsonValue[]): string {
  return values
    .filter((value): value is { type: 'text'; text: string } =>
      typeof value === 'object' && value !== null && !Array.isArray(value)
      && value.type === 'text' && typeof value.text === 'string')
    .map(value => value.text)
    .join('')
}

/** Settle pending startup without rejecting the task producer contract. */
async function settleStart(start: Promise<SubagentRun>, signal: AbortSignal): Promise<JobOutcome> {
  try {
    return await settleRun(await start)
  } catch (error: unknown) {
    // Product providers aggregate startup and rollback failures. Cancellation
    // must not turn a failed cleanup into a cleanly killed Job.
    return signal.aborted && !(error instanceof AggregateError)
      ? { status: 'killed' }
      : { status: 'failed', detail: String(error) }
  }
}

/** A non-`completed` stop reason means the child did not finish cleanly. */
function stopReasonError(result: SubagentResult): string | undefined {
  switch (result.stopReason) {
    case 'completed':
      return undefined
    case 'aborted':
      return 'subagent run was cancelled'
    case 'error':
      return 'subagent run failed'
    case 'max-tokens':
      return 'subagent run hit its token limit before finishing'
    case 'refusal':
      return 'subagent declined the task'
    // Merge-extensible union: a backend may add stop reasons. Treat an unknown
    // terminal reason as a failure rather than reporting partial output as success.
    default:
      return `subagent run ended abnormally (${String(result.stopReason)})`
  }
}

/**
 * Append provider-authored failure detail and the child's preserved partial
 * answer to a stop-reason error, keeping diagnostic text separate from the
 * child's assistant output.
 * @param error - the stop-reason headline.
 * @param result - the child's terminal result.
 * @returns the headline, diagnostic, and partial text that are present.
 */
function withDiagnosticAndPartialText(error: string, result: SubagentResult): string {
  const diagnostic = result.diagnostic === undefined
    ? ''
    : `\nDiagnostic: ${result.diagnostic}`
  const text = result.output
    .filter((block): block is Extract<ContentBlock, { type: 'text' }> => block.type === 'text')
    .map(block => block.text)
    .join('')
  const partial = text.length === 0
    ? ''
    : `\nPartial output before the run ended:\n${text}`
  return `${error}${diagnostic}${partial}`
}

type ForegroundToolResult = {
  readonly kind: 'foreground'
  readonly runId: SubagentRun['id']
  readonly output: JsonValue[]
}

/**
 * Collect and release one foreground run without letting disposal replace an
 * independent result failure.
 */
async function settleForegroundRun(run: SubagentRun): Promise<ForegroundToolResult> {
  const [execution] = await Promise.allSettled([
    run.result.then((result): ForegroundToolResult => {
      const error = stopReasonError(result)
      if (error !== undefined) {
        // The registry converts this throw to isError; partial output is not
        // success, but the preserved partial answer still reaches the parent.
        throw new Error(withDiagnosticAndPartialText(error, result))
      }
      return {
        kind: 'foreground',
        runId: run.id,
        // Content blocks already cross durable JSON boundaries elsewhere;
        // the registry performs the authoritative lossless snapshot here.
        output: result.output as unknown as JsonValue[],
      }
    }),
  ])
  const [disposal] = await Promise.allSettled([Promise.resolve().then(() => run.dispose())])
  if (execution.status === 'rejected') {
    if (disposal.status === 'rejected') {
      throw new AggregateError(
        [execution.reason, disposal.reason],
        `subagent run failed: ${String(execution.reason)}; dispose failed: ${String(disposal.reason)}`,
      )
    }
    throw execution.reason
  }
  if (disposal.status === 'rejected') throw disposal.reason
  return execution.value
}

/**
 * Model-facing wording from the provider's conversation-history descriptor
 * ({@link SubagentProvider.inheritsParentContext}).
 * A fresh child needs a standalone prompt; a forked child already sees the
 * conversation's completed turns — telling the model to restate everything
 * (or, worse, that the child "does not see this conversation") would be false
 * for a fork.
 * @param inheritsConversation - whether the child's conversation is seeded
 *   with the parent's completed turns; this says nothing about tool, service,
 *   scope, or authority inheritance.
 * @returns the tool `description` and the `prompt` parameter description.
 */
function providerWording(inheritsConversation: boolean): { description: string; promptDescription: string } {
  if (inheritsConversation) {
    return {
      description:
        'Delegate a task to a subagent that inherits this conversation\'s completed turns, but not the '
        + 'current one, for work that builds on this context, such as a review or a continuation. You get '
        + 'its result, not its intermediate steps.',
      promptDescription:
        'The task. The subagent already sees this conversation\'s completed turns, so state only what is new.',
    }
  }
  return {
    description:
      'Delegate a self-contained task, such as research, a scoped implementation, or an analysis, to a '
      + 'subagent that works in its own context, so the work does not fill this conversation. You get its '
      + 'result, not its intermediate steps. It does not see this conversation, so give it a complete, '
      + 'standalone prompt.',
    promptDescription:
      'The complete, self-contained task. The subagent does not see this conversation, so include '
      + 'everything it needs.',
  }
}

interface DelegationRunRequest {
  readonly run_in_background?: boolean
}

interface DelegationRunSpec {
  readonly runInBackground: boolean
}

/** Resolve the model's optional scheduling request into one execution route. */
function resolveDelegationRun(
  request: DelegationRunRequest,
  options: { readonly backgroundEnabled: boolean; readonly continuable: boolean },
): DelegationRunSpec {
  if (!options.backgroundEnabled) {
    // The validator permits undeclared keys, so schema omission also needs
    // execution-time enforcement.
    if (request.run_in_background === true) {
      throw new Error('run_in_background is disabled for this tool instance (enableRunInBackground: false)')
    }
    return { runInBackground: false }
  }
  return {
    // Continuable work is independently scheduled unless the caller explicitly
    // needs the result before its next action. One-shot policy keeps its existing
    // foreground default because its background result requires Task collection.
    runInBackground: request.run_in_background ?? options.continuable,
  }
}

/**
 * Ask the configured task router for a route when the calling model chose none.
 * A router that is unset, slow, failing, or out of policy leaves the default
 * route in place; cancellation still propagates.
 * @param ctx - Context of the installed tool.
 * @param policy - Routes this Session may use; the router chooses among them.
 * @param args - The delegation call: its description and prompt describe the task.
 * @param signal - Tool-call cancellation signal.
 * @returns selection fields and display evidence; an absent request preserves defaults, and undefined means routing was off.
 */
async function routedRequest(
  ctx: Context,
  policy: ModelSelectionPolicy,
  args: { description: string; prompt: string },
  signal: AbortSignal,
): Promise<{ request?: DelegationModelRequest; router: SubagentRouterDecision } | undefined> {
  const settings = ctx.get('subagentModelSelection')
  const router = settings?.router()
  if (settings === undefined || router === undefined) return undefined
  try {
    const routed = await routeDelegation(
      router, `${args.description}\n\n${args.prompt}`, policy.routes, ctx.get('llm'), signal, await settings.routerToken())
    ctx.logger.info(`subagent router chose ${routed.request.provider}/${routed.request.model}`
      + `${routed.request.reasoning_effort === undefined ? '' : ` (${routed.request.reasoning_effort})`}: ${routed.reason}`)
    return { request: routed.request, router: routed.router }
  } catch (error) {
    signal.throwIfAborted()
    ctx.logger.warn(`subagent router unavailable, using the default route: ${String(error)}`)
    return { router: error instanceof RouterFallback ? error.router : { reason: 'Router unavailable; default route retained.', fallback: true } }
  }
}

/**
 * Install one delegation-tool composition.
 * @param ctx - Context that owns the registrations.
 * @param config - delegation-tool configuration.
 * @param session - unpublished Session supplied by a direct Agent setup; omit for a standing composition.
 */
export function apply(ctx: Context, config: Config, session?: Session): void {
  // Direct apply() bypasses Schemastery's numeric constraints. A direct-apply
  // omission stays capless (the schema default only runs through the loader).
  if (config.maxDepth !== 'provider-managed') assertSubagentMaxDepth(config.maxDepth)
  // Reject an empty explicit filter at load instead of failing every delegation.
  if (config.toolFilter !== undefined && config.toolFilter.allow === undefined && config.toolFilter.deny === undefined) {
    throw new Error('tool-subagent: `toolFilter` is configured but names neither `allow` nor `deny` — remove the key or fill the filter')
  }
  const backgroundEnabled = config.enableRunInBackground !== false
  const continuable = (config.backgroundMode ?? 'one-shot') === 'continuable'
  const toolName = config.toolName ?? 'subagent'

  const modelSelectionCapable = config.modelSelectionSettings === true
  ctx.sessionProjections.register(subagentModelSelectionProjectionDefinition)
  ctx.sessionProjections.register(subagentRoutingProjectionDefinition)

  const assertSubagentProviderConfiguration = (subagentProvider: SubagentProvider): void => {
    if (ctx.subagents.resolveMaxDepth(config.maxDepth) !== undefined && !subagentProvider.capabilities.depthLimit) {
      throw new Error(
        `tool-subagent: provider "${subagentProvider.name}" cannot enforce maxDepth (no depthLimit capability) — `
        + 'set maxDepth: \'provider-managed\' to leave the recursion budget to the provider',
      )
    }
    if (config.agentOptions !== undefined && !subagentProvider.capabilities.agentOptions) {
      throw new Error(
        `tool-subagent: provider "${subagentProvider.name}" does not support child agentOptions`,
      )
    }
    if (modelSelectionCapable && !subagentProvider.capabilities.agentOptions) {
      throw new Error(
        `tool-subagent: provider "${subagentProvider.name}" does not support child model selection`,
      )
    }
    if (continuable && subagentProvider.prepareContinuable === undefined) {
      throw new Error(
        `tool-subagent: provider "${subagentProvider.name}" does not support \`backgroundMode: continuable\``,
      )
    }
  }

  // Validate provider-owned config outside the optional LLM binding so an
  // invalid provider always rejects its registration or this plugin's load.
  ctx.on('subagent/provider-added', (subagentProvider) => {
    if (subagentProvider.name === config.provider) assertSubagentProviderConfiguration(subagentProvider)
  })
  const initialProvider = ctx.subagents.getProvider(config.provider)
  if (initialProvider !== undefined) assertSubagentProviderConfiguration(initialProvider)

  const install = (runtimeCtx: Context, modelSelectionPolicy: ModelSelectionPolicy | undefined): void => {
    const modelSelectionEnabled = modelSelectionPolicy !== undefined
    if (modelSelectionPolicy !== undefined) registerListSubagentModels(runtimeCtx, modelSelectionPolicy)
    // Load order and HMR replacement can change provider availability while
    // this fiber remains active.
    let mounted: { subagentProvider: SubagentProvider; disposeTool: () => void } | undefined
    const mount = (subagentProvider: SubagentProvider): void => {
      assertSubagentProviderConfiguration(subagentProvider)
      const wording = providerWording(subagentProvider.inheritsParentContext)
      const providerRouteDefaults = subagentProvider.agentRouteDefaults
      const defaultsDescription = providerRouteDefaults !== undefined
        ? 'the configured subagent defaults and this tool\'s default route'
        : 'the configured subagent defaults, filling gaps from your own route where compatible'
      const choiceDescription = !modelSelectionEnabled
        ? ''
        : ` Model choice is optional: omit \`provider\`, \`model\`, and \`reasoning_effort\` to let the host choose; it may pick an allowed model and effort suited to the task, and otherwise uses ${defaultsDescription}. To choose, look up routes and efforts with \`list_subagent_models\`, then pass \`provider\` and \`model\` together. If you change the route without \`reasoning_effort\`, the new model's default effort applies.`
          + (subagentProvider.inheritsParentContext
            ? ' Changing the route may prevent cache reuse of the inherited conversation.'
            : '')
      const disposeTool = runtimeCtx.tools.register(defineTool({
        name: toolName,
        // The description is the one home for delegation guidance: this plugin
        // registers no system-prompt section, so the policy travels with the
        // schema into every scope where the tool is visible.
        description: wording.description + (backgroundEnabled
          // The completion notice is the continuation service's own behavior, not
          // a separately installed capability, so this promise holds whenever the
          // continuable background path is reachable at all.
          ? continuable
            ? ' It runs in the background by default and returns its agent id right away. Start independent subagents in the same message and keep working while they run. When one finishes, you get a notice with its outcome and closing message. It stays available afterward: `send_message` steers it while it is running and otherwise starts a new turn. Set `run_in_background: false` only when your next step needs the result.'
            : ' This call waits for the result by default. Set `run_in_background: true` to return a job id; collect with `job_output` and stop with `job_kill`.'
          : ' This call waits for the subagent and returns its result.') + choiceDescription,
        parameters: {
          description: {
            type: 'string',
            required: true,
            description: 'A short (3-5 word) description of the delegated task, for display.',
          },
          prompt: {
            type: 'string',
            required: true,
            description: wording.promptDescription,
          },
          ...modelSelectionEnabled ? {
            provider: {
              type: 'string' as const,
              description: providerRouteDefaults !== undefined
                ? 'LLM provider for the subagent. Pass it with model; omit both to use the configured defaults or this tool\'s default route.'
                : 'LLM provider for the subagent. Pass it with model; omit both to use the configured defaults or your own route.',
            },
            model: {
              type: 'string' as const,
              description: providerRouteDefaults !== undefined
                ? 'Model id for that provider. Pass it with provider; omit both to use the configured defaults or this tool\'s default route.'
                : 'Model id for that provider. Pass it with provider; omit both to use the configured defaults or your own route.',
            },
            reasoning_effort: {
              type: 'string' as const,
              description: providerRouteDefaults !== undefined
                ? 'Reasoning effort for the subagent\'s model. Omit to use a compatible configured effort or the model\'s default.'
                : 'Reasoning effort for the subagent\'s model. Omit to use a compatible configured or inherited effort, or the new model\'s default after a route change.',
            },
          } : {},
          ...backgroundEnabled ? {
            run_in_background: {
              type: 'boolean' as const,
              description: continuable
                ? 'Run in the background and return the agent id right away. Defaults to true; set false to wait for the result when your next step needs it.'
                : 'Run as a background job and return its job id. Defaults to false; collect with job_output or stop with job_kill.',
            },
          } : {},
        },
        output: {
          schema: {
            oneOf: [
              {
                type: 'object',
                additionalProperties: false,
                properties: {
                  kind: { type: 'string', required: true, const: 'background' },
                  jobId: { type: 'string', required: true },
                },
              },
              {
                type: 'object',
                additionalProperties: false,
                properties: {
                  kind: { type: 'string', required: true, const: 'continuable' },
                  subagentId: { type: 'string', required: true },
                },
              },
              {
                type: 'object',
                additionalProperties: false,
                properties: {
                  kind: { type: 'string', required: true, const: 'foreground' },
                  runId: { type: 'string', required: true },
                  output: { type: 'array', required: true, items: { type: 'json' } },
                },
              },
            ],
          },
          render: (_args, value) => [{
            type: 'text',
            text: value.kind === 'background'
              ? `started background subagent job ${value.jobId}`
              : value.kind === 'continuable'
                ? `started subagent ${value.subagentId}`
                : outputValueText(value.output),
          }],
        },
        // Child work is independent; parent-owned job, catalog, and routing
        // facts are committed through synchronous insertions or appends.
        isConcurrencySafe: () => true,
        async execute(args, exec) {
          const parent = exec.agent
          if (!parent) {
            // Non-agent callers provide no parent for delegation ownership.
            throw new Error('subagent tool requires a calling agent (exec.agent was undefined)')
          }

          let modelRequest = withoutBlankRoute(args as DelegationModelRequest)
          let source: SubagentRoutingDecision['source'] = hasDelegationModelRequest(modelRequest) ? 'explicit' : 'default'
          let routerDecision: SubagentRouterDecision | undefined
          if (modelSelectionPolicy !== undefined && !hasDelegationModelRequest(modelRequest)) {
            const routed = await routedRequest(runtimeCtx, modelSelectionPolicy, args, exec.signal)
            if (routed !== undefined) {
              modelRequest = routed.request ?? modelRequest
              source = routed.request === undefined ? 'fallback' : 'auto'
              routerDecision = routed.router
            }
          }
          const parentOptions = parentAgentOptionsForDelegation(parent)
          const requiresRoutePreflight = hasDelegationModelRequest(modelRequest)
            || hasConfiguredLlmSelection(config.agentOptions)
          const configuredChildAgentOptions = requiresRoutePreflight && providerRouteDefaults !== undefined
            ? { ...providerRouteDefaults, ...config.agentOptions }
            : config.agentOptions
          const requestedChildAgentOptions = requestedAgentOptions(
            parentOptions,
            configuredChildAgentOptions,
            modelRequest,
            modelSelectionEnabled,
          )
          assertAllowedModelSelection(
            modelSelectionPolicy,
            parentOptions,
            requestedChildAgentOptions,
            modelRequest,
          )
          let resolvedRoute: LlmCallConfig | undefined
          if (requiresRoutePreflight) {
            const llm = runtimeCtx.get('llm')
            if (llm === undefined) {
              throw new Error('cannot resolve the selected child LLM route because the `llm` service is unavailable')
            }
            resolvedRoute = await preflightChildLlmRoute(
              llm,
              parentOptions,
              requestedChildAgentOptions,
              exec.signal,
              providerRouteDefaults === undefined,
            )
            if (runtimeCtx.subagents.getProvider(config.provider) !== subagentProvider) {
              throw new Error(`subagent provider "${config.provider}" changed while resolving the child LLM route; retry the delegation`)
            }
          }
          exec.signal.throwIfAborted()
          const maxDepth = runtimeCtx.subagents.resolveMaxDepth(config.maxDepth)
          const request = {
            label: args.description,
            prompt: [{ type: 'text', text: args.prompt }] as ContentBlock[],
            parent,
            ...requestedChildAgentOptions !== undefined ? { agentOptions: requestedChildAgentOptions } : {},
            ...config.persona !== undefined ? { persona: config.persona } : {},
            ...config.toolFilter !== undefined ? { toolFilter: config.toolFilter } : {},
            ...maxDepth !== undefined ? { maxDepth } : {},
          }

          const recordRoute = (childId: SessionId, child?: Agent): void => {
            // A local child's published options own its route. A remote route
            // is known only when preflight or provider/tool defaults named it.
            const options = child === undefined
              ? resolvedRoute ?? { ...providerRouteDefaults, ...requestedChildAgentOptions }
              : child.options
            const provider = options.provider
            const model = options.model
            const reasoningEffort = options.reasoningEffort
              ?? (resolvedRoute !== undefined && resolvedRoute.provider === provider && resolvedRoute.model === model
                ? resolvedRoute.reasoningEffort : undefined)
            try {
              recordSubagentRoutingDecision(parent.session, {
                childId, callId: exec.callId, source,
                ...provider === undefined || model === undefined ? {} : {
                  route: { provider, model, ...reasoningEffort === undefined ? {} : { reasoningEffort } },
                },
                ...routerDecision === undefined ? {} : { router: routerDecision },
              })
            } catch (error) {
              // Display evidence must not orphan a child that already started.
              runtimeCtx.logger.warn(`subagent routing decision could not be recorded: ${String(error)}`)
            }
          }

          const runSpec = resolveDelegationRun(args, { backgroundEnabled, continuable })
          if (runSpec.runInBackground) {
            if (continuable) {
              // Resolves at inbox acceptance: the child owns its own turns from
              // there, so this call neither waits for nor collects a result.
              const started = await runtimeCtx.subagents.startContinuable({
                provider: config.provider,
                label: args.description,
                request,
                signal: exec.signal,
              })
              recordRoute(started.childId, runtimeCtx.get('agents')?.get(started.childId))
              return { kind: 'continuable' as const, subagentId: started.childId }
            }
            const jobs = runtimeCtx.get('jobs')
            if (jobs === undefined) {
              throw new Error('background jobs unavailable: load @deepseek-ai/dsh-jobs and @deepseek-ai/dsh-tool-jobs')
            }
            // One-shot background child: job preflight finishes before the
            // starter can spawn, and the task-owned signal covers startup.
            const id = jobs.start({
              kind: 'subagent',
              label: args.description,
              owner: parent,
              run: () => {
                const controller = new AbortController()
                const start = runtimeCtx.subagents.start(config.provider, { ...request, signal: controller.signal }).then((run) => {
                  recordRoute(run.id, run.localAgent)
                  return run
                })
                return {
                  cancel: (reason?: string) => {
                    controller.abort(reason ?? 'background subagent task killed')
                  },
                  done: settleStart(start, controller.signal),
                  // No readOutput: the child session owns intermediate detail.
                }
              },
            })
            return { kind: 'background' as const, jobId: id }
          }

          const run: SubagentRun = await runtimeCtx.subagents.start(config.provider, {
            ...request,
            signal: exec.signal,
          })
          recordRoute(run.id, run.localAgent)
          return settleForegroundRun(run)
        },
        // Named by its short description; the prompt is the child's own
        // first message, not a headline. The result keeps generic rendering.
        presentCall: args => presentDelegationCall(args),
      }))
      mounted = { subagentProvider, disposeTool }
    }

    // Register listeners before checking presence so no synchronous change is missed.
    // TODO(subagent-dup-toolname): two waiting fibers configured with the same
    // toolName collide when their provider appears, and the duplicate-name throw
    // rolls back the provider registration. Add an intent registry if the late
    // collision occurs in a shipped composition.
    runtimeCtx.on('subagent/provider-added', (subagentProvider) => {
      if (subagentProvider.name === config.provider && mounted === undefined) mount(subagentProvider)
    })
    runtimeCtx.on('subagent/provider-removed', (name) => {
      if (name !== config.provider || mounted === undefined) return
      mounted.disposeTool()
      mounted = undefined
    })
    const present = runtimeCtx.subagents.getProvider(config.provider)
    if (present !== undefined) {
      mount(present)
    } else {
      // A backend fiber may activate later; a misspelled provider remains visible in this log.
      runtimeCtx.logger.info(`subagent provider "${config.provider}" not registered yet; the "${config.toolName ?? 'subagent'}" tool will register when it appears`)
    }
  }

  if (config.modelSelectionSettings !== true) {
    install(ctx, undefined)
    return
  }

  const settings = ctx.get('subagentModelSelection')
  if (settings === undefined) {
    throw new Error(
      'tool-subagent: `modelSelectionSettings` requires '
      + '@deepseek-ai/dsh-tool-subagent/model-selection-settings in the Host scope',
    )
  }
  const selectForSession = (target: Session): ModelSelectionPolicy | undefined => {
    const freshSession = target.firstLiveSeq === 0
      // oxlint-disable-next-line typescript/no-deprecated -- Existing Session history read; migration deferred.
      && target.eventAt(SessionSeq(0))?.type !== 'session/end-seed'
    let allowedModels = subagentModelSelectionPolicy(ctx.sessionProjections, target)
    if (allowedModels === undefined) {
      const parentId = target.header.origin === 'subagent'
        ? target.header.parentSession
        : undefined
      if (parentId !== undefined) {
        const sessions = ctx.get('sessions')
        if (sessions === undefined) {
          throw new Error('tool-subagent: child model-selection inheritance requires the Session registry')
        }
        const parent = sessions.get(parentId)
        allowedModels = parent === undefined
          ? undefined
          : subagentModelSelectionPolicy(ctx.sessionProjections, parent)
      } else if (freshSession) {
        const current = settings.current()
        allowedModels = current.enabled ? current.allowedModels : undefined
      }
    }
    if (allowedModels !== undefined) {
      recordSubagentModelSelection(ctx.sessionProjections, target, allowedModels)
    }
    return allowedModels === undefined ? undefined : { routes: allowedModels }
  }

  if (session !== undefined) {
    install(ctx, selectForSession(session))
    return
  }

  // A preset's composition covers the Agents composed under its scope; an
  // unscoped one, such as the one-shot profile's, covers every Agent.
  const compositionScope = scopeOf(ctx)
  const agents = ctx.get('agents')
  /* v8 ignore next -- shipped compositions always include the Agent registry. */
  if (agents === undefined) throw new Error('tool-subagent: standing `modelSelectionSettings` requires the Agent registry')
  const scopedInstalls = new WeakMap<Agent, ReturnType<Context['inject']>>()
  const installing = new WeakSet<Agent>()
  const belongsToComposition = (candidate: Agent): boolean =>
    compositionScope === undefined || scopeChainOf(scopeOf(candidate.ctx)).includes(compositionScope)
  const installScoped = (candidate: Agent): ReturnType<Context['inject']> | undefined => {
    const existing = scopedInstalls.get(candidate)
    if (existing !== undefined) return existing
    if (installing.has(candidate)) return
    // Reserve before the injected fiber runs: tool registration emits
    // `tools/change` synchronously, which re-enters the reconciliation below.
    installing.add(candidate)
    let fiber: ReturnType<Context['inject']>
    try {
      const policy = selectForSession(candidate.session)
      fiber = candidate.ctx.inject(['tools', 'subagents'], (runtimeCtx) => {
        install(runtimeCtx, policy)
      })
    } finally {
      installing.delete(candidate)
    }
    scopedInstalls.set(candidate, fiber)
    return fiber
  }
  const removeScoped = (candidate: Agent): void => {
    const fiber = scopedInstalls.get(candidate)
    if (fiber === undefined) return
    scopedInstalls.delete(candidate)
    /* v8 ignore next 3 -- Cordis Fiber disposal contains registration cleanup failures; this is the final diagnostic sink. */
    void fiber.dispose().catch((error: unknown) => {
      ctx.logger.warn(`tool-subagent: failed to remove recomposed Agent "${candidate.id}" definitions: ${String(error)}`)
    })
  }
  const reconcileComposedAgents = (): void => {
    for (const candidate of agents.list()) {
      if (belongsToComposition(candidate)) installScoped(candidate)
      else removeScoped(candidate)
    }
  }
  // The listener admits the composition's Agents, descendants included, and
  // installs the sampled tool definition in each Agent's own scope, so a later
  // settings change cannot mutate a live session.
  ctx.on('agent/created', async ({ agent: created }) => {
    await installScoped(created)
  })
  ctx.on('agent/disposed', ({ agent: disposed }) => { removeScoped(disposed) })
  // Reparenting an Agent between standing presets changes its inherited tool
  // set and emits `tools/change`; reconcile the Agent-owned override with the
  // new ancestry. Other registry changes are idempotent no-ops here.
  ctx.on('tools/change', reconcileComposedAgents)
  reconcileComposedAgents()
}
