/**
 * Remove ambient `BAKE_*` names from every Vitest process.
 *
 * Bake reads each `BAKE_<name>` setting before its earlier `DSH_<name>`
 * spelling, and a Bake tool shell exports `BAKE_HOME` and `BAKE_SESSION_ID`
 * beside their `DSH_` names. Suites isolate themselves by setting `DSH_HOME`
 * and its siblings, so an inherited `BAKE_HOME` would point them, and every
 * child they start, back at the developer's real home. Clearing the names here
 * gives every suite the environment it was written against; a test that
 * exercises a `BAKE_` name sets it itself.
 *
 * @module
 */

/**
 * Delete every `BAKE_*` name from one environment.
 *
 * @param env - the environment to clear.
 * @returns the names that carried a value, in iteration order.
 */
export function clearAmbientBakeEnv(env: NodeJS.ProcessEnv): string[] {
  const cleared = Object.keys(env).filter(name => name.toUpperCase().startsWith('BAKE_'))
  for (const name of cleared) Reflect.deleteProperty(env, name)
  return cleared
}

clearAmbientBakeEnv(process.env)
