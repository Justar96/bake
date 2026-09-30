/** Human decisions use the production scoped waterfalls and cancellation semantics. */
import { afterEach, expect, it, vi } from 'vitest'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { openSession } from '../src/session.ts'
import { Interactions } from '../src/interactions.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  let interactions!: Interactions
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
    interactions = new Interactions(fixture.ctx, agent, () => {})
  })
  cleanup.push(async () => { interactions.dispose(); await handle.dispose() })
  const start = async (agent: Agent) => {
    const started = Promise.withResolvers<void>()
    fixture.model.response = async function* (options) {
      started.resolve()
      await new Promise<void>(resolve => {
        if (options.signal?.aborted) resolve()
        else options.signal?.addEventListener('abort', () => resolve(), { once: true })
      })
      options.signal?.throwIfAborted()
    }
    agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Start' }], source: { kind: 'user' } }))
    await started.promise
  }
  await start(handle.agent)
  return { ...fixture, start, handle, interactions, agent: handle.agent }
}

it('queues approvals and questions, rejects stale answers, and preserves plan choices', async () => {
  const { ctx, agent, interactions } = await connected()
  const approval = ctx.approval.request({ agent, toolName: 'bash', reason: 'Write outside workspace' })
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('approval'))
  const first = interactions.current!
  expect(first.kind).toBe('approval')
  const questions = [{ id: 'plan', question: 'Review', detail: '# Full plan\nImplement this change', options: [{ label: 'Revise' }, { label: 'Implement' }], intent: { kind: 'plan-review' as const, approve: 'Implement' } }]
  const answer = ctx.userQuestions.ask({ agent, questions })
  expect(interactions.current).toBe(first)
  interactions.answer(first.id, 'allowed-once')
  await expect(approval).resolves.toBe('allowed-once')
  expect(interactions.current).toMatchObject({ kind: 'questions', questions })
  const second = interactions.current!
  interactions.answer(first.id, 'rejected')
  expect(interactions.current).toBe(second)
  interactions.answer(second.id, { answers: [{ id: 'plan', selected: ['Implement'] }] })
  await expect(answer).resolves.toEqual({ answers: [{ id: 'plan', selected: ['Implement'] }] })
  expect(interactions.current).toBeUndefined()
})

it('withdraws queued and visible requests on abort and settles every request on disposal', async () => {
  const { ctx, agent, interactions } = await connected()
  const signal = new AbortController()
  signal.abort()
  await expect(ctx.approval.request({ agent, toolName: 'bash', signal: signal.signal })).resolves.toBe('cancelled')
  expect(interactions.current).toBeUndefined()
  const first = ctx.approval.request({ agent, toolName: 'bash' })
  const abort = new AbortController()
  const queued = ctx.userQuestions.ask({ agent, questions: [{ id: 'q', question: 'Choose' }], signal: abort.signal })
  const rejected = expect(queued).rejects.toMatchObject({ code: 'ASK_ABORTED' })
  abort.abort()
  await rejected
  expect(interactions.current?.kind).toBe('approval')
  interactions.dispose()
  await expect(first).resolves.toBe('cancelled')
  expect(interactions.current).toBeUndefined()
  await expect(ctx.userQuestions.ask({ agent, questions: [{ id: 'q', question: 'Choose' }] })).rejects.toMatchObject({ code: 'NO_PROVIDER' })
})

it('delegates requests for other agents and cancels only the displayed request', async () => {
  const { ctx, agent, interactions, start } = await connected()
  const other = await openSession(ctx, {}, new AbortController().signal, () => {})
  cleanup.push(() => other.dispose())
  await start(other.agent)
  ctx.on('approval/request', (request, next) => request.agent === other.agent ? Promise.resolve('rejected') : next())
  ctx.on('user-questions/request', (request, next) => request.agent === other.agent ? Promise.resolve({ answers: [{ id: 'q', selected: [], custom: 'Elsewhere' }] }) : next())
  await expect(ctx.approval.request({ agent: other.agent, toolName: 'bash' })).resolves.toBe('rejected')
  await expect(ctx.userQuestions.ask({ agent: other.agent, questions: [{ id: 'q', question: 'Choose' }] })).resolves.toMatchObject({ answers: [{ custom: 'Elsewhere' }] })
  expect(interactions.current).toBeUndefined()
  const decision = ctx.approval.request({ agent, toolName: 'bash' })
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('approval'))
  interactions.cancel()
  await expect(decision).resolves.toBe('cancelled')
})

