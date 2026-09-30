import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { createScope } from '@deepseek-ai/dsh-scope'
import type { Scope } from '@deepseek-ai/dsh-scope'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import ToolRuntime, { TOOL_HELP_NAME, defineTool } from '@deepseek-ai/dsh-tools'
import type { ToolDefinition } from '@deepseek-ai/dsh-tools'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { PtcRuntime } from '@deepseek-ai/dsh-ptc-runtime'
import type { PtcRunRequest, PtcRunResult, PtcRunSpec } from '@deepseek-ai/dsh-ptc-runtime'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import type { SessionId } from '@deepseek-ai/dsh-session'

const signal = new AbortController().signal

/** A PTC runtime that only has to exist for assembly to render the SDK. */
class IdleRuntime extends PtcRuntime {
  readonly language = 'typescript'
  readonly isolation = 'none'
  resolve(request: PtcRunRequest): PtcRunSpec {
    return { ...request, cwd: request.cwd ?? process.cwd(), timeoutMs: request.timeoutMs ?? 1000 }
  }
  run(): Promise<PtcRunResult> { return Promise.resolve({ logs: [] }) }
}

async function mount(config: Record<string, unknown> = {}): Promise<Context> {
  const ctx = new Context()
  await ctx.plugin(SystemPrompt, {})
  await ctx.plugin(ToolRuntime, config)
  return ctx
}

async function agentScope(ctx: Context, name: string): Promise<{ scope: Scope; key: Agent }> {
  const key = { id: name as SessionId } as Agent
  let scope!: Scope
  await ctx.plugin(Object.assign((inner: Context) => { scope = createScope(inner, key) },
    { inject: ['tools', 'systemPrompt'] }))
  return { scope, key }
}

function tool(name: string, details?: string): ToolDefinition {
  return defineTool({
    name,
    description: `tool ${name}`,
    parameters: {},
    ...details === undefined ? {} : { details },
    output: {
      schema: { type: 'string' },
      render: (_args, value) => [{ type: 'text', text: value }],
    },
    execute: () => Promise.resolve(`ran:${name}`),
  })
}

async function help(ctx: Context, name: string, agent?: Agent) {
  const result = await ctx.tools.execute({
    signal,
    callId: ToolCallId('help-1'),
    name: TOOL_HELP_NAME,
    arguments: { name },
    ...agent === undefined ? {} : { agent },
  })
  const first = result.content[0]
  return { isError: result.isError, text: first?.type === 'text' ? first.text : '' }
}

describe('on-demand tool details', () => {
  it('keeps details out of every native schema and lists the reader only beside a tool that has them', async () => {
    const ctx = await mount()
    ctx.tools.register(tool('plain'))
    expect(ctx.tools.schemas().map(schema => schema.name)).toEqual(['plain'])

    const dispose = ctx.tools.register(tool('guided', 'Full guide.'))
    const schemas = ctx.tools.schemas()
    expect(schemas.map(schema => schema.name)).toEqual(['plain', 'guided', TOOL_HELP_NAME])
    expect(JSON.stringify(schemas)).not.toContain('Full guide.')
    const assembly = await ctx.systemPrompt.assemble()
    expect(assembly.tools.map(schema => schema.name).sort()).toEqual(['guided', 'plain', TOOL_HELP_NAME])

    dispose()
    expect(ctx.tools.schemas().map(schema => schema.name)).toEqual(['plain'])
  })

  it('returns the details of a visible tool and refuses a tool without them', async () => {
    const ctx = await mount()
    ctx.tools.register(tool('plain'))
    ctx.tools.register(tool('guided', 'Full guide.'))
    expect(await help(ctx, 'guided')).toEqual({ isError: false, text: 'Full guide.' })
    expect(await help(ctx, 'plain')).toMatchObject({ isError: true })
    expect((await help(ctx, 'plain')).text).toContain('tool "plain" has no usage reference')
    expect((await help(ctx, 'missing')).text).toContain('tool "missing" has no usage reference')
  })

  it('follows the calling scope, so a restricted or foreign tool has no details to read', async () => {
    const ctx = await mount()
    const a = await agentScope(ctx, 'a')
    const b = await agentScope(ctx, 'b')
    ctx.tools.register(tool('guided', 'Global guide.'))
    a.scope.ctx.tools.register(tool('mine', 'Scoped guide.'))
    b.scope.ctx.tools.restrict({ deny: ['guided'] })

    expect(await help(ctx, 'mine', a.key)).toEqual({ isError: false, text: 'Scoped guide.' })
    expect((await help(ctx, 'mine', b.key)).isError).toBe(true)
    // With its only detailed tool masked, scope b loses the reader as well.
    expect(ctx.tools.schemas(b.key).map(schema => schema.name)).not.toContain(TOOL_HELP_NAME)
    expect((await help(ctx, 'guided', b.key)).isError).toBe(true)
  })

  it('reserves the name and validates the field', async () => {
    const ctx = await mount()
    const { scope } = await agentScope(ctx, 'a')
    expect(() => ctx.tools.register(tool(TOOL_HELP_NAME))).toThrow(/reserved/)
    expect(() => ctx.tools.register(tool('blank', '  '))).toThrow(/details must be a non-empty string/)
    expect(() => scope.ctx.tools.restrict({ deny: [TOOL_HELP_NAME] })).toThrow(/cannot name reserved "tool_help"/)
  })

  it('appends details to the PTC mode SDK instead of sending the reader', async () => {
    const ctx = new Context()
    await ctx.plugin(SystemPrompt, {})
    await ctx.plugin(ToolRuntime, { mode: 'ptc' })
    await ctx.plugin(IdleRuntime)
    ctx.tools.register(tool('guided', 'Full guide.'))
    const assembly = await ctx.systemPrompt.assemble()
    expect(assembly.tools.map(schema => schema.name)).toEqual(['run_code'])
    const sdk = assembly.sections.find(section => section.name === 'tools:sdk')
    expect(sdk?.text).toContain('Full guide.')
    expect(sdk?.text).not.toContain(`${TOOL_HELP_NAME}:`)
  })
})
