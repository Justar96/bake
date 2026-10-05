/**
 * Every release launcher starts Node with the flags the runtime watchdog needs
 * to arm fatal-error reports and heap snapshots, under an existing
 * `<Bake home>/diagnostics`, whatever spaces the paths hold.
 */
import { afterEach, describe, expect, test } from 'bun:test'
import {
  chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { windowsLauncher } from '../../packages/boot/updater/src/install.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** What the stand-in CLI saw of its own start. */
interface Launch {
  execArgv: string[]
  script: string
  argv: string[]
  home: string | null
  nodeOptions: string | null
  excludeEnv: boolean
  excludeNetwork: boolean
}

/** A scratch directory whose path holds a space. */
function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake launcher '))
  roots.push(root)
  return root
}

/** A release tree at `root` whose CLI reports how Node started it, instead of starting Bake. */
function release(root: string): string {
  const cli = join(root, 'apps/cli/lib/bin.js')
  mkdirSync(join(cli, '..'), { recursive: true })
  writeFileSync(cli, `process.stdout.write(JSON.stringify({
  execArgv: process.execArgv, script: process.argv[1], argv: process.argv.slice(2),
  home: process.env.BAKE_HOME ?? null, legacyHome: process.env.DSH_HOME ?? null, nodeOptions: process.env.NODE_OPTIONS ?? null,
  excludeEnv: process.report.excludeEnv, excludeNetwork: process.report.excludeNetwork,
}))\n`)
  return cli
}

async function launch(argv: string[], env: NodeJS.ProcessEnv): Promise<Launch> {
  const child = Bun.spawn(argv, { env, stdout: 'pipe', stderr: 'pipe', timeout: 20_000 })
  const [code, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()])
  if (code !== 0) throw new Error(`${argv.join(' ')} exited ${code}: ${stderr}`)
  return JSON.parse(stdout) as Launch
}

const FLAGS = ['--report-exclude-env', '--report-exclude-network']

test('the release bake.cmd starts Node as the installed Windows launcher does', () => {
  const starts = (text: string): string[] => text.split(/\r?\n/).filter(line => line.startsWith('node ') || line.includes(' mkdir '))
  const shipped = starts(readFileSync(resolve(import.meta.dir, 'bake.cmd'), 'utf8'))
  expect(shipped).toEqual(starts(windowsLauncher('C:\\Program Files\\Bake')))
  expect(shipped).toEqual([
    'if not exist "%BAKE_HOME%\\diagnostics\\" mkdir "%BAKE_HOME%\\diagnostics" 2>nul',
    'node --report-exclude-env --report-exclude-network "--diagnostic-dir=%BAKE_HOME%\\diagnostics" "%BAKE_CLI%" --profile tui %*',
    'node --report-exclude-env --report-exclude-network "--diagnostic-dir=%BAKE_HOME%\\diagnostics" "%BAKE_CLI%" %*',
  ])
})

describe.skipIf(process.platform === 'win32')('the POSIX launcher', () => {
  /** The installer's layout: a command-directory link to the release's `bin/bake`. */
  function install(): { root: string; command: string; cli: string } {
    const root = scratch()
    const cli = release(join(root, 'release root'))
    cpSync(resolve(import.meta.dir, 'bake'), join(root, 'release root/bin/bake'))
    chmodSync(join(root, 'release root/bin/bake'), 0o755)
    mkdirSync(join(root, 'bin dir'))
    symlinkSync(join(root, 'release root/bin/bake'), join(root, 'bin dir/bake'))
    return { root, command: join(root, 'bin dir/bake'), cli: realpathSync(cli) }
  }

  test('defaults the Bake home and creates its private diagnostics directory', async () => {
    const { root, command, cli } = install()
    const home = join(root, 'user home')
    const env: NodeJS.ProcessEnv = { PATH: process.env.PATH, HOME: home }
    const started = await launch([command, '--help'], env)
    const diagnostics = join(home, '.bake/diagnostics')
    expect(started).toEqual({
      execArgv: [...FLAGS, `--diagnostic-dir=${diagnostics}`], script: cli, argv: ['--profile', 'tui', '--help'],
      home: join(home, '.bake'), legacyHome: join(home, '.bake'), nodeOptions: null, excludeEnv: true, excludeNetwork: true,
    })
    expect(statSync(diagnostics).mode & 0o777).toBe(0o700)
  })

  test('uses an explicit home and passes a subcommand through, leaving NODE_OPTIONS alone', async () => {
    const { root, command, cli } = install()
    const home = join(root, 'custom home')
    const env: NodeJS.ProcessEnv = { PATH: process.env.PATH, HOME: root, BAKE_HOME: home, DSH_HOME: join(root, 'ignored'), NODE_OPTIONS: '--no-warnings' }
    const started = await launch([command, 'update', '--check'], env)
    expect(started).toMatchObject({
      execArgv: [...FLAGS, `--diagnostic-dir=${home}/diagnostics`], script: cli, argv: ['update', '--check'], home,
      legacyHome: home, nodeOptions: '--no-warnings',
    })
    expect(existsSync(join(home, 'diagnostics'))).toBe(true)
  })

  test('falls back to DSH_HOME when BAKE_HOME is unset', async () => {
    const { root, command } = install()
    const home = join(root, 'earlier home')
    const started = await launch([command, '--help'], { PATH: process.env.PATH, HOME: root, DSH_HOME: home })
    expect(started).toMatchObject({ execArgv: [...FLAGS, `--diagnostic-dir=${home}/diagnostics`], home, legacyHome: home })
  })
})

describe.skipIf(process.platform !== 'win32')('the Windows launchers', () => {
  const expectWindowsLaunch = (started: Launch, home: string, argv: string[]): void => {
    expect(started).toMatchObject({
      execArgv: [...FLAGS, `--diagnostic-dir=${home}\\diagnostics`], argv, home, excludeEnv: true, excludeNetwork: true,
    })
    expect(existsSync(join(home, 'diagnostics'))).toBe(true)
  }

  test('the release bake.cmd', async () => {
    const root = scratch()
    release(join(root, 'release root'))
    cpSync(resolve(import.meta.dir, 'bake.cmd'), join(root, 'release root/bin/bake.cmd'))
    const home = join(root, 'user home', '.bake')
    const command = join(root, 'release root/bin/bake.cmd')
    const started = await launch(['cmd.exe', '/d', '/c', command, '--help'], { ...process.env, BAKE_HOME: home })
    expectWindowsLaunch(started, home, ['--profile', 'tui', '--help'])
  })

  test('the installed launcher the updater and installer write', async () => {
    const root = scratch()
    const install = join(root, 'install root')
    release(join(install, 'versions/0.1.0-aaaaaaaaaaaa'))
    writeFileSync(join(install, 'current.txt'), '0.1.0-aaaaaaaaaaaa\r\n')
    mkdirSync(join(install, 'bin'))
    writeFileSync(join(install, 'bin/bake.cmd'), windowsLauncher(install))
    const home = join(root, 'user home', '.bake')
    const command = join(install, 'bin/bake.cmd')
    const started = await launch(['cmd.exe', '/d', '/c', command, 'update', '--check'], { ...process.env, BAKE_HOME: home })
    expectWindowsLaunch(started, home, ['update', '--check'])
  })
})
