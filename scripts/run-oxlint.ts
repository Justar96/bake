import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const oxlintCli = fileURLToPath(new URL('../node_modules/oxlint/bin/oxlint', import.meta.url))
const MAX_CAPTURED_OUTPUT_BYTES = 64 * 1024 * 1024
const FIX_FLAGS = new Set(['--fix', '--fix-dangerously', '--fix-suggestions'])

function isFixInvocation(args: readonly string[]): boolean {
  return args.some(arg => FIX_FLAGS.has(arg))
}

function hasOutputFormat(args: readonly string[]): boolean {
  return args.some(arg =>
    arg === '-f'
    || arg.startsWith('-f=')
    || arg === '--format'
    || arg.startsWith('--format='))
}

/** Complete Oxlint child-process arguments and environment. */
export interface OxlintInvocation {
  readonly args: readonly string[]
  readonly env: NodeJS.ProcessEnv
}

/**
 * Apply the repository worker bound to both Oxlint backends.
 * @param args - Oxlint CLI arguments requested by the caller.
 * @param env - Environment inherited by the Oxlint process.
 * @returns the complete CLI arguments and child environment.
 */
export function resolveOxlintInvocation(args: readonly string[], env: NodeJS.ProcessEnv): OxlintInvocation {
  const resolvedArgs = [...args]
  if (env.CI === 'true' && !hasOutputFormat(args)) resolvedArgs.push('--format=default')
  const raw = env.DSH_OXLINT_THREADS
  if (raw === undefined || raw === '') return { args: resolvedArgs, env: { ...env } }
  const parsed = Number.parseInt(raw, 10)
  if (!Number.isSafeInteger(parsed) || parsed < 1 || String(parsed) !== raw) {
    throw new Error(`run-oxlint: DSH_OXLINT_THREADS must be a positive integer, got ${JSON.stringify(raw)}.`)
  }
  if (args.some(arg => arg === '--threads' || arg.startsWith('--threads='))) {
    throw new Error('run-oxlint: use DSH_OXLINT_THREADS instead of passing --threads directly.')
  }
  return {
    args: [...resolvedArgs, `--threads=${raw}`],
    env: { ...env, GOMAXPROCS: raw },
  }
}

/** Run Oxlint with the invocation, its output either shown or captured. */
function oxlint(invocation: OxlintInvocation, output: 'inherit' | 'pipe'): Bun.SyncSubprocess {
  const result = Bun.spawnSync([process.execPath, oxlintCli, ...invocation.args], {
    env: invocation.env,
    stdio: output === 'inherit' ? ['inherit', 'inherit', 'inherit'] : ['ignore', 'pipe', 'pipe'],
    ...output === 'pipe' ? { maxBuffer: MAX_CAPTURED_OUTPUT_BYTES } : {},
  })
  if (result.exitedDueToMaxBuffer === true) {
    throw new Error(`run-oxlint: Oxlint output exceeded ${String(MAX_CAPTURED_OUTPUT_BYTES)} bytes.`)
  }
  return result
}

function completeFrom(result: Bun.SyncSubprocess): void {
  if (result.signalCode !== undefined) {
    process.kill(process.pid, result.signalCode)
    return
  }
  process.exitCode = result.exitCode
}

function main(): void {
  const invocation = resolveOxlintInvocation(process.argv.slice(2), process.env)
  if (!isFixInvocation(invocation.args)) {
    completeFrom(oxlint(invocation, 'inherit'))
    return
  }

  const first = oxlint(invocation, 'pipe')
  if (first.signalCode !== undefined) {
    completeFrom(first)
    return
  }
  if (first.exitCode === 0) {
    process.stdout.write(first.stdout ?? '')
    process.stderr.write(first.stderr ?? '')
    process.exitCode = 0
    return
  }

  // Overlapping JS-plugin fixes can expose one more fixable diagnostic after the first pass.
  completeFrom(oxlint(invocation, 'inherit'))
}

const entrypoint = process.argv[1]
if (entrypoint !== undefined && resolve(entrypoint) === fileURLToPath(import.meta.url)) main()
