import { afterEach, describe, expect, it, vi } from 'vitest'
import { createServer } from 'node:http'
import type { IncomingHttpHeaders, Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { Context } from '@deepseek-ai/cordis'
import { ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type { SubagentStartRequest } from '@deepseek-ai/dsh-subagent'
import { MockAdapter } from '../../../core/agent-loop/tests/mock-adapter.ts'
import { MemoryCredentials } from '../../../credentials/credentials/tests/memory.ts'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import SubagentModelSelectionConfig from '../src/model-selection-settings.ts'
import { DEFAULT_ROUTER_URL } from '../src/model-selection-settings.ts'
import type { SubagentRouterSettings } from '../src/model-selection-settings.ts'
import { callSubagent, modelSelectionSetupAgent, setup } from './harness.ts'

const REASONING = {
  efforts: [
    { id: ReasoningEffortId('low'), name: 'Low' },
    { id: ReasoningEffortId('high'), name: 'High' },
  ],
  defaultEffort: ReasoningEffortId('high'),
} as const

interface Received {
  body: { task: string; allowed_models: { provider: string; model: string; info?: unknown }[]; priority?: string }
  headers: IncomingHttpHeaders
  method: string
  path: string
}

const servers: Server[] = []

/** A router on a test-owned port that answers with `reply`. */
async function startRouter(reply: (received: Received) => { status?: number; body?: unknown; delayMs?: number }) {
  const received: Received[] = []
  const server = createServer((request, response) => {
    let raw = ''
    request.on('data', (chunk: Buffer) => { raw += chunk.toString() })
    request.on('end', () => {
      const entry = { body: (raw === '' ? {} : JSON.parse(raw)) as Received['body'], headers: request.headers,
        method: request.method ?? '', path: request.url ?? '' }
      received.push(entry)
      const { status = 200, body, delayMs = 0 } = reply(entry)
      const timer = setTimeout(() => {
        response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(body ?? {}))
      }, delayMs)
      response.on('close', () => { clearTimeout(timer) })
    })
  })
  servers.push(server)
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve) })
  return { url: `http://127.0.0.1:${(server.address() as AddressInfo).port}`, received }
}

afterEach(async () => {
  vi.unstubAllEnvs()
  await Promise.all(servers.splice(0).map(server => new Promise<void>((resolve) => {
    server.closeAllConnections()
    server.close(() => { resolve() })
  })))
})

async function routedSetup(router: Partial<SubagentRouterSettings> & { url: string }) {
  const requests: SubagentStartRequest[] = []
  const ctx = await setup({ provider: 'mock', withModelSelection: true, router: { enabled: true, ...router } },
    { onStart: (request) => { requests.push(request) } })
  ctx.llm.registerAdapter(['alpha'], new MockAdapter([], REASONING))
  const parent = modelSelectionSetupAgent(ctx)
  ;(parent as unknown as { options: Agent['options'] }).options = { provider: 'alpha', model: 'parent-model' }
  return { ctx, requests }
}

