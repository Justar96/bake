/**
 * Command-line dispatch for dsh, loaded by the `bin` entry once the module
 * compile cache is on.
 * @module bake-cli/cli
 */

/* v8 ignore file -- built-bin acceptance exercises this dispatch. */

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { loadLayeredEnv, StartupError } from 'bake-app-boot'
import { resolveDshHome } from 'bake-home-paths'
import { parseDshArgs } from './args.ts'
import { reportStartupFailure } from './startup-diagnostics.ts'

// Both the source tree (apps/cli/src) and the bundled bin (apps/cli/lib) sit
// one directory under apps/cli, so the checked-in manifest resolves with the
// same relative hop from either artifact.
function readVersion(): string {
  const manifest = JSON.parse(
    readFileSync(fileURLToPath(new URL('../package.json', import.meta.url)), 'utf8'),
  ) as { version?: unknown }
  return typeof manifest.version === 'string' ? manifest.version : '0.0.0'
}

/**
 * Holds the caller's own `NODE_ENV` once the renderer's build is chosen:
 * `=` and the value, or `-` when it was unset. The subprocess scrub reads it
 * back, so a command the agent runs sees the user's value rather than the
 * renderer's.
 */
export const INHERITED_NODE_ENV = 'DSH_INHERITED_NODE_ENV'

/**
 * Load React's production build for this process, whatever `NODE_ENV` the
 * caller exported.
 *
 * React and its reconciler pick a build from `NODE_ENV` when first loaded,
 * and they are external to Bake's bundle, so the choice is made at run time.
 * The development build records a `performance.measure` entry for nearly
 * every component render, and Node keeps each entry until someone clears
 * them. A session that redraws for hours holds millions, and the heap runs
 * out. `BAKE_RENDERER=development` (or `DSH_RENDERER`) keeps the development
 * build on purpose, as the performance baseline does.
 *
 * Must run before anything imports `react` or `ink`.
 * @param env - the process environment to update.
 */
export function selectRendererBuild(env: NodeJS.ProcessEnv = process.env): void {
  if (env[INHERITED_NODE_ENV] === undefined) env[INHERITED_NODE_ENV] = env.NODE_ENV === undefined ? '-' : `=${env.NODE_ENV}`
  env.NODE_ENV = (env.BAKE_RENDERER ?? env.DSH_RENDERER) === 'development' ? 'development' : 'production'
}

/**
 * Run the public dsh command-line interface.
 * @returns a promise that settles when the selected command mode finishes,
 *   or, for a profile, once it has booted and its app owns the process.
 */
export async function runCli(): Promise<void> {
  const version = readVersion()
  const invocation = parseDshArgs(process.argv.slice(2), version)

  switch (invocation.mode) {
    case 'profile': {
      // After the `.env` layers, which may name NODE_ENV for the agent's
      // commands, and before the profile loads the renderer.
      const environment = loadLayeredEnv('dsh')
      selectRendererBuild()
      const { runProfile } = await import('./profile-boot.ts')
      void import('./update.ts').then(update => update.recordLaunch()).catch(() => {})
      try {
        await runProfile({
          environment,
          profile: invocation.profile,
          fromDefaultProfile: invocation.fromDefaultProfile,
          patchFiles: invocation.patches,
          args: invocation.args,
        })
      } catch (error) {
        if (!(error instanceof StartupError)) throw error
        await reportStartupFailure(error, { home: resolveDshHome(), version, profile: invocation.profile })
        process.exit(1)
      }
      break
    }
    case 'plugin': {
      const { runPlugin } = await import('./plugin.ts')
      process.exit(await runPlugin(invocation.profile, invocation.args))
      break
    }
    case 'update': {
      const { runRollback, runUpdate } = await import('./update.ts')
      const abort = new AbortController()
      const cancel = (): void => abort.abort()
      process.once('SIGINT', cancel)
      try {
        process.exitCode = invocation.rollback
          ? await runRollback({ signal: abort.signal })
          : await runUpdate(invocation.check, version, { signal: abort.signal })
      } finally {
        process.off('SIGINT', cancel)
      }
      break
    }
    case 'self-check': {
      // The renderer build a launch loads, chosen before anything imports React.
      selectRendererBuild()
      const { runSelfCheck } = await import('./self-check.ts')
      const code = await runSelfCheck(version)
      // Imported modules may hold handles open, and the check waits for
      // nothing more: exit once its report has reached the pipes.
      await Promise.all([process.stdout, process.stderr].map(stream => new Promise<void>((resolve) => {
        stream.write('', () => resolve())
      })))
      process.exit(code)
      break
    }
    case 'dump-config': {
      const { runDumpConfig } = await import('./dump-config.ts')
      runDumpConfig(
        invocation.profile,
        invocation.defaultOnly,
        invocation.patches,
        invocation.fromDefaultProfile,
      )
      break
    }
    case 'dump-config-schema': {
      const { runDumpConfigSchema } = await import('./dump-config-schema.ts')
      await runDumpConfigSchema(invocation.profile, invocation.patches, invocation.fromDefaultProfile)
      break
    }
    default:
      invocation satisfies never
      throw new Error(`dsh: unhandled invocation mode ${JSON.stringify(invocation)}`)
  }
}
