/**
 * Remove provider API keys from an integration run unless it opts into live
 * calls with `DSH_E2E_LIVE=1`.
 *
 * A key-gated `*.e2e.ts` test skips itself when its key is absent. A developer
 * shell that exports `DEEPSEEK_API_KEY` for daily use would otherwise turn a
 * preflight run into a paid, model-dependent one. Clearing every
 * `*_API_KEY` name here keeps the default run keyless, and the subprocesses a
 * suite spawns inherit the cleared environment.
 *
 * @module
 */

/** The opt-in that keeps the keys. */
const LIVE_E2E_ENV = 'DSH_E2E_LIVE'

/**
 * Delete every provider key from one environment unless it opts into live calls.
 * @param env - the environment to clear.
 * @returns the names that were removed, sorted.
 */
export function clearProviderKeys(env: NodeJS.ProcessEnv): string[] {
  if (env[LIVE_E2E_ENV] === '1') return []
  const cleared = Object.keys(env).filter(name => name.endsWith('_API_KEY')).sort()
  for (const name of cleared) Reflect.deleteProperty(env, name)
  return cleared
}

clearProviderKeys(process.env)
