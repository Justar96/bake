/**
 * The model-facing `workflow` tool: run a JavaScript orchestration script that fans out
 * subagents, and return the script's final value. It owns the model-facing schema and run lifecycle; script
 * parsing, execution, caps, and cancellation live behind `ctx.workflowEngine`
 * (`@deepseek-ai/dsh-workflow`), so a hardened engine swaps in without touching what the model
 * sees. Execution awaits `run.result` and always disposes the run; non-completed reasons become tool
 * errors, and background collection remains deferred. Presentation is an args-only generic card
 * titled from `meta.name`. The explicit-ask usage policy is part of the tool description; the
 * plugin contributes no system-prompt section.
 * @module @deepseek-ai/dsh-tool-workflow
 */

import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ToolCallView, ToolResultView } from '@deepseek-ai/dsh-tools'
import type { ContentBlock } from '@deepseek-ai/dsh-llm'
import type { Session, SessionEventMap } from '@deepseek-ai/dsh-session'
import type { JsonValue } from '@deepseek-ai/dsh-util-values'
import type {
  WorkflowResult, WorkflowRun, WorkflowRunId, WorkflowStopReason,
} from '@deepseek-ai/dsh-workflow'
import type {
  ToolWorkflowAgentEndData, ToolWorkflowAgentStartData,
  ToolWorkflowRunEndData, ToolWorkflowRunStartData,
} from './types.ts'

export const name = 'tool-workflow'
export const inject = ['tools', 'workflowEngine']

/** Config: the model-facing tool name plus result rendering caps. */
export interface Config {
  /** The model-facing tool name to register (default `workflow`). */
  toolName?: string
  /** Rendered-result ceiling, in characters: a longer JSON value is truncated with a notice (default 50000). */
  maxResultChars?: number
}

export const Config: z<Config> = z.object({
  toolName: z.string().default('workflow'),
  maxResultChars: z.natural().min(1).default(50_000),
})

type ResolvedConfig = Required<Config>

interface WorkflowRecorder {
  start(session: Session, run: WorkflowRun): void
  finish(runId: WorkflowRunId, stopReason: WorkflowStopReason): void
  abandon(runId: WorkflowRunId): void
}

interface ToolWorkflowRecordEventMap {
  'tool-workflow/run-start': ToolWorkflowRunStartData
  'tool-workflow/agent-start': ToolWorkflowAgentStartData
  'tool-workflow/agent-end': ToolWorkflowAgentEndData
  'tool-workflow/run-end': ToolWorkflowRunEndData
}

/** Render a contained recording failure without trusting the thrown value. */
function renderRecordingError(error: unknown): string {
  try {
    return String(error)
  } catch {
    return '[unrenderable thrown value]'
  }
}

/**
 * Project active top-level workflow runs into their parent Sessions without
 * letting recording failure affect tool execution.
 */
function createWorkflowRecorder(ctx: Context): WorkflowRecorder {
  const active = new Map<WorkflowRunId, Session>()
  const append = <Type extends keyof ToolWorkflowRecordEventMap>(
    session: Session,
    type: Type,
    data: SessionEventMap[Type],
  ): boolean => {
    // These four package-owned events are all log-only. Narrowing the generic
    // append face here discharges Session.append's conditional options tuple.
    const appendRecord = session.append.bind(session) as <Event extends keyof ToolWorkflowRecordEventMap>(
      event: Event,
      value: SessionEventMap[Event],
    ) => void
    try {
      appendRecord(type, data)
      return true
    } catch (error: unknown) {
      ctx.logger.warn(`tool-workflow: disabled durable record after ${type} append failed: ${renderRecordingError(error)}`)
      return false
    }
  }

  ctx.on('workflow/agent-start', (info, agent) => {
    const session = active.get(info.id)
    if (session === undefined) return
    const data: ToolWorkflowAgentStartData = {
      runId: info.id,
      seq: agent.seq,
      label: agent.label,
      ...agent.phase === undefined ? {} : { phase: agent.phase },
      childId: agent.childId,
    }
    if (!append(session, 'tool-workflow/agent-start', data)) active.delete(info.id)
  })
  ctx.on('workflow/agent-end', (info, agent) => {
    const session = active.get(info.id)
    if (session === undefined) return
    const data: ToolWorkflowAgentEndData = {
      runId: info.id,
      seq: agent.seq,
      outcome: agent.outcome,
    }
    if (!append(session, 'tool-workflow/agent-end', data)) active.delete(info.id)
  })

  return {
    start(session, run) {
      if (append(session, 'tool-workflow/run-start', { runId: run.id, name: run.meta.name })) {
        active.set(run.id, session)
      }
    },
    finish(runId, stopReason) {
      const session = active.get(runId)
      if (session !== undefined) append(session, 'tool-workflow/run-end', { runId, stopReason })
      active.delete(runId)
    },
    abandon: (runId) => { active.delete(runId) },
  }
}

