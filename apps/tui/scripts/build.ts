/** Bun-only bundling shared by the product launcher and performance diagnostics. */
import { existsSync } from 'node:fs'
import { join, resolve } from 'node:path'

/**
 * Refuse a built-profile launch when an entry file is absent.
 * @param entries - required built entry files; missing files report the build command.
 */
export function requireBuilt(entries: readonly string[]): void {
  for (const entry of entries) {
    if (!existsSync(entry)) throw new Error(`Missing built entry: ${entry}. Run bun run build from the repository root.`)
  }
}

/** React compilation and runtime mode for a built TUI launch. */
export type BuildMode = 'development' | 'production'

/** Built applications use production React; component previews use development React. */
export const BUILD_MODE: BuildMode = 'production'

/** Environment override passed only to built application processes and their PTY drivers. */
export const BUILT_ENV = { NODE_ENV: BUILD_MODE } as const

/**
 * Select Bake's data directory and production renderer without changing the caller's environment.
 * @param home - operating-system user home.
 * @param env - inherited environment; an explicit DSH_HOME remains authoritative.
 * @returns overrides for a built profile launch, isolated from upstream's default home.
 */
export function profileEnvironment(home: string, env: NodeJS.ProcessEnv): typeof BUILT_ENV & { DSH_HOME: string } {
  if (env.DSH_HOME !== undefined && env.DSH_HOME.trim() === '') throw new Error('DSH_HOME must name a directory or be unset')
  return { ...BUILT_ENV, DSH_HOME: env.DSH_HOME ?? join(home, '.bake') }
}

/**
 * Bundle Bake code while keeping runtime singletons and the renderer external.
 * @param entries - entry files, resolved from the caller's working directory.
 * @param outdir - private or ordinary build output directory.
 * @param mode - explicit development baseline or production compilation.
 * @returns emitted artifacts; rejects if any entry cannot be built.
 */
export async function bundle(entries: readonly string[], outdir: string, mode: BuildMode = BUILD_MODE): Promise<Bun.BuildArtifact[]> {
  const result = await Bun.build({
    entrypoints: entries.map(entry => resolve(entry)), outdir, target: 'node', format: 'esm',
    // @dsh-tui/ui is inlined; the host's packages retain their module identity.
    // Shiki loads each grammar by dynamic import, which an unsplit bundle
    // would inline. Every bundled language, loaded or not.
    external: ['@deepseek-ai/*', 'ink', 'react', 'commander', 'shiki'], naming: '[name].js',
    jsx: { runtime: 'automatic', development: mode === 'development' },
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) }, minify: mode === 'production',
  })
  if (!result.success) throw new AggregateError(result.logs, 'TUI bundle failed')
  return result.outputs
}
