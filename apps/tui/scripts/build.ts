/** Bun-only bundling shared by the product launcher and performance diagnostics. */
import { existsSync, mkdirSync } from 'node:fs'
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
 * @param env - inherited environment; an explicit BAKE_HOME, or else DSH_HOME, remains authoritative.
 * @returns overrides for a built profile launch, isolated from upstream's default home, naming the home under both names.
 */
export function profileEnvironment(home: string, env: NodeJS.ProcessEnv): typeof BUILT_ENV & { BAKE_HOME: string; DSH_HOME: string } {
  const name = env.BAKE_HOME !== undefined ? 'BAKE_HOME' : 'DSH_HOME'
  const configured = env[name]
  if (configured !== undefined && configured.trim() === '') throw new Error(`${name} must name a directory or be unset`)
  const selected = configured ?? join(home, '.bake')
  return { ...BUILT_ENV, BAKE_HOME: selected, DSH_HOME: selected }
}

/**
 * Start Node as the release launchers do, so the runtime watchdog can arm
 * fatal-error reports and heap snapshots: reports without environment
 * variables or network interfaces, and Node's diagnostic files in
 * `<home>/diagnostics`, created owner-only here. Node arguments rather than
 * `NODE_OPTIONS`, which the agent's subprocesses would inherit.
 * @param home - the Bake home of the launch, its BAKE_HOME.
 * @returns Node arguments to place before the entry script.
 */
export function diagnosticArguments(home: string): string[] {
  const directory = resolve(home, 'diagnostics')
  mkdirSync(resolve(home), { recursive: true })
  mkdirSync(directory, { recursive: true, mode: 0o700 })
  return ['--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${directory}`]
}

/**
 * Keep the host's `bake-*` runtime packages external, so they retain their
 * module identity, while the TUI's own `bake-tui-*` packages are inlined:
 * `bake-tui-ui` ships TypeScript sources that Node cannot load.
 */
const RUNTIME_PACKAGES_EXTERNAL: Bun.BunPlugin = {
  name: 'bake-runtime-packages-external',
  setup(build) {
    build.onResolve({ filter: /^bake-(?!tui-)/ }, ({ path }) => ({ path, external: true }))
  },
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
    // Shiki loads each grammar by dynamic import, which an unsplit bundle
    // would inline. Every bundled language, loaded or not.
    external: ['@deepseek-ai/*', 'ink', 'react', 'commander', 'shiki'], naming: '[name].js',
    plugins: [RUNTIME_PACKAGES_EXTERNAL],
    jsx: { runtime: 'automatic', development: mode === 'development' },
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) }, minify: mode === 'production',
  })
  if (!result.success) throw new AggregateError(result.logs, 'TUI bundle failed')
  return result.outputs
}
