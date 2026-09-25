#!/usr/bin/env bun
/** Build one host-native, offline-installable Bake archive from built workspace output. */

import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { windowsLauncher } from '../../packages/boot/updater/src/install.ts'

/** Placeholder `install.ps1` replaces with the install root in the launcher template. */
const LAUNCHER_ROOT = '@@BAKE_RELEASE_ROOT@@'

const ROOT = resolve(import.meta.dir, '../..')
const OUTPUT = join(ROOT, '.artifacts/bake-release')
const cli = JSON.parse(readFileSync(join(ROOT, 'apps/cli/package.json'), 'utf8')) as { version: string }
const root = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')) as { version: string; workspaces: string[]; scripts: Record<string, string> }
if (cli.version !== root.version) throw new Error(`Bake and CLI versions differ: ${root.version} != ${cli.version}`)
const target = `${process.platform}-${process.arch}`
const supported = new Set(['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64'])
if (!supported.has(target)) throw new Error(`Unsupported release target: ${target}`)
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(cli.version)) throw new Error('CLI version is not a release version')

async function run(argv: string[], cwd: string): Promise<void> {
  const child = Bun.spawn(argv, { cwd, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' })
  const code = await child.exited
  if (code !== 0) throw new Error(`${argv[0]} exited ${code}`)
}

/** Copy package files and the first directory of each published file pattern. */
function copyWorkspace(relative: string, stage: string): void {
  const source = join(ROOT, relative)
  const destination = join(stage, relative)
  const manifest = JSON.parse(readFileSync(join(source, 'package.json'), 'utf8')) as { files?: string[] }
  mkdirSync(destination, { recursive: true })
  cpSync(join(source, 'package.json'), join(destination, 'package.json'))
  const entries = new Set(['LICENSE', 'LICENSE.md', 'README.md', 'README.zh.md'])
  for (const pattern of manifest.files ?? ['lib', 'src']) {
    if (pattern.startsWith('!')) continue
    const [entry] = pattern.split('/')
    if (entry) entries.add(entry)
  }
  // The TUI app is private and has no files list; its profile patch is a runtime input.
  if (relative === 'apps/tui/packages/app') entries.add('cordis.built.patch.yml')
  for (const entry of entries) {
    const from = join(source, entry)
    if (!existsSync(from)) continue
    cpSync(from, join(destination, entry), { recursive: true, filter: path => !basename(path).startsWith('.env') })
  }
}

// GitHub's Windows TEMP is an 8.3 short path (RUNNER~1). Bun then matches no
// staged workspace to the lockfile and re-resolves them, so use the long form.
const stage = realpathSync.native(mkdtempSync(join(tmpdir(), 'bake-pack-')))
try {
  for (const pattern of root.workspaces) {
    for (const file of new Bun.Glob(`${pattern}/package.json`).scanSync({ cwd: ROOT, onlyFiles: true })) {
      copyWorkspace(dirname(file).replaceAll('\\', '/'), stage)
    }
  }
  const rootManifest = structuredClone(root)
  // A release has no development hook installation; native dependency scripts still run.
  delete rootManifest.scripts.postinstall
  writeFileSync(join(stage, 'package.json'), `${JSON.stringify(rootManifest, null, 2)}\n`)
  cpSync(join(ROOT, 'bun.lock'), join(stage, 'bun.lock'))
  cpSync(join(ROOT, 'patches'), join(stage, 'patches'), { recursive: true })
  cpSync(join(ROOT, 'LICENSE'), join(stage, 'LICENSE'))
  cpSync(join(ROOT, 'THIRD_PARTY_NOTICES.md'), join(stage, 'THIRD_PARTY_NOTICES.md'))
  cpSync(join(ROOT, 'CHANGELOG.md'), join(stage, 'CHANGELOG.md'))
  mkdirSync(join(stage, 'bin'), { recursive: true })
  cpSync(join(import.meta.dir, 'bake'), join(stage, 'bin/bake'))
  cpSync(join(import.meta.dir, 'bake.cmd'), join(stage, 'bin/bake.cmd'))
  if (process.platform === 'win32') {
    const command = readFileSync(join(stage, 'bin/bake.cmd'), 'utf8')
    writeFileSync(join(stage, 'bin/bake.cmd'), command.replace(/\r?\n/g, '\r\n'))
  }
  // The installed Windows command, with the install root left for install.ps1
  // to fill in: the updater and the installer then write the same launcher.
  writeFileSync(join(stage, 'bin/bake-launcher.cmd.template'), windowsLauncher(LAUNCHER_ROOT))
  chmodSync(join(stage, 'bin/bake'), 0o755)
  const install = ['bun', 'install', '--production', '--filter', '@deepseek-ai/dsh']
  try {
    await run([...install, '--frozen-lockfile'], stage)
  } catch (error) {
    // Name what bun would change, so a host-specific lockfile drift is diagnosable from the log.
    const frozen = readFileSync(join(stage, 'bun.lock'), 'utf8').split('\n')
    // `--production` always freezes the lockfile, so the rerun resolves without it.
    await run(['bun', 'install', '--lockfile-only'], stage).catch(() => {})
    const kept = new Set(frozen)
    const changed = readFileSync(join(stage, 'bun.lock'), 'utf8').split('\n')
    const added = changed.filter(line => !kept.has(line))
    const removed = frozen.filter(line => !new Set(changed).has(line))
    console.error(['The staged lockfile differs on this host:', ...removed.map(line => `- ${line}`), ...added.map(line => `+ ${line}`)].slice(0, 80).join('\n'))
    throw error
  }

  const home = mkdtempSync(join(tmpdir(), 'bake-pack-home-'))
  try {
    const check = Bun.spawn(['node', 'apps/cli/lib/bin.js', '--profile', 'tui', '--help'], {
      cwd: stage, env: { ...process.env, DSH_HOME: home }, stdout: 'pipe', stderr: 'inherit',
    })
    const output = await new Response(check.stdout).text()
    if (await check.exited !== 0 || !output.includes('Usage:')) throw new Error('Staged CLI did not boot')
  } finally {
    rmSync(home, { recursive: true, force: true })
  }

  const directory = join(OUTPUT, cli.version)
  mkdirSync(directory, { recursive: true })
  const name = `bake-v${cli.version}-${target}.tar.gz`
  const archive = join(directory, name)
  await run(['tar', '-czf', archive, '-C', stage, '.'], ROOT)
  console.log(archive)
} finally {
  rmSync(stage, { recursive: true, force: true })
}