/**
 * The usage policy and script-authoring contract, embedded in the tool
 * description. This IS the model-facing spec: the hooks and their exact
 * semantics, the supported schema subset, failure propagation, and caps. The
 * script body, meta block, and `args` rules live on their parameters, so each
 * fact has one home.
 */
const DESCRIPTION = `Run a JavaScript workflow script that coordinates many subagents, and return the script's result. Use it only when the user explicitly asks for a workflow or for large-scale multi-agent orchestration, because one run can start many subagents; for one or two delegations, use a plain subagent call. The call blocks until the script finishes.

The script can call these globals:
- \`agent(prompt, opts?)\` runs one subagent to completion. It resolves to the subagent's final text, to an object validated against \`opts.schema\` when one is given, or to \`null\` if the subagent fails. \`opts.schema\` must be an object-rooted JSON Schema that uses only type, properties, required, additionalProperties, items, enum, const, oneOf, and annotations such as description; pattern, format, and numeric bounds are rejected. The other options are \`label\` (display name), \`phase\` (progress group, defaulting to the current phase), and \`provider\` and \`model\` (route overrides, usable separately). Any other option is an error.
- \`pipeline(items, ...stages)\` runs each item through the stages independently, with no barrier between stages, and resolves to the final values in item order. Each stage is called as \`stage(prev, item, index)\`, where \`prev\` is the previous stage's result, or the item itself for the first stage. A stage that throws turns that item into \`null\` and skips its remaining stages.
- \`parallel(thunks)\` runs zero-argument functions concurrently, waits for all of them, and resolves to their results in order; a thunk that throws yields \`null\`.
- \`phase(title)\` starts a progress phase, and \`log(message)\` reports progress.

Misusing a hook (bad arguments, unknown options, unsupported schemas, exceeded caps) or a subagent that cannot start throws an error that \`pipeline\` and \`parallel\` pass through instead of turning into \`null\`; if nothing catches it, the run fails and returns only the error. Caps limit concurrent subagents (extra \`agent()\` calls wait for a slot), total subagents per run, and items per \`pipeline()\` or \`parallel()\` call. The script has no filesystem, network, timers, or Node.js APIs; the subagents do the work.`

type WorkflowCallArgs = {
  script: string
  meta: {
    name: string
    description: string
    whenToUse?: string
    phases?: { title: string; detail?: string; provider?: string; model?: string }[]
  }
  args?: Record<string, unknown>
}

/** The pending-state card: a generic card titled by the workflow's meta name. */
function presentWorkflowCall(args: WorkflowCallArgs): ToolCallView {
  return {
    card: 'generic',
    title: `workflow: ${args.meta.name}`,
    rawInput: args.script,
  }
}

/** The completed-state card: keep the pending title; render the result content as-is. */
function presentWorkflowResult(args: WorkflowCallArgs, result: { content: ContentBlock[]; isError: boolean }): ToolResultView {
  void args
  void result
  return { card: 'generic' }
}

/** A non-`completed` stop reason means the script did not finish cleanly. */
function stopReasonError(result: WorkflowResult): string | undefined {
  switch (result.stopReason) {
    case 'completed':
      return undefined
    case 'cancelled':
      return `workflow run was cancelled${result.error !== undefined ? ` (${result.error})` : ''}`
    case 'error':
      return `workflow run failed: ${result.error ?? 'unknown error'}`
    /* v8 ignore start -- defensive: WorkflowStopReason is a closed union, exhaustive by construction; a future variant fails here loudly */
    default:
      return `workflow run ended abnormally (${String(result.stopReason satisfies never)})`
    /* v8 ignore stop */
  }
}

/** Render the run's outcome text: the meta name, agent count, and the JSON value (capped). */
function renderResult(name: string, agentsStarted: number, value: JsonValue, maxChars: number): string {
  // The engine returns JSON data (null for a valueless script), so stringify never yields undefined.
  const rendered = JSON.stringify(value, null, 2)
  const clipped = rendered.length > maxChars
    ? `${rendered.slice(0, maxChars)}\n… [truncated: ${rendered.length - maxChars} more characters]`
    : rendered
  return `workflow "${name}" completed (${agentsStarted} agent${agentsStarted === 1 ? '' : 's'}).\nReturn value:\n${clipped}`
}

