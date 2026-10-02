import { mkdtempSync, rmSync } from 'node:fs'
import { once } from 'node:events'
import { createServer } from 'node:http'
import type { ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import { mountAgentLoopTestDependencies } from '@deepseek-ai/dsh-agent-loop-testkit'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import * as LlmPiAi from '@deepseek-ai/dsh-llm-pi-ai'
import { SessionId } from '@deepseek-ai/dsh-session'
import JsonlSessionPersistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import * as SubagentSpawn from '@deepseek-ai/dsh-subagent-spawn-in-process'
import SubagentRuntime, { type SubagentRunEndInfo } from '../src/index.ts'
import { loadStoredSession } from './persistence-helpers.ts'

const MODEL = 'deepseek-flash'
const start = { type: 'message_start', message: { id: 'msg_1', model: MODEL, usage: { input_tokens: 12, output_tokens: 1 } } }
const end = [
  { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 5 } },
  { type: 'message_stop' },
]
const sse = (events: unknown[]) => events.map(event => `event: ${(event as { type: string }).type}\ndata: ${JSON.stringify(event)}\n\n`).join('')

/** Each wire message's role and text; the adapter may merge a message's text blocks into one. */
function wireMessages(body: Record<string, unknown> | undefined): { role: string; text: string }[] {
  return (body?.messages as { role: string; content: string | { type: string; text?: string }[] }[]).map(({ role, content }) => ({
    role,
    text: typeof content === 'string' ? content : content.map(block => block.text ?? '').join(''),
  }))
}

/** Loopback Anthropic Messages endpoint that records each request and replies through `reply`. */
async function server(reply: (response: ServerResponse, count: number) => void) {
  const requests: { path: string; body: Record<string, unknown> }[] = []
  const http = createServer((request, response) => {
    void (async () => {
      const parts: Buffer[] = []
      for await (const part of request as AsyncIterable<Buffer>) parts.push(part)
      requests.push({ path: request.url!, body: JSON.parse(Buffer.concat(parts).toString()) as Record<string, unknown> })
      response.setHeader('content-type', 'text/event-stream')
      reply(response, requests.length)
    })().catch((error: unknown) => response.destroy(error as Error))
  })
  http.listen(0, '127.0.0.1')
  await once(http, 'listening')
  const address = http.address()
  if (address === null || typeof address === 'string') throw new Error('missing loopback port')
  return {
    url: `http://127.0.0.1:${address.port}/anthropic`,
    requests,
    async close() {
      const closed = new Promise<void>((resolve, reject) => http.close(error => error ? reject(error) : resolve()))
      http.closeAllConnections()
      await closed
    },
  }
}

afterEach(() => {
  vi.unstubAllEnvs()
})

it('continues the parent through default Messages after a reasoning-bearing continuable child settles', async () => {
  const root = mkdtempSync(join(tmpdir(), 'dsh-settlement-messages-'))
  const ctx = new Context()
  let http: Awaited<ReturnType<typeof server>> | undefined
  try {
    http = await server((response, count) => {
      const blocks = count === 1
        ? [
          { content_block: { type: 'thinking', thinking: '' }, delta: { type: 'thinking_delta', thinking: 'child reasoning' } },
          { content_block: { type: 'text', text: '' }, delta: { type: 'text_delta', text: 'child answer' } },
        ]
        : [{ content_block: { type: 'text', text: '' }, delta: { type: 'text_delta', text: 'parent answer' } }]
      response.end(sse([
        start,
        ...blocks.flatMap(({ content_block, delta }, index) => [
          { type: 'content_block_start', index, content_block },
          { type: 'content_block_delta', index, delta },
          { type: 'content_block_stop', index },
        ]),
        ...end,
      ]))
    })
    const { requests } = http
    vi.stubEnv('DEEPSEEK_API_KEY', 'test-key')
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(LlmPiAi, {
      providers: {
        'deepseek-official': {
          apiKeyEnv: 'DEEPSEEK_API_KEY',
          api: 'anthropic-messages',
          baseURL: http.url,
          reasoning: 'high',
          adaptiveThinkingType: 'enabled',
          compat: { forceAdaptiveThinking: true, allowEmptySignature: true },
          models: [{ id: MODEL, reasoningEfforts: { off: null, low: 'low', high: 'high', max: 'max' } }],
        },
      },
    })
    await ctx.plugin(JsonlSessionPersistence, { root })
    await ctx.plugin(AgentLoop, { agents: [] })
    await ctx.plugin(SubagentRuntime)
    await ctx.plugin(SubagentSpawn, { providerName: 'spawn' })
    const parent = await ctx.agentLoop.create(SessionId('parent'), { provider: 'deepseek-official', model: MODEL })
    const ends: SubagentRunEndInfo[] = []
    const settled = Promise.withResolvers<undefined>()
    ctx.on('subagent/end', (info) => {
      ends.push(info)
      settled.resolve(undefined)
    })

    const started = await ctx.subagents.startContinuable({
      provider: 'spawn',
      label: 'child task',
      request: { parent, prompt: [{ type: 'text', text: 'child task' }] },
      signal: new AbortController().signal,
    })
    await settled.promise
    await parent.whenIdle()

    const output = [{ type: 'reasoning', text: 'child reasoning' }, { type: 'text', text: 'child answer' }]
    expect(ends).toHaveLength(1)
    expect(ends[0]?.lastAssistantMessage).toEqual(output)
    const child = await loadStoredSession(ctx.sessionPersistence, started.childId)
    expect(child.events.filter(event => event.type === 'assistant/message').at(-1))
      .toMatchObject({ data: { message: { content: output } } })
    expect(parent.session.snapshotEvents().at(-1))
      .toMatchObject({ type: 'turn/end', data: { reason: { kind: 'completed' } } })
    expect(requests).toHaveLength(2)
    expect(requests.map(request => request.path)).toEqual(['/anthropic/v1/messages?beta=true', '/anthropic/v1/messages?beta=true'])
    const notice = parent.session.deriveMessages().find(message => message.source.kind === 'subagent-settled')
    expect(notice?.content).toEqual([
      { type: 'text', text: `Background subagent ${started.childId} finished and will do no further work unless you send it more.` },
      { type: 'text', text: 'Its closing message:' },
      { type: 'text', text: 'child answer' },
    ])
    const noticeParts = notice!.content.map(block => block.type === 'text' ? block.text : '')
    const [noticeWire] = wireMessages(requests[1]?.body)
    expect(wireMessages(requests[1]?.body)).toHaveLength(1)
    expect(noticeWire?.role).toBe('user')
    // Every notice part reaches the wire, in order.
    const offsets = noticeParts.map(part => noticeWire!.text.indexOf(part))
    expect(offsets).not.toContain(-1)
    expect(offsets).toEqual(offsets.toSorted((a, b) => a - b))

    parent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'continue' }] }))
    await parent.whenIdle()
    expect(parent.session.snapshotEvents().filter(event => event.type === 'turn/end'))
      .toMatchObject([{ data: { reason: { kind: 'completed' } } }, { data: { reason: { kind: 'completed' } } }])
    expect(requests).toHaveLength(3)
    expect(wireMessages(requests[2]?.body)).toEqual([
      noticeWire,
      { role: 'assistant', text: 'parent answer' },
      { role: 'user', text: 'continue' },
    ])
  } finally {
    try {
      await ctx.fiber.dispose()
    } finally {
      try {
        await http?.close()
      } finally {
        rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
      }
    }
  }
})
