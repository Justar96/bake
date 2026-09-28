/** CLIProxyAPI setup and model mapping without network or credential fixtures. */
import { describe, expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { cliProxyApi, cliProxyEndpoints, cliProxyModels, configureCliProxyApi, fetchCliProxyModels } from '../src/cliproxyapi.ts'

const labels = { url: 'CLIProxyAPI base URL', key: 'CLIProxyAPI API key' }

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
  const questions: string[] = []
  const count = await configureCliProxyApi(ctx, async question => {
    questions.push(question.message)
    return prompts.shift()!
  }, new AbortController().signal, labels, fetcher)
  expect(count).toBe(2)
  const [op] = (writes[1] as [string, { value: { baseURL: string, models: { id: string, api?: string, baseURL?: string }[] } }[]])[1]
  expect(op!.value.baseURL).toBe('https://proxy.example/v1')
  expect(op!.value.models).toMatchObject([{ id: 'gpt-test' },
    { id: 'claude-test', api: 'anthropic-messages', baseURL: 'https://proxy.example' }])
  expect(questions).toEqual([
    '1/2 · CLIProxyAPI base URL [http://127.0.0.1:8317]',
    '2/2 · CLIProxyAPI API key',
  ])
  expect(writes[0]).toEqual(['credential', 'test-key'])
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
  const questions: string[] = []
  const fetcher = (async (url: string) => {
    expect(url).toBe('https://proxy.example/prefix/v1/models?client_version=pi')
    return Response.json({ models: [{ slug: 'gpt-test' }] })
  }) as typeof fetch
  await configureCliProxyApi(ctx, async question => {
    questions.push(question.message)
    return question.kind === 'text' ? '' : 'new-key'
  }, new AbortController().signal, labels, fetcher)
  expect(questions[0]).toBe('1/2 · CLIProxyAPI base URL [https://proxy.example/prefix]')
})
