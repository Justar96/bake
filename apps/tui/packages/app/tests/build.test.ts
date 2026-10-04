/** Built JSX must execute with production React under Node. No Harness service runs on Bun. */
import { mkdtemp, mkdir, rm, stat, writeFile } from 'node:fs/promises'
import { basename, join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { pathToFileURL } from 'node:url'
import { afterEach, expect, it } from 'bun:test'
import { bundle, BUILT_ENV, diagnosticArguments, profileEnvironment, requireBuilt } from '../../../scripts/build.ts'

const roots: string[] = []
afterEach(async () => { for (const root of roots.splice(0)) await rm(root, { recursive: true, force: true }) })

it('executes bundled JSX with external production React on Node', async () => {
  const lib = resolve(import.meta.dirname, '../lib')
  await mkdir(lib, { recursive: true })
  const root = await mkdtemp(join(lib, 'build-test-'))
  roots.push(root)
  const entry = join(root, 'view.tsx')
  await writeFile(entry, 'export const view = <h1>Built</h1>; export const mode = process.env.NODE_ENV;\n')
  const artifacts = await bundle([entry], root)
  const output = artifacts.find(artifact => basename(artifact.path) === 'view.js')!
  const child = Bun.spawn([process.env.DSH_TUI_TEST_NODE ?? 'node', '--input-type=module', '-e',
    `const {view,mode}=await import(${JSON.stringify(pathToFileURL(output.path).href)}); console.log(view.type+":"+view.props.children+":"+mode);`],
  { env: { ...process.env, ...BUILT_ENV }, stdout: 'pipe', stderr: 'pipe', timeout: 10_000 })
  try {
    expect(await child.exited).toBe(0)
    expect(await new Response(child.stdout).text()).toBe('h1:Built:production\n')
    expect(await new Response(child.stderr).text()).toBe('')
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    await child.exited
  }
}, 30_000)

// The release installs production dependencies only, and on Windows into
// Bun's isolated layout, where a built entry resolves just the packages
// `@dsh-tui/app` declares. A hoisted checkout finds an undeclared one anyway.
it('declares every package the built entries import at runtime', async () => {
  const app = resolve(import.meta.dirname, '..')
  const lib = join(app, 'lib')
  await mkdir(lib, { recursive: true })
  const root = await mkdtemp(join(lib, 'build-test-'))
  roots.push(root)
  const entries = ['index.ts', 'startup.ts', 'runner-loader.ts', 'ui-loader.ts', 'syntax-loader.ts']
  const artifacts = await bundle(entries.map(entry => join(app, 'src', entry)), root)
  const manifest = await Bun.file(join(app, 'package.json')).json() as Record<'dependencies' | 'peerDependencies', Record<string, string>>
  const declared = new Set([...Object.keys(manifest.dependencies), ...Object.keys(manifest.peerDependencies)])
  const imported = new Set<string>()
  for (const artifact of artifacts) {
    const code = await artifact.text()
    // Static and literal dynamic imports of a bare package specifier; minified
    // copy strings that follow the word `import` never form a package name.
    for (const [, specifier] of code.matchAll(/\b(?:from|import)\s*\(?\s*["']((?:@[a-z0-9][\w.-]*\/)?[a-z0-9][\w.-]*(?::[\w./-]+|\/[\w./-]+)?)["']/gi)) {
      if (specifier!.startsWith('node:')) continue
      const parts = specifier!.split('/')
      imported.add(parts[0]!.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]!)
    }
  }
  expect(imported).toContain('ink')
  expect(imported).toContain('react')
  expect([...imported].filter(name => !declared.has(name)).sort()).toEqual([])
}, 30_000)

it('isolates Bake profiles from upstream and respects an explicit data directory', () => {
  const home = join(tmpdir(), 'bake-user')
  const custom = join(tmpdir(), 'bake-custom')
  expect(profileEnvironment(home, {})).toEqual({ NODE_ENV: 'production', DSH_HOME: join(home, '.bake') })
  const env = { DSH_HOME: custom, NODE_ENV: 'development' }
  expect(profileEnvironment(home, env)).toEqual({ NODE_ENV: 'production', DSH_HOME: custom })
  expect(env).toEqual({ DSH_HOME: custom, NODE_ENV: 'development' })
  expect(() => profileEnvironment(home, { DSH_HOME: '' })).toThrow('DSH_HOME must name a directory or be unset')
})

it('starts Node with the release launchers\' diagnostic flags under a created Bake-home directory', async () => {
  const root = await mkdtemp(join(tmpdir(), 'bake diagnostics '))
  roots.push(root)
  const home = join(root, 'user home', '.bake')
  const directory = join(home, 'diagnostics')
  expect(diagnosticArguments(home)).toEqual(['--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${directory}`])
  expect((await stat(directory)).isDirectory()).toBe(true)
  if (process.platform !== 'win32') expect((await stat(directory)).mode & 0o777).toBe(0o700)
  // The next launch finds the directory already there.
  expect(() => diagnosticArguments(home)).not.toThrow()
})

it('requires every built entry and gives a clean checkout its build command', async () => {
  const root = await mkdtemp(join(tmpdir(), 'bake-entry-test-'))
  roots.push(root)
  const present = join(root, 'index.js')
  const missing = join(root, 'startup.js')
  await writeFile(present, 'export {};\n')
  expect(() => requireBuilt([present])).not.toThrow()
  expect(() => requireBuilt([present, missing])).toThrow(`Missing built entry: ${missing}. Run bun run build from the repository root.`)
})

it('rejects a missing bundle entry', async () => {
  const lib = resolve(import.meta.dirname, '../lib')
  await mkdir(lib, { recursive: true })
  const root = await mkdtemp(join(lib, 'build-test-'))
  roots.push(root)
  await expect(bundle([join(root, 'missing.ts')], root)).rejects.toThrow()
})
