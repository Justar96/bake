/**
 * `dsh --self-check`, as `bake update` runs it on an unpacked release: it
 * passes for a complete layout, names the package a layout lacks, stops at
 * its time limit, and leaves no private home or temporary files behind.
 */
import { existsSync, realpathSync } from 'node:fs'
import { cp, lstat, mkdir, mkdtemp, readdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { launchProblem } from 'bake-updater'

const repoRoot = fileURLToPath(new URL('../../../', import.meta.url))
const built = existsSync(join(repoRoot, 'apps/cli/lib/bin.js')) && existsSync(join(repoRoot, 'apps/tui/packages/app/lib/runner-loader.js'))
/** Newer than any release without `--self-check`, so the updater runs the check rather than `--version`. */
const VERSION = '99.0.0'

let root: string
beforeEach(async () => {
  root = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-self-check-')))
  // The updater's scratch directory, and through it the check's private home, goes here.
  await mkdir(join(root, 'tmp'))
  vi.stubEnv('TMPDIR', join(root, 'tmp'))
  vi.stubEnv('TMP', join(root, 'tmp'))
  vi.stubEnv('TEMP', join(root, 'tmp'))
})
afterEach(async () => {
  vi.unstubAllEnvs()
  await rm(root, { recursive: true, force: true, maxRetries: 3 })
})

/**
 * An unpacked release laid out as the hoisted installs are: the CLI's built
 * files and the terminal package are copies, and every other package links to
 * the workspace's own. A package the copies import resolves through the
 * release's `node_modules`, so leaving one out is what a platform layout
 * missing that package looks like to them.
 * @param missing - the package the layout lacks.
 * @returns the release directory.
 */
async function release(missing?: string): Promise<string> {
  const directory = join(root, 'release')
  await mkdir(join(directory, 'apps/cli'), { recursive: true })
  await cp(join(repoRoot, 'apps/cli/lib'), join(directory, 'apps/cli/lib'), { recursive: true })
  const manifest = JSON.parse(await readFile(join(repoRoot, 'apps/cli/package.json'), 'utf8')) as { version: string }
  await writeFile(join(directory, 'apps/cli/package.json'), JSON.stringify({ ...manifest, version: VERSION }))
  const modules = join(directory, 'node_modules')
  const link = async (name: string): Promise<void> => {
    if (name === missing) return
    const target = join(modules, name)
    if (name === '@dsh-tui/app') {
      const app = join(repoRoot, 'apps/tui/packages/app')
      await mkdir(target, { recursive: true })
      for (const entry of ['package.json', 'cordis.built.patch.yml', 'lib']) await cp(join(app, entry), join(target, entry), { recursive: true })
      return
    }
    await symlink(realpathSync(join(repoRoot, 'node_modules', name)), target)
  }
  await mkdir(modules)
  for (const name of await readdir(join(repoRoot, 'node_modules'))) {
    if (name.startsWith('.')) continue
    if (!name.startsWith('@')) { await link(name); continue }
    await mkdir(join(modules, name))
    for (const scoped of await readdir(join(repoRoot, 'node_modules', name))) await link(`${name}/${scoped}`)
  }
  return directory
}

/** Run the updater's check on `directory`. */
const check = (directory: string, timeoutMs?: number) => launchProblem({
  node: process.execPath, release: directory, version: VERSION, ...timeoutMs === undefined ? {} : { timeoutMs },
})

/** Whatever the check left in the temporary directory it was given. */
const leftovers = async (): Promise<string[]> => await readdir(join(root, 'tmp'))

describe.skipIf(!built || process.platform === 'win32')('dsh --self-check', () => {
  it('passes for a complete release and leaves nothing behind', async () => {
    const directory = await release()
    await expect(check(directory)).resolves.toBeUndefined()
    expect(await leftovers()).toEqual([])
    // The release itself is unchanged: no launch marker, no profile.
    expect((await readdir(directory)).sort()).toEqual(['apps', 'node_modules'])
  })

  it('names the package missing from the layout, as Bake 0.3.5 on Windows lacked one', async () => {
    const directory = await release('bake-compaction-basic')
    expect(await check(directory))
      .toMatch(/^dsh: self-check: tui profile: bake-compaction-basic: .*'bake-compaction-basic'/u)
    expect(await leftovers()).toEqual([])
  })

  it('names a package only the terminal runner imports, which a launch loads after its profile', async () => {
    const directory = await release('ink')
    expect(await check(directory)).toMatch(/^dsh: self-check: terminal runner runner-loader: Cannot find package 'ink'/u)
    expect(await leftovers()).toEqual([])
  })

  it('names a missing package only a shipped agent preset mounts', async () => {
    const directory = await release('bake-tool-cordis')
    expect(await check(directory))
      .toMatch(/^dsh: self-check: agent presets: bake-tool-cordis: .*'bake-tool-cordis'/u)
    expect(await leftovers()).toEqual([])
  })

  it('stops at its time limit and removes the private home it made', async () => {
    const directory = await release()
    const started = Date.now()
    await expect(check(directory, 200)).resolves.toBe('no result within 200 ms')
    expect(Date.now() - started).toBeLessThan(10_000)
    expect(await leftovers()).toEqual([])
    expect((await lstat(directory)).isDirectory()).toBe(true)
  })
})