describe('dsh-tool-subagent task router', () => {
  it('routes a call that names no model to the router\'s allowed choice and effort', async () => {
    vi.stubEnv('TEST_ROUTER_TOKEN', 's3cret')
    const router = await startRouter(() => ({
      body: { provider: 'alpha', model: 'fast-model', reasoning_effort: 'low', utility: 0.8, reason: 'cheap and able' },
    }))
    const { ctx, requests } = await routedSetup({ url: `${router.url}/`, tokenEnv: 'TEST_ROUTER_TOKEN' })

    const result = await callSubagent(ctx, { description: 'rename files', prompt: 'Rename the test files.' })

    expect(result.isError).toBe(false)
    expect(requests[0]?.agentOptions).toEqual({ provider: 'alpha', model: 'fast-model', reasoningEffort: 'low' })
    expect(router.received).toHaveLength(1)
    const [sent] = router.received
    expect(sent?.body.task).toBe('rename files\n\nRename the test files.')
    // Each route carries its own efforts, so a router that knows no model name can still pick one it accepts.
    expect(sent?.body.allowed_models).toContainEqual({
      provider: 'alpha', model: 'fast-model', info: { efforts: ['low', 'high'], default_effort: 'high' },
    })
    expect(sent?.body.priority).toBeUndefined()
    expect(sent?.headers.authorization).toBe('Bearer s3cret')
  })

  it('sends the user\'s route hints and stated priority', async () => {
    const router = await startRouter(() => ({ body: { provider: 'alpha', model: 'fast-model' } }))
    const { ctx } = await routedSetup({
      url: router.url,
      priority: 'cost',
      hints: [{ provider: 'alpha', model: 'fast-model', sameAs: 'claude-opus-4.5', cost: 'free' }],
    })

    expect((await callSubagent(ctx, { description: 'work', prompt: 'do it' })).isError).toBe(false)
    const [sent] = router.received
    expect(sent?.body.priority).toBe('cost')
    expect(sent?.body.allowed_models).toContainEqual({
      provider: 'alpha', model: 'fast-model',
      info: { efforts: ['low', 'high'], default_effort: 'high', hints: { same_as: 'claude-opus-4.5', cost: 'free' } },
    })
    expect(sent?.body.allowed_models).toContainEqual({
      provider: 'alpha', model: 'selected-model', info: { efforts: ['low', 'high'], default_effort: 'high' },
    })
  })

  it('turns an effort the chosen model does not advertise into its nearest one, or drops an unknown one', async () => {
    for (const [suggested, applied] of [['medium', 'high'], ['minimal', 'low'], ['thinking-64k', undefined]] as const) {
      const router = await startRouter(() => ({
        body: { provider: 'alpha', model: 'selected-model', reasoning_effort: suggested },
      }))
      const { ctx, requests } = await routedSetup({ url: router.url })

      expect((await callSubagent(ctx, { description: 'work', prompt: 'do it' })).isError).toBe(false)
      expect(requests[0]?.agentOptions).toEqual({
        provider: 'alpha', model: 'selected-model', ...applied === undefined ? {} : { reasoningEffort: applied },
      })
      expect(router.received[0]?.headers.authorization).toBeUndefined()
    }
  })

  it('keeps the default route when the router says its choice is only a fallback', async () => {
    const router = await startRouter(() => ({
      body: { provider: 'alpha', model: 'fast-model', reasoning_effort: 'high', fallback: true, reason: 'nothing known' },
    }))
    const { ctx, requests } = await routedSetup({ url: router.url })
    const warn = vi.spyOn(ctx.logger, 'warn')

    expect((await callSubagent(ctx, { description: 'work', prompt: 'do it' })).isError).toBe(false)
    expect(requests[0]?.agentOptions).toBeUndefined()
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('router could not tell the allowed routes apart: nothing known'))
  })

  it('keeps the default route when the router fails, is slow, or answers outside the allowlist', async () => {
    for (const reply of [
      { status: 500, body: { detail: 'down' } },
      { delayMs: 2_000, body: { provider: 'alpha', model: 'fast-model' } },
      { body: { provider: 'alpha', model: 'not-allowed-model' } },
      { body: { provider: 'alpha' } },
    ]) {
      const router = await startRouter(() => reply)
      const { ctx, requests } = await routedSetup({ url: router.url, timeoutMs: 100 })
      const warn = vi.spyOn(ctx.logger, 'warn')

      const result = await callSubagent(ctx, { description: 'work', prompt: 'do it' })

      expect(result.isError).toBe(false)
      expect(requests[0]?.agentOptions).toBeUndefined()
      expect(warn).toHaveBeenCalledWith(expect.stringContaining('subagent router unavailable, using the default route'))
    }
  })

  it('asks no router when the call chooses a route or routing is off', async () => {
    const router = await startRouter(() => ({ body: { provider: 'alpha', model: 'fast-model' } }))
    const { ctx, requests } = await routedSetup({ url: router.url })
    const chosen = await callSubagent(ctx, {
      description: 'work', prompt: 'do it', provider: 'alpha', model: 'selected-model',
    })
    expect(chosen.isError).toBe(false)
    expect(requests[0]?.agentOptions).toEqual({ provider: 'alpha', model: 'selected-model' })

    const unset = await routedSetup({ url: router.url, enabled: false })
    expect((await callSubagent(unset.ctx, { description: 'work', prompt: 'do it' })).isError).toBe(false)
    expect(unset.requests[0]?.agentOptions).toBeUndefined()
    expect(router.received).toHaveLength(0)
  })

  it('rejects a router URL that is not http or https, or routing on with no URL', async () => {
    const ctx = new Context()
    await expect(ctx.plugin(SubagentModelSelectionConfig, { router: { url: 'ftp://router' } }))
      .rejects.toThrow('subagent router `url` must be an http or https URL')
    await expect(new Context().plugin(SubagentModelSelectionConfig, { router: { enabled: true, url: '' } }))
      .rejects.toThrow('an enabled subagent router requires a `url`')
  })

  it('leaves routing off by default, pointed at the hosted router', async () => {
    const ctx = new Context()
    await ctx.plugin(SubagentModelSelectionConfig, {})
    expect(ctx.subagentModelSelection.router()).toBeUndefined()
    const on = new Context()
    await on.plugin(SubagentModelSelectionConfig, { router: { enabled: true } })
    expect(on.subagentModelSelection.router()?.url).toBe(DEFAULT_ROUTER_URL)
  })

  it('describes the allowed routes with what a delegation sends, keeping only the routes asked about', async () => {
    const router = await startRouter(() => ({ body: { routes: [
      { provider: 'alpha', model: 'fast-model', profile: 'claude-opus-4-5', matched_by: 'same_as', ranked: true,
        quality: 0.93, quality_source: 'benchmarks', price: 9.5, price_source: 'hint' },
      { provider: 'other', model: 'injected', ranked: true },
    ] } }))
    const { ctx } = await routedSetup({
      url: router.url, enabled: false, hints: [{ provider: 'alpha', model: 'fast-model', sameAs: 'claude-opus-4.5' }],
    })

    const views = await ctx.subagentModelSelection.describeRoutes(ctx.llm, AbortSignal.timeout(5000))

    const [sent] = router.received
    expect(sent?.body.allowed_models).toContainEqual({
      provider: 'alpha', model: 'fast-model',
      info: { efforts: ['low', 'high'], default_effort: 'high', hints: { same_as: 'claude-opus-4.5' } },
    })
    const allowed = ctx.subagentModelSelection.current().allowedModels
    expect(views.map(view => `${view.provider}/${view.model}`)).toEqual(allowed.map(route => `${route.provider}/${route.model}`))
    expect(views).toContainEqual({ provider: 'alpha', model: 'fast-model', profile: 'claude-opus-4-5', matchedBy: 'same_as',
      ranked: true, quality: 0.93, qualitySource: 'benchmarks', price: 9.5, priceSource: 'hint' })
    // A route the router said nothing about is unranked.
    expect(views).toContainEqual({ provider: 'alpha', model: 'selected-model', ranked: false })
  })

  it('names the token variable when the router refuses a request that sent none', async () => {
    const router = await startRouter(() => ({ status: 401 }))
    const { ctx } = await routedSetup({ url: router.url, tokenEnv: 'UNSET_ROUTER_TOKEN' })
    await expect(ctx.subagentModelSelection.describeRoutes(ctx.llm, AbortSignal.timeout(5000)))
      .rejects.toThrow('router requires a token; sign in from /settings or set UNSET_ROUTER_TOKEN')
  })

  it('signs in with an emailed code, stores the token, and sends it until signing out', async () => {
    const router = await startRouter(({ path, headers }) => {
      if (path === '/auth/email/start') return { status: 202, body: { sent: true } }
      if (path === '/auth/email/verify') return { body: { token: 'ing_abc', email: 'ada@example.com', created: true } }
      if (path === '/auth/me') return headers.authorization === 'Bearer ing_abc' ? { body: { email: 'ada@example.com' } } : { status: 401 }
      if (path === '/auth/logout') return { status: 204 }
      return { body: { routes: [] } }
    })
    const { ctx } = await routedSetup({ url: router.url, tokenEnv: 'TEST_ROUTER_TOKEN' })
    await ctx.plugin(MemoryCredentials)
    const settings = ctx.subagentModelSelection
    const signal = AbortSignal.timeout(5000)
    expect(await settings.routerTokenStatus()).toEqual({ tokenEnv: 'TEST_ROUTER_TOKEN', configured: false, writable: true })

    await settings.requestSignInCode('ada@example.com', signal)
    expect(await settings.signIn('ada@example.com', '123456', signal)).toBe('ada@example.com')

    expect(router.received[1]?.body).toEqual({ email: 'ada@example.com', code: '123456', label: 'bake' })
    expect((await ctx.credentials.resolve(credentialRef('TEST_ROUTER_TOKEN')))?.value).toBe('ing_abc')
    expect(await settings.routerTokenStatus()).toMatchObject({ configured: true, source: 'memory' })
    expect(await settings.routerAccount(signal)).toBe('ada@example.com')
    await settings.describeRoutes(ctx.llm, signal)
    expect(router.received.at(-1)?.headers.authorization).toBe('Bearer ing_abc')

    await settings.signOut(signal)
    expect(router.received.at(-1)).toMatchObject({ method: 'POST', path: '/auth/logout' })
    expect(router.received.at(-1)?.headers.authorization).toBe('Bearer ing_abc')
    expect(await settings.routerToken()).toBeUndefined()
    expect(await settings.routerAccount(signal)).toBeUndefined()
  })

  it('shows the router\'s reason when it refuses a code', async () => {
    const router = await startRouter(() => ({ status: 400, body: { detail: 'wrong or expired code' } }))
    const { ctx } = await routedSetup({ url: router.url })
    await ctx.plugin(MemoryCredentials)
    await expect(ctx.subagentModelSelection.signIn('a@b.io', '000000', AbortSignal.timeout(5000)))
      .rejects.toThrow(/^wrong or expired code$/)
    expect(await ctx.subagentModelSelection.routerToken()).toBeUndefined()
  })

  it('refuses to sign in while only the environment can hold the token', async () => {
    const router = await startRouter(() => ({ status: 202 }))
    vi.stubEnv('TEST_ROUTER_TOKEN', 'from-env')
    const { ctx } = await routedSetup({ url: router.url, tokenEnv: 'TEST_ROUTER_TOKEN' })
    expect(await ctx.subagentModelSelection.routerTokenStatus())
      .toEqual({ tokenEnv: 'TEST_ROUTER_TOKEN', configured: true, source: 'env', writable: false })
    await expect(ctx.subagentModelSelection.requestSignInCode('a@b.io', AbortSignal.timeout(5000)))
      .rejects.toThrow('no credential store is mounted; set TEST_ROUTER_TOKEN instead')
    expect(router.received).toHaveLength(0)
  })

  it('rejects a token variable that is not an environment variable name', () => {
    const ctx = new Context()
    expect(() => new SubagentModelSelectionConfig(ctx, { router: { enabled: false, url: DEFAULT_ROUTER_URL,
      tokenEnv: 'not a name', timeoutMs: 5000 } })).toThrow('`tokenEnv` must be an environment variable name')
  })
})
