/** The built-in CLIProxyAPI setup stays discoverable before any route exists. */
import { expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { listTargets, login } from '../src/login.ts'
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
