import { describe, expect, it, onTestFinished, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime, { TOOL_ABORTED_BEFORE_DISPATCH } from '@deepseek-ai/dsh-tools'
import type { ToolExecutionResult, ToolExecutionToken } from '@deepseek-ai/dsh-tools'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { WorkflowRunId, WorkflowEngine } from '@deepseek-ai/dsh-workflow'
import type {
  WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowResult, WorkflowRun,
  WorkflowRunId as WorkflowRunIdType, WorkflowStartRequest,
} from '@deepseek-ai/dsh-workflow'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import SubagentRuntime from '@deepseek-ai/dsh-subagent'
import PtcWorkflowEngine from '@deepseek-ai/dsh-workflow-ptc'
import { mountWorkflowRuntime } from '../../workflow-ptc/tests/setup.ts'
import * as toolWorkflow from '../src/index.ts'
import { Session, SessionId } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'

const testToolSignal = new AbortController().signal

/** A controllable engine standing in behind ctx.workflowEngine (the tool's only seam). */
class StubEngine extends WorkflowEngine {
  requests: WorkflowStartRequest[] = []
  cancels: string[] = []
  disposed = 0
  disposeBarrier: Promise<void> | undefined
  settle!: (result: WorkflowResult) => void
  readonly settlements = new Map<WorkflowRunIdType, (result: WorkflowResult) => void>()
  startError: Error | undefined

  start(request: WorkflowStartRequest): WorkflowRun {
    if (this.startError) throw this.startError
    this.requests.push(request)
    const id = WorkflowRunId(`run-${this.requests.length}`)
    const result = new Promise<WorkflowResult>((resolve) => { this.settle = resolve })
    this.settlements.set(id, this.settle)
    request.signal?.addEventListener('abort', () => {
      this.settle({ value: null, stopReason: 'cancelled', error: 'signal', agentsStarted: 0 })
    }, { once: true })
    return {
      id,
      meta: request.meta,
      result,
      cancel: (reason?: string) => {
        this.cancels.push(reason ?? 'cancelled')
        this.settle({ value: null, stopReason: 'cancelled', ...reason !== undefined ? { error: reason } : {}, agentsStarted: 0 })
      },
      dispose: async () => {
        this.disposed += 1
        await this.disposeBarrier
        this.settlements.delete(id)
      },
    }
  }

  settleRun(id: WorkflowRunIdType, result: WorkflowResult): void {
    const settle = this.settlements.get(id)
    if (settle === undefined) throw new Error(`unknown stub workflow ${id}`)
    settle(result)
  }

  agentStart(id: WorkflowRunIdType, agent: WorkflowAgentInfo): void {
    this.emitWorkflowEvent('workflow/agent-start', {
      id,
      meta: this.requests[Number(String(id).slice(4)) - 1]!.meta,
    }, agent)
  }

  agentEnd(id: WorkflowRunIdType, agent: WorkflowAgentEndInfo): void {
    this.emitWorkflowEvent('workflow/agent-end', {
      id,
      meta: this.requests[Number(String(id).slice(4)) - 1]!.meta,
    }, agent)
  }
}

async function setup(config?: { toolName?: string; maxResultChars?: number }) {
  const ctx = new Context()
  await ctx.plugin(SystemPrompt)
  await ctx.plugin(ToolRuntime)
  await ctx.plugin(StubEngine)
  await ctx.plugin(toolWorkflow, config ?? {})
  const engine = ctx.workflowEngine as StubEngine
  const session = Session.create(SessionId('caller'))
  const parent = { id: session.id, options: {}, session } as unknown as Agent
  return { ctx, engine, parent, session }
}

const SCRIPT = 'return 1'
const META = { name: 'audit', description: 'd' }

/** The usage policy every request carries; parameter texts are pinned beside it. */
const WORKFLOW_DESCRIPTION = 'Run a JavaScript workflow script that coordinates many subagents, and return the script\'s result. Use it only when the user explicitly asks for a workflow or for large-scale multi-agent orchestration, because one run can start many subagents; for one or two delegations, use a plain subagent call. The call blocks until the script finishes. Before writing a script, call tool_help with name "workflow" for the script reference.'

/** The exact authoring contract the model reads through `tool_help`. */
const WORKFLOW_DETAILS = `The script can call these globals:
- \`agent(prompt, opts?)\` runs one subagent to completion. It resolves to the subagent's final text, to a value validated against \`opts.schema\` when one is given, or to \`null\` if the subagent fails. \`opts.schema\` is a JSON Schema with any root type, such as an object or \`{type: "string"}\`, that uses only type, properties, required, additionalProperties, items, enum, const, oneOf, and annotations such as description; pattern, format, and numeric bounds are rejected. The other options are \`label\` (display name), \`phase\` (progress group, defaulting to the current phase), and \`provider\` and \`model\` (route overrides, usable separately). Any other option is an error.
- \`pipeline(items, ...stages)\` runs each item through the stages independently, with no barrier between stages, and resolves to the final values in item order. Each stage is called as \`stage(prev, item, index)\`, where \`prev\` is the previous stage's result, or the item itself for the first stage. A stage that throws turns that item into \`null\` and skips its remaining stages.
- \`parallel(thunks)\` runs zero-argument functions concurrently, waits for all of them, and resolves to their results in order; a thunk that throws yields \`null\`.
- \`phase(title)\` starts a progress phase, and \`log(message)\` reports progress.

Misusing a hook (bad arguments, unknown options, unsupported schemas, exceeded caps) or a subagent that cannot start throws an error that \`pipeline\` and \`parallel\` pass through instead of turning into \`null\`; if nothing catches it, the run fails and returns only the error. Caps limit concurrent subagents (extra \`agent()\` calls wait for a slot), total subagents per run, and items per \`pipeline()\` or \`parallel()\` call. The script has no filesystem, network, timers, or Node.js APIs; the subagents do the work.`

function execute(ctx: Context, args: unknown, extra?: {
  agent?: Agent
  signal?: AbortSignal
  parent?: ToolExecutionToken
}): Promise<ToolExecutionResult> {
  return ctx.tools.execute({
    signal: testToolSignal,
    callId: ToolCallId('call-1'),
    name: 'workflow',
    arguments: args,
    ...extra?.agent ? { agent: extra.agent } : {},
    ...extra?.signal ? { signal: extra.signal } : {},
    ...extra?.parent ? { parent: extra.parent } : {},
  })
}

describe('dsh-tool-workflow', () => {
  it('starts a run with the script/args/parent/signal and renders the completed value', async () => {
    const { ctx, engine, parent } = await setup()
    const controller = new AbortController()
    const pending = execute(ctx, { script: SCRIPT, meta: META, args: { files: ['a.ts'] } }, { agent: parent, signal: controller.signal })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    expect(engine.requests[0]).toMatchObject({ script: SCRIPT, meta: META, args: { files: ['a.ts'] }, parent })
    expect(engine.requests[0]!.signal).toBe(controller.signal)
    engine.settle({ value: { findings: [1, 2] }, stopReason: 'completed', agentsStarted: 7 })
    const result = await pending
    expect(result.isError).toBe(false)
    if (result.isError) throw new Error('expected workflow success')
    expect(result.value).toEqual({ runId: 'run-1', agentsStarted: 7, result: { findings: [1, 2] } })
    const rendered = (result.content[0] as { text: string }).text
    expect(rendered).toContain('workflow "audit" completed (7 agents)')
    expect(rendered).toContain('"findings"')
    expect(engine.disposed).toBe(1)
  })

  it('records one top-level run and its members in the calling Session after cleanup', async () => {
    const { ctx, engine, parent, session } = await setup()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(1) })
    const runId = WorkflowRunId('run-1')
    engine.agentStart(runId, {
      seq: 1,
      label: '',
      phase: '',
      childId: SessionId('child-1'),
    })
    engine.agentEnd(runId, {
      seq: 1,
      label: '',
      phase: '',
      childId: SessionId('child-1'),
      outcome: 'completed',
    })
    engine.settleRun(runId, { value: 1, stopReason: 'completed', agentsStarted: 1 })
    expect((await pending).isError).toBe(false)
    expect(engine.disposed).toBe(1)
    expect(session.snapshotEvents().map(event => [event.type, event.data])).toEqual([
      ['tool-workflow/run-start', { runId: 'run-1', name: 'audit' }],
      ['tool-workflow/agent-start', {
        runId: 'run-1', seq: 1, label: '', phase: '', childId: 'child-1',
      }],
      ['tool-workflow/agent-end', { runId: 'run-1', seq: 1, outcome: 'completed' }],
      ['tool-workflow/run-end', { runId: 'run-1', stopReason: 'completed' }],
    ])
  })

  it('writes run-end only after run disposal reaches quiescence', async () => {
    const { ctx, engine, parent, session } = await setup()
    const barrier = Promise.withResolvers<undefined>()
    engine.disposeBarrier = barrier.promise
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(1) })
    engine.settleRun(WorkflowRunId('run-1'), {
      value: null, stopReason: 'completed', agentsStarted: 0,
    })
    await vi.waitFor(() => { expect(engine.disposed).toBe(1) })
    expect(session.snapshotEvents().map(event => event.type)).toEqual(['tool-workflow/run-start'])
    barrier.resolve(undefined)
    expect((await pending).isError).toBe(false)
    expect(session.snapshotEvents().map(event => event.type)).toEqual([
      'tool-workflow/run-start', 'tool-workflow/run-end',
    ])
  })

  it('records zero-member and concurrent runs independently', async () => {
    const { ctx, engine, parent, session } = await setup()
    const first = execute(ctx, { script: SCRIPT, meta: { ...META, name: 'first' } }, { agent: parent })
    const second = execute(ctx, { script: SCRIPT, meta: { ...META, name: 'second' } }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(2) })
    const secondId = WorkflowRunId('run-2')
    engine.agentStart(secondId, {
      seq: 1, label: 'member', childId: SessionId('child-2'),
    })
    engine.agentEnd(secondId, {
      seq: 1, label: 'member', childId: SessionId('child-2'), outcome: 'failed',
    })
    engine.settleRun(WorkflowRunId('run-1'), { value: null, stopReason: 'completed', agentsStarted: 0 })
    engine.settleRun(secondId, { value: null, stopReason: 'error', error: 'child failed', agentsStarted: 1 })
    expect((await first).isError).toBe(false)
    expect((await second).isError).toBe(true)
    expect(session.snapshotEvents().filter(event => event.type === 'tool-workflow/agent-start'))
      .toHaveLength(1)
    expect(session.snapshotEvents().filter(event => event.type === 'tool-workflow/run-end').map(event => event.data))
      .toEqual([
        { runId: 'run-1', stopReason: 'completed' },
        { runId: 'run-2', stopReason: 'error' },
      ])
  })

  it('does not record nested transport executions', async () => {
    const { ctx, engine, parent, session } = await setup()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, {
      agent: parent,
      parent: Symbol('outer') as ToolExecutionToken,
    })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(1) })
    engine.settleRun(WorkflowRunId('run-1'), { value: null, stopReason: 'completed', agentsStarted: 0 })
    expect((await pending).isError).toBe(false)
    expect(session.snapshotEvents()).toEqual([])
  })

  it.each([
    'tool-workflow/run-start',
    'tool-workflow/agent-start',
    'tool-workflow/agent-end',
    'tool-workflow/run-end',
  ] as const)('isolates a first append failure at %s and preserves a valid prefix', async (failedType) => {
    const { ctx, engine, parent, session } = await setup()
    const warnings: string[] = []
    ctx.logger.warn = ((message: unknown) => { warnings.push(String(message)) }) as typeof ctx.logger.warn
    const append = session.append.bind(session)
    session.append = ((type: Parameters<Session['append']>[0], data: never) => {
      if (type === failedType) throw new Error(`injected ${failedType} failure`)
      return append(type, data)
    }) as Session['append']

    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(1) })
    const runId = WorkflowRunId('run-1')
    engine.agentStart(runId, {
      seq: 1, label: 'member', childId: SessionId('child-1'),
    })
    engine.agentEnd(runId, {
      seq: 1, label: 'member', childId: SessionId('child-1'), outcome: 'completed',
    })
    engine.settleRun(runId, { value: null, stopReason: 'completed', agentsStarted: 1 })
    expect((await pending).isError).toBe(false)
    expect(engine.disposed).toBe(1)
    expect(warnings).toHaveLength(1)
    expect(warnings[0]).toContain(failedType)
    const types = session.snapshotEvents().map(event => event.type)
    const expectedPrefixes = {
      'tool-workflow/run-start': [],
      'tool-workflow/agent-start': ['tool-workflow/run-start'],
      'tool-workflow/agent-end': ['tool-workflow/run-start', 'tool-workflow/agent-start'],
      'tool-workflow/run-end': [
        'tool-workflow/run-start', 'tool-workflow/agent-start', 'tool-workflow/agent-end',
      ],
    } as const
    expect(types).toEqual(expectedPrefixes[failedType])
  })

  it('contains an append failure whose thrown value cannot be rendered', async () => {
    const { ctx, engine, parent, session } = await setup()
    const warnings: string[] = []
    ctx.logger.warn = ((message: unknown) => { warnings.push(String(message)) }) as typeof ctx.logger.warn
    session.append = () => {
      throw { toString: () => { throw new Error('coercion trap') } }
    }
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests).toHaveLength(1) })
    engine.settleRun(WorkflowRunId('run-1'), {
      value: null, stopReason: 'completed', agentsStarted: 0,
    })
    expect((await pending).isError).toBe(false)
    expect(warnings).toHaveLength(1)
    expect(warnings[0]).toContain('[unrenderable thrown value]')
  })

  it('maps a non-completed stop reason to an isError result (and still disposes)', async () => {
    const { ctx, engine, parent } = await setup()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    engine.settle({ value: null, stopReason: 'error', error: 'script threw: boom', agentsStarted: 2 })
    const result = await pending
    expect(result.isError).toBe(true)
    expect((result.content[0] as { text: string }).text).toContain('workflow run failed: script threw: boom')
    expect(engine.disposed).toBe(1)
  })

  it('reports a cancelled run distinctly (with and without a reason)', async () => {
    const { ctx, engine, parent } = await setup()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    engine.settle({ value: null, stopReason: 'cancelled', error: 'user', agentsStarted: 0 })
    const result = await pending
    expect(result.isError).toBe(true)
    expect((result.content[0] as { text: string }).text).toContain('workflow run was cancelled (user)')

    const bare = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(2) })
    engine.settle({ value: null, stopReason: 'cancelled', agentsStarted: 0 })
    expect(((await bare).content[0] as { text: string }).text.trim().endsWith('cancelled')).toBe(true)
  })

  it('an error result without a message renders the unknown-error fallback', async () => {
    const { ctx, engine, parent } = await setup()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    engine.settle({ value: null, stopReason: 'error', agentsStarted: 0 })
    expect(((await pending).content[0] as { text: string }).text).toContain('unknown error')
  })

  it('cancels the run when exec.signal aborts MID-FLIGHT (the abort bridge)', async () => {
    const { ctx, engine, parent } = await setup()
    const controller = new AbortController()
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent, signal: controller.signal })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    controller.abort()
    const result = await pending
    expect(result.isError).toBe(true)
    expect(engine.cancels).toContain('parent step aborted')
    expect(engine.disposed).toBe(1)
  })

  it('a synchronous engine start throw (meta/parse failure) becomes an isError result', async () => {
    const { ctx, engine, parent } = await setup()
    engine.startError = new Error('invalid meta: meta.name must be a non-empty string')
    const result = await execute(ctx, { script: 'nope', meta: { name: '', description: 'd' } }, { agent: parent })
    expect(result.isError).toBe(true)
    expect((result.content[0] as { text: string }).text).toContain('meta.name must be a non-empty string')
  })

  it('requires a calling agent (fails loud without exec.agent)', async () => {
    const { ctx, engine } = await setup()
    const result = await execute(ctx, { script: SCRIPT, meta: META })
    expect(result.isError).toBe(true)
    expect((result.content[0] as { text: string }).text).toContain('requires a calling agent')
    expect(engine.requests.length).toBe(0)
  })

  it('validates its own arguments via the schema DSL (missing script)', async () => {
    const { ctx, parent } = await setup()
    const result = await execute(ctx, {}, { agent: parent })
    expect(result.isError).toBe(true)
    expect(result.error?.info?.code).toBe('INVALID_ARGS')
  })

  it('skips workflow startup when exec.signal is already aborted', async () => {
    const { ctx, engine, parent } = await setup()
    const controller = new AbortController()
    controller.abort()
    const result = await execute(ctx, { script: SCRIPT, meta: META }, { agent: parent, signal: controller.signal })
    expect(result.isError).toBe(true)
    expect(result.error).toEqual({
      message: 'tool call aborted before dispatch',
      info: { name: 'AbortError', code: TOOL_ABORTED_BEFORE_DISPATCH },
    })
    expect(engine.requests).toHaveLength(0)
    expect(engine.cancels).toHaveLength(0)
    expect(engine.disposed).toBe(0)
  })

  it('truncates an oversized rendered value with a notice (maxResultChars)', async () => {
    const { ctx, engine, parent } = await setup({ maxResultChars: 40 })
    const pending = execute(ctx, { script: SCRIPT, meta: META }, { agent: parent })
    await vi.waitFor(() => { expect(engine.requests.length).toBe(1) })
    engine.settle({ value: { blob: 'x'.repeat(500) }, stopReason: 'completed', agentsStarted: 1 })
    const result = await pending
    if (result.isError) throw new Error('expected workflow success')
    expect(result.value).toEqual({ runId: 'run-1', agentsStarted: 1, result: { blob: 'x'.repeat(500) } })
    const rendered = (result.content[0] as { text: string }).text
    expect(rendered).toContain('[truncated:')
    expect(rendered.length).toBeLessThan(400)
  })

  it('registers under a configured toolName and unregisters on fiber dispose (HMR safety)', async () => {
    const ctx = new Context()
    onTestFinished(async () => { await ctx.fiber.dispose() })
    await ctx.plugin(SystemPrompt)
    await ctx.plugin(ToolRuntime)
    await ctx.plugin(StubEngine)
    await ctx.plugin(SessionProjectionRegistry)
    const session = Session.create(SessionId('workflow-projection-disposal'))
    const bareSections = (await ctx.systemPrompt.assemble()).sections
    const fiber = await ctx.plugin(toolWorkflow, { toolName: 'orchestrate' })
    expect(ctx.tools.get('orchestrate')).toBeDefined()
    expect(ctx.tools.get('workflow')).toBeUndefined()
    expect(ctx.sessionProjections.snapshot(session, ['workflows']).values.workflows).toEqual([])
    // The tool carries its usage policy in its description, so the plugin
    // adds no prompt section under any name.
    expect((await ctx.systemPrompt.assemble()).sections).toEqual(bareSections)
    await fiber.dispose()
    expect(ctx.tools.get('orchestrate')).toBeUndefined()
    expect(ctx.sessionProjections.snapshot(session, ['workflows']).values.workflows).toBeUndefined()
    expect((await ctx.systemPrompt.assemble()).sections).toEqual(bareSections)
  })

  it('pins the model-facing description and parameter descriptions', async () => {
    const { ctx } = await setup({ maxResultChars: 1234 })
    const schema = ctx.tools.schemas().find(s => s.name === 'workflow')!
    expect(schema.description).toBe(WORKFLOW_DESCRIPTION)
    expect(ctx.tools.get('workflow')?.details).toBe(WORKFLOW_DETAILS)
    // The registry adds its details reader beside a tool that declares details.
    expect(ctx.tools.schemas().map(s => s.name)).toContain('tool_help')
    type Field = {
      description: string
      items?: { properties: Record<string, { description: string }> }
    }
    const parameters = schema.parameters as {
      properties: Record<string, { description: string; properties?: Record<string, Field> }>
    }
    const { script, meta, args } = parameters.properties
    // The result cap is plugin config, so it is the only variable text.
    expect(script!.description).toBe('Body of an async JavaScript function, so `await` works at the top level. Plain JavaScript only: no TypeScript and no import or export statements. Its return value, which must be JSON-serializable, becomes the tool result; results longer than 1234 characters are truncated.')
    expect(meta!.description).toBe('The workflow\'s identity, as JSON data.')
    expect(args!.description).toBe('Optional JSON object available to the script as the global `args`.')
    const metaFields = meta!.properties!
    expect(Object.fromEntries(Object.entries(metaFields).map(([name, field]) => [name, field.description]))).toEqual({
      name: 'Short kebab-case workflow name.',
      description: 'One-line description of what the workflow does.',
      whenToUse: 'Optional note on when this workflow applies.',
      phases: 'Optional list of the phases the script enters with phase(); informational only.',
    })
    const phaseFields = metaFields.phases!.items!.properties
    expect(Object.fromEntries(Object.entries(phaseFields).map(([name, field]) => [name, field.description]))).toEqual({
      title: 'The title the script passes to phase().',
      detail: 'Optional one-line description of the phase.',
      provider: 'Informational; pass `provider` to agent() to route a subagent.',
      model: 'Informational; pass `model` to agent() to choose a subagent\'s model.',
    })
    expect((JSON.stringify(schema).match(/\b[A-Z]{3,}\b/g) ?? []).filter(word => word !== 'JSON')).toEqual([])
    expect((await ctx.systemPrompt.assemble()).sections.some(s => s.name.startsWith('tool:'))).toBe(false)
  })

  it('presents a generic pending card titled by the meta name, with the script as rawInput', async () => {
    const { ctx } = await setup()
    const tool = ctx.tools.get('workflow')!
    const view = tool.presentCall!({ script: SCRIPT, meta: META })
    expect(view).toMatchObject({ card: 'generic', title: 'workflow: audit', rawInput: SCRIPT })
  })

  it('presentResult keeps the generic card; presentation is pure and replay-safe on malformed args', async () => {
    const { ctx } = await setup()
    const tool = ctx.tools.get('workflow')!
    expect(tool.presentResult!({ script: SCRIPT, meta: META }, { content: [], isError: false })).toEqual({ card: 'generic' })
    // defineTool soft-validates presentation args: a malformed logged shape
    // (wrong fields entirely, or a call missing its meta) falls back to
    // undefined instead of throwing mid-replay.
    expect(tool.presentCall!({ not: 'the schema' })).toBeUndefined()
    expect(tool.presentCall!({ script: SCRIPT })).toBeUndefined()
  })

  it('has the namespace-plugin export shape (no stray default)', () => {
    expect('default' in toolWorkflow).toBe(false)
    expect(toolWorkflow.name).toBe('tool-workflow')
    expect(toolWorkflow.inject).toEqual(['tools', 'workflowEngine'])
    const loader = Object.create(Loader.prototype) as Loader
    const unwrapped = loader.unwrapExports(toolWorkflow) as Record<string, unknown>
    expect(unwrapped).toBe(toolWorkflow)
    expect(typeof unwrapped.apply).toBe('function')
  })

  describe('composition with the sandboxed PTC workflow engine', () => {
    it('an abort releases the tool even when the script parks on a promise no hook owns', async () => {
      const ctx = new Context()
      onTestFinished(async () => { await ctx.fiber.dispose() })
      await ctx.plugin(SystemPrompt)
      await ctx.plugin(ToolRuntime)
      await ctx.plugin(SessionProjectionRegistry)
      await ctx.plugin(SubagentRuntime)
      ctx.subagents.registerProvider({
        name: 'spawn',
        capabilities: { agentOptions: true, outputSchema: true, depthLimit: true, toolFilter: true, persona: true },
        inheritsParentContext: false,
        start: () => Promise.reject(new Error('the parked-script fixture must not start a child')),
      })
      await mountWorkflowRuntime(ctx)
      await ctx.plugin(PtcWorkflowEngine, {})
      await ctx.plugin(toolWorkflow, {})
      const session = Session.create(SessionId('caller'))
      const parent = { id: session.id, options: {}, session } as unknown as Agent
      const controller = new AbortController()
      const ready = Promise.withResolvers<undefined>()
      ctx.on('workflow/log', () => { ready.resolve(undefined) })
      const pending = execute(ctx, {
        script: 'log("ready"); await new Promise(() => {})\nreturn 1',
        meta: { name: 'stuck', description: 'parks forever' },
      }, { agent: parent, signal: controller.signal })
      await ready.promise
      controller.abort('user abort')
      const result = await pending
      expect(result.isError).toBe(true)
      expect((result.content[0] as { text: string }).text).toContain('cancelled')
    })
  })
})
