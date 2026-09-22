/** Bun-only bundling shared by the product launcher and performance diagnostics. */
import { resolve } from 'node:path'

/** React compilation and runtime mode for a built TUI launch. */
export type BuildMode = 'development' | 'production'

/** Built applications use production React; source and component development remain separate. */
export const BUILD_MODE: BuildMode = 'production'

/** Environment override passed only to built application processes and their PTY drivers. */
export const BUILT_ENV = { NODE_ENV: BUILD_MODE } as const

/**
 * Bundle fork code while keeping Harness singletons and the renderer external.
 * @param entries - entry files, resolved from the caller's working directory.
 * @param outdir - private or ordinary build output directory.
 * @param mode - explicit development baseline or production compilation.
 * @returns emitted artifacts; rejects if any entry cannot be built.
 */
export async function bundle(entries: readonly string[], outdir: string, mode: BuildMode = BUILD_MODE): Promise<Bun.BuildArtifact[]> {
  const result = await Bun.build({
    entrypoints: entries.map(entry => resolve(entry)), outdir, target: 'node', format: 'esm',
    // @dsh-tui/ui is inlined; the host's packages retain their module identity.
    external: ['@deepseek-ai/*', 'ink', 'react', 'commander'], naming: '[name].js',
    jsx: { runtime: 'automatic', development: mode === 'development' },
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) }, minify: mode === 'production',
  })
  if (!result.success) throw new AggregateError(result.logs, 'TUI bundle failed')
  return result.outputs
}