it('answers a picker with levels by its choice and level, and refuses a level the choice does not offer', async () => {
  const { interactions } = await connected()
  const signal = new AbortController().signal
  const prompt = { title: 'Model', initial: 'm/a', levels: { label: 'Effort', none: '-' }, choices: [
    { value: 'm/a', label: 'A', levels: { initial: '', items: [{ value: '', label: 'Default' }, { value: 'high', label: 'High' }] } },
    { value: 'm/b', label: 'B' }] }
  const leveled = interactions.pick(prompt, signal)
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('select'))
  interactions.answer(interactions.current!.id, { value: 'm/a', level: 'high' })
  await expect(leveled).resolves.toEqual({ value: 'm/a', level: 'high' })
  const plain = interactions.pick(prompt, signal)
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('select'))
  interactions.answer(interactions.current!.id, 'm/b')
  await expect(plain).resolves.toEqual({ value: 'm/b' })
  const refused = interactions.pick(prompt, signal)
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('select'))
  interactions.answer(interactions.current!.id, { value: 'm/b', level: 'high' })
  await expect(refused).rejects.toThrow('invalid picker answer')
  // A plain choose takes the value of an answer that carries a level.
  const chosen = interactions.choose(prompt, signal)
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('select'))
  interactions.answer(interactions.current!.id, { value: 'm/a', level: '' })
  await expect(chosen).resolves.toBe('m/a')
})

it('asks a select prompt with the picker and a sign-in field with its whole field, each withdrawn by its own signal', async () => {
  const { interactions } = await connected()
  const command = new AbortController()
  const chosen = interactions.prompt({ kind: 'select', title: 'Sign in \u00b7 OpenAI', message: 'Choose how to sign in',
    options: [{ id: 'oauth', label: 'Sign in with ChatGPT' }, { id: 'api-key', label: 'API key', description: 'paste one' }] }, command.signal)
  await vi.waitFor(() => expect(interactions.current).toMatchObject({ kind: 'select', title: 'Sign in \u00b7 OpenAI \u00b7 Choose how to sign in',
    initial: 'oauth', choices: [{ value: 'oauth' }, { value: 'api-key', description: 'paste one' }] }))
  interactions.answer(interactions.current!.id, 'api-key')
  await expect(chosen).resolves.toBe('api-key')
  const dismissed = interactions.prompt({ kind: 'select', message: 'Which?', options: [{ id: 'a', label: 'A' }] }, command.signal)
  await vi.waitFor(() => expect(interactions.current?.kind).toBe('select'))
  interactions.cancel()
  await expect(dismissed).rejects.toThrow('Authorization cancelled')

  const field = new AbortController()
  const typed = interactions.prompt({ kind: 'secret', title: 'Sign in \u00b7 DeepSeek', message: 'API key', hint: 'h', error: 'e',
    step: { index: 2, count: 2 }, initial: 'kept', signal: field.signal }, command.signal)
  await vi.waitFor(() => expect(interactions.current).toMatchObject({ kind: 'login', secret: true, title: 'Sign in \u00b7 DeepSeek',
    message: 'API key', hint: 'h', error: 'e', step: { index: 2, count: 2 }, initial: 'kept' }))
  expect(interactions.current).not.toHaveProperty('signal')
  // A flow withdrawing the losing question leaves the command running.
  field.abort()
  await expect(typed).rejects.toThrow('Authorization cancelled')
  expect(command.signal.aborted).toBe(false)
})
