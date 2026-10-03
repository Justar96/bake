import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import { PROXY_ENV_NAMES } from '../packages/util/http-proxy/src/policy.ts'
import { clearAmbientProxyEnv, TEST_PROXY_SETUP_FILE, vitestConfigFiles } from './test-proxy-environment.ts'

const ROOT = resolve(import.meta.dirname, '..')

// A runtime assertion that this process is clear would pass either way: importing the module
// above already ran it. What can actually regress is the wiring — a new Vitest project, or a
// config that lists only the invariant host — so that is what this pins, on each resolved config.
const declared = (await Promise.all(vitestConfigFiles().map(async (config) => {
  const loaded = (await import(resolve(ROOT, config)) as { default?: { test?: { setupFiles?: string | string[] } } }).default
  return { config, setupFiles: [loaded?.test?.setupFiles ?? []].flat() }
}))).filter(entry => entry.setupFiles.length > 0)

describe('ambient proxy environment', () => {
  it('clears every name the policy resolver reads, in both casings', () => {
    const env: NodeJS.ProcessEnv = {
      HTTP_PROXY: 'http://p:1', http_proxy: 'http://p:1',
      HTTPS_PROXY: 'http://p:1', https_proxy: 'http://p:1',
      ALL_PROXY: 'http://p:1', all_proxy: 'http://p:1',
      NO_PROXY: 'example.com', no_proxy: 'example.com',
      NODE_USE_ENV_PROXY: '1',
      PATH: '/usr/bin',
    }
    expect(clearAmbientProxyEnv(env)).toHaveLength(PROXY_ENV_NAMES.length + 1)
    expect(env).toEqual({ PATH: '/usr/bin' })
  })

  it('reports only the names that were set, and touches nothing else', () => {
    const env: NodeJS.ProcessEnv = { all_proxy: 'http://p:1', HOME: '/home/me' }
    expect(clearAmbientProxyEnv(env)).toEqual(['all_proxy'])
    expect(env).toEqual({ HOME: '/home/me' })
  })

  it('finds the configurations that declare a setup at all', () => {
    // Guards the discovery itself: a glob that stopped matching would make every case below vacuous.
    expect(declared.map(entry => entry.config)).toEqual(['vitest.config.ts', 'vitest.e2e.config.ts'])
  })

  it.each(declared)('$config runs the proxy setup first', ({ setupFiles }) => {
    expect(setupFiles[0]).toBe(TEST_PROXY_SETUP_FILE)
  })
})