export function apply(ctx: Context, config: Config): void {
  // schemastery (the exported Config schema) has already filled the defaulted
  // fields; the assertion records that resolution, not a hidden fallback.
  const { toolName, maxResultChars } = config as ResolvedConfig
  const recorder = createWorkflowRecorder(ctx)
  // The usage policy and the whole authoring contract live in the tool's own
  // schema; the plugin contributes no system-prompt section.
  ctx.tools.register(defineTool({
    name: toolName,
    description: DESCRIPTION,
    parameters: {
      script: {
        type: 'string',
        required: true,
        description: `Body of an async JavaScript function, so \`await\` works at the top level. Plain JavaScript only: no TypeScript and no import or export statements. Its return value, which must be JSON-serializable, becomes the tool result; results longer than ${maxResultChars} characters are truncated.`,
      },
      meta: {
        type: 'object',
        additionalProperties: true,
        required: true,
        description: 'The workflow\'s identity, as JSON data.',
        properties: {
          name: { type: 'string', required: true, description: 'Short kebab-case workflow name.' },
          description: { type: 'string', required: true, description: 'One-line description of what the workflow does.' },
          whenToUse: { type: 'string', description: 'Optional note on when this workflow applies.' },
          phases: {
            type: 'array',
            description: 'Optional list of the phases the script enters with phase(); informational only.',
            items: {
              type: 'object',
              additionalProperties: true,
              properties: {
                title: { type: 'string', required: true, description: 'The title the script passes to phase().' },
                detail: { type: 'string', description: 'Optional one-line description of the phase.' },
                provider: { type: 'string', description: 'Informational; pass `provider` to agent() to route a subagent.' },
                model: { type: 'string', description: 'Informational; pass `model` to agent() to choose a subagent\'s model.' },
              },
            },
          },
        },
      },
      args: {
        type: 'object',
        additionalProperties: true,
        description: 'Optional JSON object available to the script as the global `args`.',
      },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          runId: { type: 'string', required: true },
          agentsStarted: { type: 'integer', required: true },
          result: { type: 'json', required: true },
        },
      },
      render: (args, value) => [{
        type: 'text',
        text: renderResult(args.meta.name, value.agentsStarted, value.result, maxResultChars),
      }],
    },
    async execute(args, exec) {
      const parent = exec.agent
      if (!parent) {
        // The loop sets `exec.agent` for every model-driven call; its absence
        // means a non-agent caller invoked the tool directly, which has no
        // parent to attribute the children to. Fail loud rather than guess.
        throw new Error('workflow tool requires a calling agent (exec.agent was undefined)')
      }

      // Meta/body validation failures (META_INVALID/SCRIPT_PARSE) throw
      // synchronously here and become isError results via the registry — the
      // model sees the violation list and can correct the call.
      const run = ctx.workflowEngine.start({
        script: args.script,
        meta: args.meta,
        ...args.args !== undefined ? { args: args.args } : {},
        parent,
        signal: exec.signal,
      })
      const recordsRun = exec.parent === undefined
      // The engine publishes member events after start() returns and this run record is active.
      if (recordsRun) recorder.start(parent.session, run)

      // Bridge the tool's abort signal to the run: if the parent step is aborted while the
      // script is in flight, cancel the whole run. The signal also enters the engine directly, but
      // this local bridge preserves the tool contract even if an implementation ignores it.
      const onAbort = (): void => { run.cancel('parent step aborted') }
      exec.signal.addEventListener('abort', onAbort, { once: true })

      let result: WorkflowResult | undefined
      try {
        result = await run.result
        const error = stopReasonError(result)
        if (error !== undefined) {
          // Map a non-clean finish to an isError result (the registry turns a
          // throw into an isError). Report the reason, not partial output.
          throw new Error(error)
        }
        return {
          runId: run.id,
          agentsStarted: result.agentsStarted,
          result: result.value as JsonValue,
        }
      } finally {
        exec.signal.removeEventListener('abort', onAbort)
        try {
          // Keep member listeners alive through disposal: an engine may
          // synthesize cancelled member endings while reaching quiescence.
          await run.dispose()
          if (recordsRun) {
            /* v8 ignore next -- WorkflowRun.result never rejects by contract, so result is assigned before finally. */
            if (result === undefined) throw new Error('workflow run settled without a result')
            recorder.finish(run.id, result.stopReason)
          }
        } finally {
          if (recordsRun) recorder.abandon(run.id)
        }
      }
    },
    presentCall: args => presentWorkflowCall(args),
    presentResult: (args, result) => presentWorkflowResult(args, result),
  }))
}
