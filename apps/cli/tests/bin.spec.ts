/** Real Node processes exercise the npm entry's restart, inheritance, and shutdown. */
import { mkdtemp, readdir, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'

const repoRoot = fileURLToPath(new URL('../../../', import.meta.url))
const fixture = fileURLToPath(new URL('./fixtures/diagnostic-launch.ts', import.meta.url))

async function launch(args: string[], options: { flags?: string[]; defaultHome?: boolean; entry?: string } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'dsh-bin home-'))
  const home = options.defaultHome ? join(root, '.bake') : join(root, 'chosen home')
  const flags = options.flags?.map(flag => flag.replace('<directory>', join(home, 'diagnostics'))) ?? []
  const child = execa(process.execPath, ['--import', 'tsx/esm', ...flags, options.entry ?? fixture, ...args], {
    cwd: repoRoot,
    env: { ...process.env, HOME: root, USERPROFILE: root, DSH_HOME: options.defaultHome ? undefined : home, NODE_OPTIONS: '--no-warnings' },
    reject: false,
    timeout: 30_000,
    killSignal: 'SIGKILL',
  })
  onTestFinished(async () => {
    child.kill('SIGKILL')
    await child
    await rm(root, { recursive: true, force: true })
  })
  return { child, home }
}

describe('npm bin diagnostics restart', () => {
  it('restarts with private diagnostics, retains arguments and user NODE_OPTIONS, and leaves tools unmodified', async () => {
    const { child, home } = await launch(['failure', 'one argument', '$(literal)'], { flags: ['--no-report-exclude-env'] })
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.exitCode).toBe(17)
    expect(result.stderr).toBe('')
    const observed = JSON.parse(result.stdout)
    expect(observed).toMatchObject({
      home, argv: ['failure', 'one argument', '$(literal)'], nodeOptions: '--no-warnings',
      excludeEnv: true, excludeNetwork: true, descendant: { nodeOptions: '--no-warnings' },
    })
    expect(observed[process.platform === 'win32' || process.execve === undefined ? 'parent' : 'pid']).toBe(child.pid)
    expect(observed.execArgv.slice(-3)).toEqual(['--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${join(home, 'diagnostics')}`])
    expect(observed.descendant.execArgv).not.toContain('--report-exclude-env')
    expect(observed.descendant.execArgv).not.toContain('--report-exclude-network')
    expect(observed.descendant.execArgv.some((arg: string) => arg.startsWith('--diagnostic-dir'))).toBe(false)
    if (process.platform !== 'win32') expect((await stat(join(home, 'diagnostics'))).mode & 0o777).toBe(0o700)
  })

  it.each(['--diagnostic-dir=<directory>', '--diagnostic-dir'])('runs directly when the release launcher supplied %s and the report flags', async (directoryFlag) => {
    const { child } = await launch([], { flags: [
      '--report-exclude-env', '--report-exclude-network', directoryFlag,
      ...(directoryFlag === '--diagnostic-dir' ? ['<directory>'] : []),
    ] })
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.exitCode).toBe(0)
    expect(JSON.parse(result.stdout).pid).toBe(child.pid)
  })

  it('uses Bake’s default home when DSH_HOME is unset', async () => {
    const { child, home } = await launch([], { defaultHome: true })
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.exitCode).toBe(0)
    expect(JSON.parse(result.stdout).home).toBe(home)
    await expect(readdir(join(home, 'diagnostics'))).resolves.toEqual([])
  })

  it('runs the actual source entry after the restart', async () => {
    const { child } = await launch(['--version'], { entry: 'apps/cli/src/bin.ts' })
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.exitCode).toBe(0)
    expect(result.stderr).toBe('')
    expect(result.stdout).toMatch(/^\d+\.\d+\.\d+$/u)
  })

  it.skipIf(process.platform === 'win32').each([
    ['SIGINT', false], ['SIGTERM', false], ['SIGHUP', false],
    ['SIGINT', true], ['SIGTERM', true], ['SIGHUP', true],
  ] as const)('preserves %s and exit status with spawn fallback %s', async (signal, fallback) => {
    const { child } = await launch(['signal', ...(fallback ? ['--spawn-fallback'] : [])])
    for await (const line of child.iterable()) {
      if (line === 'ready') child.kill(signal)
    }
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.stdout).toContain(signal)
    expect(result.exitCode).toBe(signal === 'SIGINT' ? 130 : signal === 'SIGHUP' ? 129 : 0)
  })

  it.skipIf(process.platform === 'win32').each([false, true])('preserves signal death with spawn fallback %s', async (fallback) => {
    const { child } = await launch(['signal-death', ...(fallback ? ['--spawn-fallback'] : [])])
    for await (const line of child.iterable()) {
      if (line === 'ready') child.kill('SIGTERM')
    }
    const result = await child
    expect(result.timedOut).toBe(false)
    expect(result.signal).toBe('SIGTERM')
  })
})
