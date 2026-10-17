/**
 * `bun run release:pack:rust` end to end on the real `bake-rs`: the debug
 * build the native preflight group has just made is packed into a path with
 * spaces and non-ASCII text, unpacked by the system `tar`, and run with a
 * PATH that holds neither Node nor Bun, in a home whose Bake settings and
 * credentials are deliberately broken and must stay untouched. This is a
 * package-layout check, not release qualification: no model, provider,
 * sandbox, install, or update runs. Run with `bun run test:rust:package`.
 */
import { afterEach, expect, test } from 'bun:test'
import { lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { archiveName, docsDigests, hostTarget, isRustTarget, parseSelfCheck } from './pack-rust.ts'

const ROOT = resolve(import.meta.dir, '../..')
const DOCS = join(ROOT, 'rust/crates/bake-cli/docs')
const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function scratch(label: string): string {
  const root = mkdtempSync(join(tmpdir(), `bake-rs ${label} ñ 測試-`))
  roots.push(root)
  return root
}

/** Every path under `root` with its bytes and modification time. */
function snapshot(root: string): Record<string, string> {
  const out: Record<string, string> = {}
  const walk = (dir: string): void => {
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name)
      const stats = lstatSync(path)
      out[path] = `${stats.mtimeMs} ${stats.isDirectory() ? 'dir' : readFileSync(path).toString('base64')}`
      if (stats.isDirectory()) walk(path)
    }
  }
  walk(root)
  return out
}

function cargoVersion(): string {
  const metadata = Bun.spawnSync(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], { cwd: join(ROOT, 'rust'), stdout: 'pipe', stderr: 'inherit' })
  expect(metadata.exitCode).toBe(0)
  const cli = (JSON.parse(metadata.stdout.toString()) as { packages: { name: string; version: string }[] }).packages.find(pkg => pkg.name === 'bake-cli')
  if (cli === undefined) throw new Error('cargo metadata lists no bake-cli')
  return cli.version
}

test.skipIf(!isRustTarget(hostTarget()))('the packed bake-rs unpacks and runs without Node, Bun, or a usable Bake home', async () => {
  const target = hostTarget()
  if (!isRustTarget(target)) return
  const version = cargoVersion()
  const out = join(scratch('out'), 'out dir')
  const pack = Bun.spawn(['bun', 'scripts/release/pack-rust.ts', '--profile', 'dev', '--out', out], { cwd: ROOT, stdout: 'pipe', stderr: 'pipe', timeout: 540_000 })
  const [stdout, stderr] = await Promise.all([new Response(pack.stdout).text(), new Response(pack.stderr).text()])
  const code = await pack.exited
  expect(pack.signalCode).toBeNull()
  if (code !== 0) throw new Error(`release:pack:rust exited ${code}:\n${stderr}`)
  const archive = join(out, archiveName(version, target))
  expect(stdout.trimEnd().split('\n').at(-1)).toBe(archive)
  expect(readdirSync(out)).toEqual([archiveName(version, target)])

  const unpacked = join(scratch('unpacked'), 'unpacked dir')
  mkdirSync(unpacked)
  const untar = Bun.spawnSync(['tar', '-xzf', archive, '-C', unpacked], { stderr: 'pipe' })
  expect(untar.exitCode).toBe(0)
  const exe = target === 'win32-x64' ? 'bake-rs.exe' : 'bake-rs'
  expect(readdirSync(unpacked).sort()).toEqual(['LICENSE', 'THIRD_PARTY_NOTICES.md', 'bin', 'notices'])
  expect(readdirSync(join(unpacked, 'bin'))).toEqual([exe])
  // Every NOTICE is a Bake crate's, and those that port Pi keep its notice.
  const notices = readdirSync(join(unpacked, 'notices')).sort()
  expect(notices).toContain('bake-coding-agent')
  expect(notices).not.toContain('bake-conformance')
  expect(readFileSync(join(unpacked, 'notices/bake-coding-agent/NOTICE'), 'utf8')).toContain('Mario Zechner')
  if (process.platform !== 'win32') expect(statSync(join(unpacked, 'bin', exe)).mode & 0o777).toBe(0o755)

  // An empty PATH: neither Node nor Bun can be found, so nothing below can lean on them.
  const path = scratch('path')
  expect(Bun.which('node', { PATH: path })).toBeNull()
  expect(Bun.which('bun', { PATH: path })).toBeNull()
  const home = scratch('home')
  for (const dir of ['.bake', '.dsh']) {
    mkdirSync(join(home, dir))
    writeFileSync(join(home, dir, 'settings.json'), '{ broken')
    writeFileSync(join(home, dir, 'auth.json'), '["broken"')
    writeFileSync(join(home, dir, 'settings.yaml'), 'providers: [broken')
    writeFileSync(join(home, dir, '.credentials.yaml'), 'key: *broken')
  }
  const before = snapshot(home)
  const env: Record<string, string> = { PATH: path, HOME: home, USERPROFILE: home, BAKE_HOME: join(home, '.bake'), DSH_HOME: join(home, '.dsh') }
  if (process.platform === 'win32') for (const name of ['SystemRoot', 'windir']) if (process.env[name] !== undefined) env[name] = process.env[name] as string
  const run = (...args: string[]): { code: number; stdout: string; stderr: string } => {
    const child = Bun.spawnSync([join(unpacked, 'bin', exe), ...args], { cwd: unpacked, env, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', timeout: 30_000 })
    expect(child.signalCode ?? null).toBeNull()
    return { code: child.exitCode, stdout: child.stdout.toString(), stderr: child.stderr.toString() }
  }

  expect(run('--version')).toEqual({ code: 0, stdout: `bake-rs ${version}\n`, stderr: '' })
  const check = run('--self-check')
  expect(check.code).toBe(0)
  expect(check.stderr).toBe('')
  expect(parseSelfCheck(check.stdout, { version, target, docs: docsDigests(DOCS) }).runtime).toBe('rust')
  const topics = run('docs')
  expect(topics.code).toBe(0)
  for (const name of Object.keys(docsDigests(DOCS))) {
    expect(topics.stdout).toContain(`  ${name}`)
    expect(run('docs', name)).toEqual({ code: 0, stdout: readFileSync(join(DOCS, `${name}.md`), 'utf8'), stderr: '' })
  }
  const refused = run('--self-check', 'extra')
  expect(refused.code).toBe(2)
  expect(refused.stdout).toBe('')
  expect(snapshot(home)).toEqual(before)
  expect(readdirSync(path)).toEqual([])
})

test('a TypeScript release output directory is refused before building', () => {
  const child = Bun.spawnSync(['bun', 'scripts/release/pack-rust.ts', '--out', join(ROOT, 'distribution/host/public')], { cwd: ROOT, stdout: 'pipe', stderr: 'pipe', timeout: 60_000 })
  expect(child.exitCode).toBe(1)
  expect(child.stderr.toString()).toContain("inside the TypeScript release's distribution/host/public/")
  // Cargo reports every build it runs, even one with nothing to do.
  expect(child.stderr.toString()).not.toContain('Finished')
})
