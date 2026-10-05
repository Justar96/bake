/** Node startup flags for the npm entry, before any application modules load. */
import { spawn } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { constants, homedir } from 'node:os'
import { join, resolve } from 'node:path'

const SIGNALS = ['SIGINT', 'SIGTERM', 'SIGHUP'] as const

/**
 * Restart once when the entry lacks the release launchers' diagnostic flags.
 * POSIX replaces this process to preserve its PID and foreground signal delivery.
 * Windows shares console events with the child and waits for its exit status.
 * Other platforms without replacement forward signals to the child.
 * Flags are command-line arguments so tools do not inherit them through
 * NODE_OPTIONS. A fully configured launch returns.
 * @returns once this process can load the application; a restarted parent exits.
 */
export async function restartWithDiagnostics(): Promise<void> {
  // BAKE_HOME selects the home; DSH_HOME, its earlier name, is read only when
  // BAKE_HOME is unset or blank, as resolveDshHome reads them. Both are set
  // to the result, so every reader agrees.
  const name = process.env.BAKE_HOME?.trim() ? 'BAKE_HOME' : 'DSH_HOME'
  const configured = process.env[name]
  if (configured !== undefined && configured.trim() === '') throw new Error(`${name} must name a directory or be unset`)
  let home = configured ?? join(homedir(), '.bake')
  if (home === '~') home = homedir()
  else if (home.startsWith('~/') || home.startsWith('~\\')) home = join(homedir(), home.slice(2))
  const resolvedHome = resolve(home)
  process.env.BAKE_HOME = resolvedHome
  process.env.DSH_HOME = resolvedHome
  const directory = join(resolvedHome, 'diagnostics')

  const options = new Map<string, string>()
  for (let index = 0; index < process.execArgv.length; index++) {
    const argument = process.execArgv[index]
    if (argument === undefined) break
    if (argument === '--diagnostic-dir') options.set(argument, process.execArgv[++index] ?? '')
    else if (argument.startsWith('--diagnostic-dir=')) options.set('--diagnostic-dir', argument.slice('--diagnostic-dir='.length))
    else if (argument === '--report-exclude-env' || argument === '--report-exclude-network') options.set(argument, 'true')
    else if (argument === '--no-report-exclude-env' || argument === '--no-report-exclude-network') options.set(argument.replace('--no-', '--'), 'false')
  }
  const startedDirectory = options.get('--diagnostic-dir')
  if (options.get('--report-exclude-env') === 'true' && options.get('--report-exclude-network') === 'true'
    && startedDirectory !== undefined && resolve(startedDirectory) === directory) return

  const script = process.argv[1]
  if (script === undefined) throw new Error('bake: cannot restart without an entry script')
  try { mkdirSync(directory, { recursive: true, mode: 0o700 }) } catch {
    // Diagnostics are optional; an unwritable directory must not prevent launch.
  }
  const args = [
    ...process.execArgv, '--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${directory}`,
    script, ...process.argv.slice(2),
  ]
  if (process.platform !== 'win32' && process.execve !== undefined) process.execve(process.execPath, [process.execPath, ...args], process.env)
  const child = spawn(process.execPath, args, { stdio: 'inherit' })
  const handlers = SIGNALS.map((signal) => {
    const handler = () => {
      // Windows delivers console events to both processes. child.kill() there
      // would terminate the child before its shutdown handlers could finish.
      if (process.platform !== 'win32') child.kill(signal)
    }
    process.on(signal, handler)
    return { signal, handler }
  })
  let result: { code: number | null; signal: NodeJS.Signals | null }
  try {
    result = await new Promise((resolveResult, reject) => {
      child.once('error', reject)
      child.once('exit', (code, signal) => resolveResult({ code, signal }))
    })
  } finally {
    for (const { signal, handler } of handlers) process.removeListener(signal, handler)
  }
  if (result.signal !== null) {
    process.exitCode = 128 + constants.signals[result.signal]
    process.kill(process.pid, result.signal)
    process.exit(process.exitCode)
  }
  process.exit(result.code ?? 1)
}
