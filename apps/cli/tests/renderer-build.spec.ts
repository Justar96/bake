import { execFile } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { describe, expect, it } from 'vitest'
import { INHERITED_NODE_ENV, selectRendererBuild } from '../src/bin.ts'

describe('selectRendererBuild', () => {
  it('loads the production renderer whatever the caller exported, and records what it was', () => {
    const unset: NodeJS.ProcessEnv = {}
    selectRendererBuild(unset)
    expect(unset).toEqual({ NODE_ENV: 'production', [INHERITED_NODE_ENV]: '-' })
    const exported: NodeJS.ProcessEnv = { NODE_ENV: 'development' }
    selectRendererBuild(exported)
    expect(exported).toEqual({ NODE_ENV: 'production', [INHERITED_NODE_ENV]: '=development' })
    // A second call keeps the caller's value, not the renderer's.
    selectRendererBuild(exported)
    expect(exported[INHERITED_NODE_ENV]).toBe('=development')
  })

  it('keeps the development build only when asked for it by name', () => {
    const env: NodeJS.ProcessEnv = { NODE_ENV: 'production', DSH_RENDERER: 'development' }
    selectRendererBuild(env)
    expect(env).toMatchObject({ NODE_ENV: 'development', [INHERITED_NODE_ENV]: '=production' })
  })

  it('retains no user-timing measures across renders, where the development build keeps one per render', async () => {
    const run = async (mode: 'select' | 'inherit') => {
      const env = { ...process.env, NODE_OPTIONS: '' }
      delete env.NODE_ENV
      Reflect.deleteProperty(env, INHERITED_NODE_ENV)
      delete env.DSH_RENDERER
      const { stdout } = await promisify(execFile)(process.execPath,
        ['--import', 'tsx/esm', fileURLToPath(new URL('./fixtures/renderer-measures.mts', import.meta.url)), mode],
        { env, timeout: 25_000, killSignal: 'SIGKILL' })
      return JSON.parse(stdout.trim().split('\n').at(-1)!) as { measures: number; nodeEnv: string | undefined }
    }
    const [selected, inherited] = await Promise.all([run('select'), run('inherit')])
    expect(selected).toEqual({ measures: 0, nodeEnv: 'production' })
    // The control proves the fixture sees the leak it guards against.
    expect(inherited.measures).toBeGreaterThan(200)
  })
})
