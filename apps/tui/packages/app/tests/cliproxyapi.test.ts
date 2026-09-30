/** CLIProxyAPI setup and model mapping without network or credential fixtures. */
import { describe, expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import {
  CliProxyCheckError, cliProxyApi, cliProxyEndpoints, cliProxyFailureText, cliProxyModels, cliProxyUpgradeNotice,
  configureCliProxyApi, fetchCliProxyModels, planCliProxyRouteUpgrade, upgradeCliProxyRoute,
} from '../src/cliproxyapi.ts'
import type { LoginPrompt } from '../src/login.ts'

const labels = dictionaries.en

describe('CLIProxyAPI endpoints', () => {
  it('accepts the published root, /v1, and /backend-api forms', () => {
    for (const input of ['127.0.0.1:8317', 'http://127.0.0.1:8317/v1', 'http://127.0.0.1:8317/backend-api']) {
      expect(cliProxyEndpoints(input)).toEqual({
        root: 'http://127.0.0.1:8317',
        models: 'http://127.0.0.1:8317/v1/models?client_version=pi',
        inference: 'http://127.0.0.1:8317/v1',
      })
    }
    expect(cliProxyEndpoints('https://proxy.example/prefix/v1').inference).toBe('https://proxy.example/prefix/v1')
  })

  it('rejects a URL carrying other request data', () => {
    expect(() => cliProxyEndpoints('https://user:pass@proxy.example')).toThrow()
    expect(() => cliProxyEndpoints('https://proxy.example?key=secret')).toThrow()
  })
})

describe('CLIProxyAPI models', () => {
  it('uses proxy slugs and capability metadata while skipping hidden entries', () => {
    expect(cliProxyModels({ models: [
      { slug: 'gpt-test', display_name: 'GPT Test', context_window: 128000, max_output_tokens: 8192,
        input_modalities: ['image'], supported_reasoning_levels: [{ effort: 'low' }, { effort: 'high' }] },
      { slug: 'hidden', visibility: 'hide' },
    ] })).toEqual([{
      id: 'gpt-test', name: 'GPT Test', contextWindow: 128000, maxTokens: 8192,
      input: ['text', 'image'], reasoningEfforts: { low: 'low', high: 'high' },
    }])
    expect(cliProxyModels({ data: [{ id: 'model-a' }] })).toEqual([{ id: 'model-a', name: 'model-a' }])
  })

  it('serves each family over the protocol it relays cleanly', () => {
    const apis = cliProxyModels({ data: ['kimi-k3', 'moonshotai/kimi-k3-256k', 'glm-5.3-flash', 'qwen3-coder-plus',
      'deepseek-v4-pro', 'MiniMax-M3', 'gpt-6-sol', 'claude-opus-5-5', 'anthropic/claude-sonnet-5', 'gemini-3.8-flash-high', 'grok-4.7']
      .map(id => ({ id })) }).map(model => [model.id, model.api])
    expect(apis).toEqual([
      ['kimi-k3', 'openai-completions'], ['moonshotai/kimi-k3-256k', 'openai-completions'],
      ['glm-5.3-flash', 'openai-completions'], ['qwen3-coder-plus', 'openai-completions'],
      ['deepseek-v4-pro', 'openai-completions'], ['MiniMax-M3', 'openai-completions'],
      ['gpt-6-sol', undefined], ['claude-opus-5-5', 'anthropic-messages'], ['anthropic/claude-sonnet-5', 'anthropic-messages'],
      ['gemini-3.8-flash-high', undefined], ['grok-4.7', undefined],
    ])
    // The listing's owner decides for an id that does not name its family.
    expect(cliProxyApi('house-model', 'Anthropic')).toBe('anthropic-messages')
    expect(cliProxyApi('house-model', 'openai')).toBe('openai-responses')
  })

  it('sends Claude to the proxy root, with adaptive thinking only where it takes an effort level', () => {
    const models = cliProxyModels({ data: [
      { id: 'claude-opus-5-5', owned_by: 'anthropic', supported_reasoning_levels: ['none', 'low', 'high', 'xhigh', 'max'] },
      { id: 'claude-opus-4-5-20251101', owned_by: 'anthropic', supported_reasoning_levels: ['none', 'low', 'high'] },
      { id: 'gpt-6-sol', owned_by: 'openai', supported_reasoning_levels: ['low', 'high'] },
    ] }, 'https://proxy.example')
    expect(models).toEqual([
      { id: 'claude-opus-5-5', api: 'anthropic-messages', baseURL: 'https://proxy.example', name: 'claude-opus-5-5',
        reasoningEfforts: { low: 'low', high: 'high', xhigh: 'xhigh', max: 'max' }, compat: { forceAdaptiveThinking: true } },
      { id: 'claude-opus-4-5-20251101', api: 'anthropic-messages', baseURL: 'https://proxy.example', name: 'claude-opus-4-5-20251101',
        reasoningEfforts: { low: 'low', high: 'high' } },
      { id: 'gpt-6-sol', name: 'gpt-6-sol', reasoningEfforts: { low: 'low', high: 'high' } },
    ])
  })

  it('leaves out models that cannot answer in text', () => {
    expect(cliProxyModels({ data: [
      { id: 'gpt-image-2', output_modalities: ['image'] },
      { id: 'gpt-6-sol', output_modalities: ['text'] },
      { id: 'unlabelled' },
    ] }).map(model => model.id)).toEqual(['gpt-6-sol', 'unlabelled'])
  })

  it('validates the model response before setup proceeds', async () => {
    const abort = new AbortController()
    const fetcher = (async (_url: string, init: RequestInit) => {
      expect(new Headers(init.headers).get('Authorization')).toBe('Bearer test-key')
      return Response.json({ data: [] })
    }) as typeof fetch
    await expect(fetchCliProxyModels('https://proxy.example/v1/models', 'test-key', abort.signal, fetcher))
      .rejects.toThrow('no selectable models')
  })
})

it('saves a validated URL and model route without placing the key in settings', async () => {
  const writes: unknown[] = []
  const ctx = {
    get(name: string) {
      if (name === 'credentials') return {
        resolve: async () => undefined,
        set: async (_ref: unknown, value: string) => { writes.push(['credential', value]) },
      }
      if (name === 'settings') return { get: () => undefined,
        mutate: async (_ns: string, ops: unknown) => { writes.push(['settings', ops]) } }
      return undefined
    },
  } as unknown as Context
  const prompts = ['https://proxy.example/v1', 'test-key']
  const fetcher = (async (url: string) => {
    expect(url).toBe('https://proxy.example/v1/models?client_version=pi')
    return Response.json({ models: [{ slug: 'gpt-test' }, { slug: 'claude-test', owned_by: 'anthropic' }] })
  }) as typeof fetch
  const questions: LoginPrompt[] = []
  const count = await configureCliProxyApi(ctx, async question => {
    questions.push(question)
    return prompts.shift()!
  }, new AbortController().signal, labels, fetcher)
  expect(count).toBe(2)
  const [op] = (writes[1] as [string, { value: { baseURL: string, models: { id: string, api?: string, baseURL?: string }[] } }[]])[1]
  expect(op!.value.baseURL).toBe('https://proxy.example/v1')
  expect(op!.value.models).toMatchObject([{ id: 'gpt-test' },
    { id: 'claude-test', api: 'anthropic-messages', baseURL: 'https://proxy.example' }])
  expect(questions).toMatchObject([
    { kind: 'text', title: 'Sign in · CLIProxyAPI', message: 'Base URL', step: { index: 1, count: 2 }, fallback: 'http://127.0.0.1:8317' },
    { kind: 'secret', message: 'API key', step: { index: 2, count: 2 }, hint: `proxy.example · ${labels.keyStoredLocally}` },
  ])
  expect(writes[0]).toEqual(['credential', 'test-key'])
  // The multi-credential gateway defaults: wait out a credential cooldown, keep a session on one credential.
  expect(op!.value).toMatchObject({
    retryPolicy: { mode: 'normal', backoff: { maxDelayMs: 60_000 } },
    compat: { sendSessionAffinityHeaders: true },
  })
  expect(JSON.stringify(writes[1])).toContain('https://proxy.example/v1')
  expect(JSON.stringify(writes[1])).toContain('openai-responses')
  expect(JSON.stringify(writes[1])).not.toContain('test-key')
})

it('restores the previous key when route settings reject a reconfiguration', async () => {
  const keys: string[] = []
  const ctx = {
    get(name: string) {
      if (name === 'credentials') return {
        resolve: async () => ({ value: 'old-key', source: 'file' }),
        set: async (_ref: unknown, value: string) => { keys.push(value) },
      }
      if (name === 'settings') return { get: () => undefined,
        mutate: async () => { throw new Error('settings rejected') } }
      return undefined
    },
  } as unknown as Context
  const answers = ['https://proxy.example', 'new-key']
  const fetcher = (async () => Response.json({ data: [{ id: 'gpt-test' }] })) as unknown as typeof fetch
  await expect(configureCliProxyApi(ctx, async () => answers.shift()!, new AbortController().signal, labels, fetcher))
    .rejects.toThrow('settings rejected')
  expect(keys).toEqual(['new-key', 'old-key'])
})

it('uses the saved connection URL when refreshing models', async () => {
  const ctx = {
    get(name: string) {
      if (name === 'credentials') return { resolve: async () => undefined, set: async () => {} }
      if (name === 'settings') return {
        get: () => ({ providers: { cliproxyapi: { baseURL: 'https://proxy.example/prefix/v1' } } }),
        mutate: async () => {},
      }
      return undefined
    },
  } as unknown as Context
  const questions: LoginPrompt[] = []
  const fetcher = (async (url: string) => {
    expect(url).toBe('https://proxy.example/prefix/v1/models?client_version=pi')
    return Response.json({ models: [{ slug: 'gpt-test' }] })
  }) as typeof fetch
  await configureCliProxyApi(ctx, async question => {
    questions.push(question)
    return question.kind === 'text' ? '' : 'new-key'
  }, new AbortController().signal, labels, fetcher)
  expect(questions[0]).toMatchObject({ fallback: 'https://proxy.example/prefix' })
})

describe('upgrading a route an earlier login wrote', () => {
  // The route `/login cliproxyapi` wrote before 0.1.7: every model on the
  // route's Responses protocol, and no multi-account defaults.
  const legacy = {
    displayName: 'CLIProxyAPI', apiKeyEnv: 'CLIPROXYAPI_API_KEY', api: 'openai-responses',
    baseURL: 'https://proxy.example/v1',
    models: [
      { id: 'gpt-test', name: 'GPT' },
      { id: 'claude-test', name: 'Claude', reasoningEfforts: { high: 'high', max: 'max' } },
      { id: 'glm-test', name: 'GLM' },
    ],
  }

  it('fills exactly what the current login writes, from the saved route alone', () => {
    const plan = planCliProxyRouteUpgrade(legacy)
    expect(plan?.changes).toEqual(['protocols', 'adaptive-thinking', 'retry', 'affinity'])
    expect(plan?.ops).toEqual([
      { op: 'set', path: ['providers', 'cliproxyapi', 'models'], value: [
        { id: 'gpt-test', name: 'GPT' },
        { id: 'claude-test', name: 'Claude', reasoningEfforts: { high: 'high', max: 'max' },
          api: 'anthropic-messages', baseURL: 'https://proxy.example', compat: { forceAdaptiveThinking: true } },
        { id: 'glm-test', name: 'GLM', api: 'openai-completions' },
      ] },
      { op: 'set', path: ['providers', 'cliproxyapi', 'retryPolicy'], value: { mode: 'normal', backoff: { maxDelayMs: 60_000 } } },
      { op: 'set', path: ['providers', 'cliproxyapi', 'compat', 'sendSessionAffinityHeaders'], value: true },
    ])
  })

  it('finds nothing to change on the route the current login writes', async () => {
    let written: unknown
    const ctx = {
      get(name: string) {
        if (name === 'credentials') return { resolve: async () => undefined, set: async () => {} }
        if (name === 'settings') return { get: () => undefined,
          mutate: async (_ns: string, ops: { value: unknown }[]) => { written = ops[0]!.value } }
        return undefined
      },
    } as unknown as Context
    const fetcher = (async (_url: string) => Response.json({ models: [{ slug: 'gpt-test' },
      { slug: 'claude-test', supported_reasoning_levels: ['high', 'max'] }, { slug: 'kimi-test' }] })) as typeof fetch
    const answers = ['https://proxy.example/v1', 'test-key']
    await configureCliProxyApi(ctx, async () => answers.shift()!, new AbortController().signal, labels, fetcher)
    expect(written).toBeDefined()
    expect(planCliProxyRouteUpgrade(written)).toBeUndefined()
  })

  it('keeps every value the user set, including an explicit opt-out', () => {
    const plan = planCliProxyRouteUpgrade({
      ...legacy,
      models: [{ id: 'claude-test', api: 'openai-responses' }],
      retryPolicy: { mode: 'normal' },
      compat: { sendSessionAffinityHeaders: false },
    })
    expect(plan).toBeUndefined()
  })

  it('leaves a route that is not a login\'s alone', () => {
    expect(planCliProxyRouteUpgrade(undefined)).toBeUndefined()
    expect(planCliProxyRouteUpgrade({ ...legacy, apiKeyEnv: 'OTHER_KEY' })).toBeUndefined()
    expect(planCliProxyRouteUpgrade({ ...legacy, api: 'openai-completions' })).toBeUndefined()
    expect(planCliProxyRouteUpgrade({ ...legacy, baseURL: 42 })).toBeUndefined()
    expect(planCliProxyRouteUpgrade({ ...legacy, models: 'gpt-test' })).toBeUndefined()
  })

  function settingsContext(settings: object | undefined): Context {
    return { get: (name: string) => name === 'settings' ? settings : undefined } as unknown as Context
  }

  it('writes the upgrade through settings and reports what changed', async () => {
    const writes: unknown[] = []
    const result = await upgradeCliProxyRoute(settingsContext({
      writable: true,
      get: () => ({ providers: { cliproxyapi: legacy } }),
      mutate: async (ns: string, ops: unknown) => { writes.push([ns, ops]) },
    }))
    expect(result).toEqual({ kind: 'upgraded', changes: ['protocols', 'adaptive-thinking', 'retry', 'affinity'] })
    expect(writes).toEqual([['llm-pi-ai', planCliProxyRouteUpgrade(legacy)!.ops]])
  })

  it('asks for a new login when settings cannot take the upgrade', async () => {
    const readOnly = await upgradeCliProxyRoute(settingsContext({
      writable: false, get: () => ({ providers: { cliproxyapi: legacy } }),
      mutate: async () => { throw new Error('unreachable') },
    }))
    expect(readOnly).toMatchObject({ kind: 'relogin', reason: 'the settings file is read-only' })
    const refused = await upgradeCliProxyRoute(settingsContext({
      writable: true, get: () => ({ providers: { cliproxyapi: legacy } }),
      mutate: async () => { throw new Error('settings rejected') },
    }))
    expect(refused).toMatchObject({ kind: 'relogin', reason: 'settings rejected' })
    expect(await upgradeCliProxyRoute(settingsContext(undefined))).toEqual({ kind: 'current' })
  })

  it('names the changes in the startup notice, in each locale', () => {
    const changes = ['retry', 'affinity'] as const
    expect(cliProxyUpgradeNotice({ kind: 'current' }, dictionaries.en)).toBeUndefined()
    expect(cliProxyUpgradeNotice({ kind: 'upgraded', changes }, dictionaries.en))
      .toBe('Updated the CLIProxyAPI route for this version: waiting out account cooldowns, one account per session')
    expect(cliProxyUpgradeNotice({ kind: 'relogin', changes, reason: 'read-only' }, dictionaries.en))
      .toBe('The CLIProxyAPI route is from an earlier version and could not be updated; '
        + 'run /login cliproxyapi for: waiting out account cooldowns, one account per session')
    expect(cliProxyUpgradeNotice({ kind: 'upgraded', changes }, dictionaries.zh))
      .toBe('已为此版本更新 CLIProxyAPI 路由：等待账号冷却结束、每个会话固定一个账号')
  })
})

describe('a proxy that does not validate', () => {
  const ctx = (): Context => ({
    get(name: string) {
      if (name === 'credentials') return { resolve: async () => undefined, set: async () => {} }
      if (name === 'settings') return { get: () => undefined, mutate: async () => {} }
      return undefined
    },
  }) as unknown as Context

  it('says which field to ask again, and words each failure', async () => {
    const signal = new AbortController().signal
    const failure = async (fetcher: typeof fetch) => {
      const error = await fetchCliProxyModels('http://127.0.0.1:18399/v1/models', 'k', signal, fetcher).catch((thrown: unknown) => thrown)
      if (!(error instanceof CliProxyCheckError)) throw new Error(`expected a check failure, got ${String(error)}`)
      return [error.field, cliProxyFailureText(error.failure, labels)]
    }
    const refused = Object.assign(new TypeError('fetch failed'), { cause: Object.assign(new Error('connect ECONNREFUSED'), { code: 'ECONNREFUSED' }) })
    expect(await failure((async () => { throw refused }) as unknown as typeof fetch))
      .toEqual(['url', 'Could not reach 127.0.0.1:18399 (ECONNREFUSED)'])
    expect(await failure((async () => { throw Object.assign(new TypeError('fetch failed'), { cause: new Error('unexpected redirect') }) }) as unknown as typeof fetch))
      .toEqual(['url', labels.cliProxyRedirect])
    expect(await failure((async () => { throw new DOMException('timed out', 'TimeoutError') }) as unknown as typeof fetch))
      .toEqual(['url', 'No answer within 15 seconds from 127.0.0.1:18399'])
    expect(await failure((async () => new Response('no', { status: 401 })) as unknown as typeof fetch))
      .toEqual(['key', 'The proxy rejected this API key (HTTP 401)'])
    expect(await failure((async () => new Response('no', { status: 404 })) as unknown as typeof fetch))
      .toEqual(['url', 'No CLIProxyAPI model list at this address (HTTP 404)'])
    expect(await failure((async () => new Response('<html>', { status: 200 })) as unknown as typeof fetch))
      .toEqual(['url', labels.cliProxyNotProxy])
  })

  it('lets the caller abort instead of reporting a failure', async () => {
    const abort = new AbortController()
    abort.abort(new Error('stopped'))
    const fetcher = (async () => { throw new TypeError('fetch failed') }) as unknown as typeof fetch
    await expect(fetchCliProxyModels('http://127.0.0.1:18399/v1/models', 'k', abort.signal, fetcher)).rejects.toThrow('fetch failed')
  })

  it('asks for the key again after a rejection, and the address again, with what was typed, after an unreachable proxy', async () => {
    const answers = ['http://proxy.example', 'wrong-key', 'right-key', 'bad url with spaces', 'http://good.example', 'right-key']
    const statuses = [401, 0, 200]
    const questions: LoginPrompt[] = []
    const fetcher = (async (url: string) => {
      const status = statuses.shift()!
      if (status === 0) throw Object.assign(new TypeError('fetch failed'), { cause: Object.assign(new Error('refused'), { code: 'ECONNREFUSED' }) })
      if (status !== 200) return new Response('no', { status })
      expect(url).toBe('http://good.example/v1/models?client_version=pi')
      return Response.json({ data: [{ id: 'gpt-test' }] })
    }) as unknown as typeof fetch
    const checked: string[] = []
    const count = await configureCliProxyApi(ctx(), async question => { questions.push(question); return answers.shift()! },
      new AbortController().signal, labels, fetcher, host => checked.push(host))
    expect(count).toBe(1)
    expect(questions.map(question => [question.kind, question.error, question.initial])).toEqual([
      ['text', undefined, undefined],
      ['secret', undefined, undefined],
      // Rejected: the key alone, fresh.
      ['secret', 'The proxy rejected this API key (HTTP 401)', undefined],
      // Unreachable: back to the address, which keeps what was typed.
      ['text', 'Could not reach proxy.example (ECONNREFUSED)', 'http://proxy.example'],
      // Unreadable address: refused before any request.
      ['text', labels.cliProxyBadUrl, 'bad url with spaces'],
      // The key survives an address problem, so Enter checks it again.
      ['secret', undefined, 'right-key'],
    ])
    expect(checked).toEqual(['proxy.example', 'proxy.example', 'good.example'])
  })

  it('ends the setup when the user declines a field', async () => {
    const fetcher = (async () => new Response('no', { status: 401 })) as unknown as typeof fetch
    const answers: (string | Error)[] = ['', 'wrong-key', new Error('declined')]
    await expect(configureCliProxyApi(ctx(), async () => {
      const next = answers.shift()!
      if (next instanceof Error) throw next
      return next
    }, new AbortController().signal, labels, fetcher)).rejects.toThrow('declined')
  })
})
