import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { HarnessError, ToolCallId } from '@deepseek-ai/dsh-llm'
import SessionStore, { type Session } from '@deepseek-ai/dsh-session'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import type { Agent } from '@deepseek-ai/dsh-agent'
import ToolRuntime, { defineTool, TOOL_DUPLICATE_CALL, type ToolExecutionToken } from '@deepseek-ai/dsh-tools'

const signal = new AbortController().signal

/** A guarded mutation whose refusal code the test controls, plus a plain read. */
async function setup() {
  const ctx = new Context()
  await ctx.plugin(SessionStore)
  await ctx.plugin(SystemPrompt)
  await ctx.plugin(ToolRuntime)
  const state = { refusal: 'FS_STALE_VERSION' as string | undefined, writes: 0, reads: 0, preExecute: 0 }
  ctx.tools.register(defineTool({
    name: 'write',
    description: 'guarded write',
    parameters: { path: { type: 'string', required: true }, content: { type: 'string', required: true } },
    output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
    async execute(args) {
      state.writes++
      if (state.refusal !== undefined) throw new HarnessError(`refused ${args.path}`, state.refusal)
      return 'written'
    },
  }))
  ctx.tools.register(defineTool({
    name: 'read',
    description: 'read',
    parameters: { path: { type: 'string', required: true } },
    output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
    isConcurrencySafe: () => true,
    async execute() {
      state.reads++
      return 'content'
    },
  }))
  ctx.on('tools/pre-execute', (_exec, next) => {
    state.preExecute++
    return next()
  })
  const phases = { postExecute: 0, results: [] as string[] }
  ctx.on('tools/post-execute', async (_exec, _result, next) => {
    phases.postExecute++
    return next()
  })
  ctx.on('tools/result', (_exec, result) => {
    phases.results.push(result.error?.info?.code ?? 'OK')
  })
  const session: Session = ctx.sessions.create()
  const agent = { session } as unknown as Agent
  let calls = 0
  const run = (name: string, args: unknown, extra: { parent?: ToolExecutionToken; agent?: Agent | undefined } = { agent }) =>
    ctx.tools.execute({ callId: ToolCallId(`c${++calls}`), name, arguments: args, signal, ...extra })
  return { ctx, state, phases, session, agent, run }
}

const WRITE = { path: '/a', content: 'x' }

