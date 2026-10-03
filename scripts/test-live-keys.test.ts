import { afterAll, beforeAll, describe, expect, it } from 'bun:test'

// Loading the module clears this process's keys, so opt in first and restore after.
const saved = process.env.DSH_E2E_LIVE
let clearProviderKeys: typeof import('./test-live-keys.ts').clearProviderKeys
beforeAll(async () => {
  process.env.DSH_E2E_LIVE = '1'
  ;({ clearProviderKeys } = await import('./test-live-keys.ts'))
})
afterAll(() => {
  if (saved === undefined) delete process.env.DSH_E2E_LIVE
  else process.env.DSH_E2E_LIVE = saved
})

describe('clearProviderKeys', () => {
  it('removes every provider key and leaves the rest', () => {
    const env: NodeJS.ProcessEnv = { DEEPSEEK_API_KEY: 'a', OPENAI_API_KEY: 'b', PATH: '/bin', DSH_HOME: '/h' }
    expect(clearProviderKeys(env)).toEqual(['DEEPSEEK_API_KEY', 'OPENAI_API_KEY'])
    expect(env).toEqual({ PATH: '/bin', DSH_HOME: '/h' })
  })

  it('keeps the keys when the run opts into live calls', () => {
    const env: NodeJS.ProcessEnv = { DEEPSEEK_API_KEY: 'a', DSH_E2E_LIVE: '1' }
    expect(clearProviderKeys(env)).toEqual([])
    expect(env.DEEPSEEK_API_KEY).toBe('a')
  })

  it('treats any other opt-in value as keyless', () => {
    const env: NodeJS.ProcessEnv = { DEEPSEEK_API_KEY: 'a', DSH_E2E_LIVE: 'true' }
    expect(clearProviderKeys(env)).toEqual(['DEEPSEEK_API_KEY'])
  })
})
