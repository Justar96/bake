/** Every Vitest process starts without the developer's `BAKE_*` names. */
import { expect, test } from 'bun:test'
import { clearAmbientBakeEnv } from './test-bake-environment.ts'

test('clears every BAKE_ name, in any case, and leaves the rest', () => {
  const env: NodeJS.ProcessEnv = { BAKE_HOME: '/home/me/.bake', bake_session_id: 's', DSH_HOME: '/tmp/home', HOME: '/home/me' }
  expect(clearAmbientBakeEnv(env)).toEqual(['BAKE_HOME', 'bake_session_id'])
  expect(env).toEqual({ DSH_HOME: '/tmp/home', HOME: '/home/me' })
})
