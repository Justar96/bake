/** Built JSX must execute with production React under Node. No Harness service runs on Bun. */
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { pathToFileURL } from 'node:url'
import { afterEach, expect, it } from 'bun:test'
import { bundle, BUILT_ENV, profileEnvironment, requireBuilt } from '../../../scripts/build.ts'

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
  const output = artifacts.find(artifact => artifact.path.endsWith('/view.js'))!
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

it('isolates Bake profiles from upstream and respects an explicit data directory', () => {
  const home = join(tmpdir(), 'bake-user')
  const custom = join(tmpdir(), 'bake-custom')
  expect(profileEnvironment(home, {})).toEqual({ NODE_ENV: 'production', DSH_HOME: join(home, '.bake') })
  const env = { DSH_HOME: custom, NODE_ENV: 'development' }
  expect(profileEnvironment(home, env)).toEqual({ NODE_ENV: 'production', DSH_HOME: custom })
  expect(env).toEqual({ DSH_HOME: custom, NODE_ENV: 'development' })
  expect(() => profileEnvironment(home, { DSH_HOME: '' })).toThrow('DSH_HOME must name a directory or be unset')
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
