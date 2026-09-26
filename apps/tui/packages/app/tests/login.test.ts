/** The built-in CLIProxyAPI setup stays discoverable before any route exists. */
import { expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { availableSelection, listTargets, login } from '../src/login.ts'
import { dictionaries } from '@dsh-tui/ui/copy.ts'

it('offers CLIProxyAPI in /login before configuration and keeps cancellation local', async () => {
  const ctx = {
    get(name: string) {
      if (name === 'credentials') return { describe: async () => ({ configured: false, writable: true }) }
      if (name === 'settings') return { writable: true, get: () => undefined }
      if (name === 'llm') return { listProviders: () => [] }
      if (name === 'authorization') return { list: () => [] }
      return undefined
    },
  } as unknown as Context
  const targets = await listTargets(ctx, [])
  expect(targets).toContainEqual({
    id: 'cliproxyapi', label: 'CLIProxyAPI', configured: false, writable: true, kind: 'cliproxyapi',
  })
  const result = await login(ctx, targets, 'cliproxyapi', {
    notify: () => {}, prompt: () => Promise.reject(new Error('user dismissed')),
  }, new AbortController().signal, dictionaries.en)
  expect(result).toEqual({ kind: 'cancelled', target: 'cliproxyapi' })
})

function startup(configured: Record<string, boolean>, models: readonly string[] | Error): Context {
  return {
    get(name: string) {
      if (name === 'credentials') return { describe: async (ref: string) => ({ configured: configured[ref] ?? false, writable: true }) }
      if (name === 'settings') return { writable: true, get: () => undefined }
      if (name === 'llm') return {
        listProviders: () => [{ id: 'cliproxyapi', name: 'CLIProxyAPI' }],
        listModels: async () => {
          if (models instanceof Error) throw models
          return models.map(id => ({ provider: 'cliproxyapi', id, name: id }))
        },
      }
      if (name === 'authorization') return { list: () => [] }
      return undefined
    },
  } as unknown as Context
}

const deepseek = { provider: 'deepseek-official', model: 'deepseek-flash' }

it('starts on CLIProxyAPI when the default provider has no key', async () => {
  const ctx = startup({ CLIPROXYAPI_API_KEY: true }, ['gpt-5', 'claude'])
  expect(await availableSelection(ctx, ['DEEPSEEK_API_KEY'], deepseek)).toEqual({ provider: 'cliproxyapi', model: 'gpt-5' })
})

it('keeps the default provider when its key is set or nothing else can answer', async () => {
  expect(await availableSelection(startup({ DEEPSEEK_API_KEY: true, CLIPROXYAPI_API_KEY: true }, ['gpt-5']),
    ['DEEPSEEK_API_KEY'], deepseek)).toEqual(deepseek)
  expect(await availableSelection(startup({}, ['gpt-5']), ['DEEPSEEK_API_KEY'], deepseek)).toEqual(deepseek)
  expect(await availableSelection(startup({ CLIPROXYAPI_API_KEY: true }, []), ['DEEPSEEK_API_KEY'], deepseek)).toEqual(deepseek)
  expect(await availableSelection(startup({ CLIPROXYAPI_API_KEY: true }, new Error('offline')),
    ['DEEPSEEK_API_KEY'], deepseek)).toEqual(deepseek)
})
