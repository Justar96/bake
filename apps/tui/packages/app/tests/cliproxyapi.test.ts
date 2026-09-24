/** CLIProxyAPI setup and model mapping without network or credential fixtures. */
import { describe, expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { cliProxyEndpoints, cliProxyModels, configureCliProxyApi, fetchCliProxyModels } from '../src/cliproxyapi.ts'

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
    return Response.json({ models: [{ slug: 'gpt-test' }] })
  }) as typeof fetch
  const questions: string[] = []
  const count = await configureCliProxyApi(ctx, async question => {
    questions.push(question.message)
    return prompts.shift()!
  }, new AbortController().signal, labels, fetcher)
  expect(count).toBe(1)
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