describe('duplicate refused-call suppression', () => {
  it('answers an identical stale mutation with a structured error without policy or dispatch', async () => {
    const { state, phases, session, run } = await setup()
    session.append('turn/start', { turn: 1 })

    const first = await run('write', WRITE)
    expect(first).toMatchObject({ isError: true, error: { info: { code: 'FS_STALE_VERSION' } } })

    // Key order is normalized: the retry is the same call.
    const second = await run('write', { content: 'x', path: '/a' })
    expect(second).toEqual({
      isError: true,
      content: [{ type: 'text', text: 'Error: not run: this "write" call repeats one already refused this turn. Earlier refusal: refused /a' }],
      error: {
        message: 'not run: this "write" call repeats one already refused this turn. Earlier refusal: refused /a',
        info: { name: 'DuplicateToolCallError', code: TOOL_DUPLICATE_CALL },
      },
    })
    // A third repeat stays suppressed: a suppressed call changes nothing.
    expect((await run('write', WRITE)).error?.info?.code).toBe(TOOL_DUPLICATE_CALL)
    expect(state.writes).toBe(1)
    expect(state.preExecute).toBe(1)
    // Suppression skips policy and dispatch, but the final result remains observable
    // to post-execute and result listeners like any other tool outcome.
    expect(phases.postExecute).toBe(3)
    expect(phases.results).toEqual(['FS_STALE_VERSION', TOOL_DUPLICATE_CALL, TOOL_DUPLICATE_CALL])
  })

  it('suppresses an identical unread mutation and keeps other refusals across a refusal of a different call', async () => {
    const { state, session, run } = await setup()
    state.refusal = 'FS_NOT_OBSERVED'
    session.append('turn/start', { turn: 1 })

    await run('write', WRITE)
    // A different target is a different call; its refusal changes no state.
    expect((await run('write', { path: '/b', content: 'x' })).error?.info?.code).toBe('FS_NOT_OBSERVED')
    expect((await run('write', WRITE)).error?.info?.code).toBe(TOOL_DUPLICATE_CALL)
    expect(state.writes).toBe(2)
  })

  it('dispatches the retry again after any other call settles', async () => {
    const { state, session, run } = await setup()
    session.append('turn/start', { turn: 1 })

    await run('write', WRITE)
    expect((await run('read', { path: '/a' })).isError).toBe(false)
    state.refusal = undefined
    expect(await run('write', WRITE)).toMatchObject({ isError: false, content: [{ type: 'text', text: 'written' }] })
    expect(state.writes).toBe(2)

    // An unrelated failure also clears the ledger: it may have changed state.
    state.refusal = 'FS_STALE_VERSION'
    await run('write', WRITE)
    await run('missing_tool', {})
    expect((await run('write', WRITE)).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(4)
  })

  it('never caches or suppresses successful mutations or other failures', async () => {
    const { state, session, run } = await setup()
    session.append('turn/start', { turn: 1 })
    state.refusal = undefined

    await run('write', WRITE)
    await run('write', WRITE)
    expect(state.writes).toBe(2)

    state.refusal = 'EIO'
    await run('write', WRITE)
    expect((await run('write', WRITE)).error?.info?.code).toBe('EIO')
    expect(state.writes).toBe(4)
  })

  it('scopes suppression to one open turn of an agent and to model-direct calls', async () => {
    const { state, session, run } = await setup()

    // No open turn: nothing is remembered.
    await run('write', WRITE)
    await run('write', WRITE)
    expect(state.writes).toBe(2)

    session.append('turn/start', { turn: 1 })
    await run('write', WRITE)
    // Nested transport sub-dispatches and agent-less calls always dispatch.
    const parent = Symbol('parent') as ToolExecutionToken
    expect((await run('write', WRITE, { agent: undefined })).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(4)
    await run('write', WRITE)
    expect(state.writes).toBe(4)
    const agent = { session } as unknown as Agent
    expect((await run('write', WRITE, { agent, parent })).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(5)

    // The nested attempt settled, so the next direct retry dispatches.
    await run('write', WRITE)
    expect(state.writes).toBe(6)
    session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
    await run('write', WRITE)
    expect(state.writes).toBe(7)

    session.append('turn/start', { turn: 2 })
    await run('write', WRITE)
    expect(state.writes).toBe(8)
  })

  it('does not leak a refusal between sessions or alias materially different arguments', async () => {
    const { ctx, state, session, run } = await setup()
    session.append('turn/start', { turn: 1 })
    await run('write', WRITE)

    // A different path/content pair is a different call, even though it uses
    // the same tool and has the same object shape.
    expect((await run('write', { path: '/a', content: 'y' })).error?.info?.code).toBe('FS_STALE_VERSION')
    expect((await run('write', { path: '/b', content: 'x' })).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(3)

    // A separate Session has its own open-turn ledger.
    const otherSession: Session = ctx.sessions.create()
    const otherAgent = { session: otherSession } as unknown as Agent
    otherSession.append('turn/start', { turn: 1 })
    expect((await run('write', WRITE, { agent: otherAgent })).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(4)
  })

  it('does not remember FS_NOT_FOUND or malformed calls as repeatable refusals', async () => {
    const { state, session, run } = await setup()
    session.append('turn/start', { turn: 1 })
    state.refusal = 'FS_NOT_FOUND'

    expect((await run('write', WRITE)).error?.info?.code).toBe('FS_NOT_FOUND')
    expect((await run('write', WRITE)).error?.info?.code).toBe('FS_NOT_FOUND')
    expect(state.writes).toBe(2)

    // Argument materialization failures have no canonical key and therefore
    // cannot poison a later valid call with the same tool name.
    const malformed = await run('write', { path: '/a' })
    expect(malformed.isError).toBe(true)
    state.refusal = 'FS_STALE_VERSION'
    expect((await run('write', WRITE)).error?.info?.code).toBe('FS_STALE_VERSION')
    expect(state.writes).toBe(3)
  })
})
