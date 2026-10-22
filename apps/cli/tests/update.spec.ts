/** `bake update` reports through its exit status and changes only an install the updater manages. */
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, readlinkSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { detectInstall, hostTarget } from 'bake-updater'
import { runRollback, runUpdate, UPDATE_AVAILABLE_EXIT } from '../src/update.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

const target = hostTarget()!

/** A release host serving `version`, signed by a throwaway key, and a managed 0.1.0 install. */
function fixture(version: string) {
  // Resolved, as the installer resolves its root: macOS's temporary directory
  // sits behind the /var -> /private/var symlink.
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'bake-cli-update-')))
  roots.push(root)
  const pair = generateKeyPairSync('ed25519')
  const tree = join(root, 'tree')
  mkdirSync(join(tree, 'apps/cli/lib'), { recursive: true })
  writeFileSync(join(tree, 'apps/cli/lib/bin.js'), `console.log(${JSON.stringify(version)})\n`)
  execFileSync('tar', ['-czf', join(root, 'release.tar.gz'), '-C', tree, '.'])
  const archive = readFileSync(join(root, 'release.tar.gz'))
  const sha256 = createHash('sha256').update(archive).digest('hex')
  const file = `bake-v${version}-${target}.tar.gz`
  const manifest = Buffer.from(JSON.stringify({ version, artifacts: { [target]: { file, sha256, size: archive.byteLength } } }))
  const files = new Map<string, Uint8Array>([
    ['latest.json', manifest], ['latest.json.sig', Buffer.from(sign(null, manifest, pair.privateKey).toString('base64'))],
    [`releases/${version}/${file}`, archive],
  ])
  const fetch = (async (url: string) => {
    const body = files.get(String(url).replace('https://releases.test/', ''))
    return body === undefined ? new Response('', { status: 404 }) : new Response(body)
  }) as unknown as typeof globalThis.fetch
  const install = join(root, 'install')
  const running = join(install, 'versions', '0.1.0-aaaaaaaaaaaa')
  mkdirSync(running, { recursive: true })
  symlinkSync(running, join(install, 'current'))
  // The update records its answer in the Bake home, which the test owns.
  const env = { DSH_HOME: join(root, 'home'), BAKE_RELEASE_BASE_URL: 'https://releases.test', BAKE_RELEASE_PUBLIC_KEY: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64') }
  const lines: string[] = []
  const run = (check: boolean, layout = detectInstall(running)) =>
    runUpdate(check, '0.1.0', { env, fetch, layout, out: line => lines.push(line), err: line => lines.push(line) })
  return { root, install, running, sha256, run, lines, env, fetch }
}

describe.skipIf(process.platform === 'win32')('runUpdate', () => {
  it('reports an available release with its own exit status and changes nothing', async () => {
    const { run, lines, install, running } = fixture('0.2.0')
    await expect(run(true)).resolves.toBe(UPDATE_AVAILABLE_EXIT)
    expect(lines).toEqual(['Bake 0.2.0 is available (running 0.1.0). Run: bake update'])
    expect(readlinkSync(join(install, 'current'))).toBe(running)
  })

  it('installs it and says which sessions it affects', async () => {
    const { run, lines, install, sha256 } = fixture('0.2.0')
    await expect(run(false)).resolves.toBe(0)
    expect(readlinkSync(join(install, 'current'))).toBe(join(install, 'versions', `0.2.0-${sha256.slice(0, 12)}`))
    expect(lines.at(-1)).toBe('Updated Bake 0.1.0 → 0.2.0. New sessions start 0.2.0; sessions already open keep 0.1.0.')
  })

  it('records what it found, so an open terminal names the same release', async () => {
    const { run, root } = fixture('0.2.0')
    await run(true)
    expect(JSON.parse(readFileSync(join(root, 'home/update-check.json'), 'utf8'))).toMatchObject({ version: '0.2.0' })
  })

  it('draws each install step and a summary on a terminal, instead of the plain report', async () => {
    const { running, lines, env, fetch } = fixture('0.2.0')
    const writes: string[] = []
    const code = await runUpdate(false, '0.1.0', {
      env: { ...env, LANG: 'en_US.UTF-8', TERM: 'xterm-256color' }, fetch, layout: detectInstall(running),
      out: line => lines.push(line), err: line => lines.push(line),
      terminal: { isTTY: true, columns: 100, write: (text) => { writes.push(text) } },
    })
    expect(code).toBe(0)
    const shown = writes.join('').replace(/\u001b\[[\d;]*m/gu, '')
    expect(shown).toContain('BAKE  update \u00b7 v0.1.0 \u2192 v0.2.0')
    expect(shown).toMatch(/\u2713 {2}Downloaded {10}0\.0 MB \u00b7 <?\d/u)
    expect(shown).toMatch(/\u2713 {2}Unpacked/u)
    expect(shown).toMatch(/\u2713 {2}Verified {12}v0\.2\.0 starts/u)
    expect(shown).toMatch(/Updated Bake 0\.1\.0 \u2192 0\.2\.0 in <?\d/u)
    expect(shown).toContain('New sessions start 0.2.0; sessions already open keep 0.1.0.')
    expect(lines).toEqual([])
  })

  it('says so when up to date', async () => {
    const { run, lines } = fixture('0.1.0')
    await expect(run(false)).resolves.toBe(0)
    expect(lines).toEqual(['Bake 0.1.0 is up to date.'])
  })

  it('refuses to install over a source checkout, but still checks from one', async () => {
    const { run, lines } = fixture('0.2.0')
    const checkout = { kind: 'unmanaged', running: '/src/bake' } as const
    await expect(run(false, checkout)).resolves.toBe(1)
    expect(lines[0]).toBe('This Bake runs from /src/bake, which the updater does not manage.')
    expect(lines[1]).toContain('git pull')
    await expect(run(true, checkout)).resolves.toBe(UPDATE_AVAILABLE_EXIT)
  })
})

/** A managed install holding `releases`, each a version directory and its command, with `current` naming `current`. */
function installed(releases: Record<string, string>, current: string) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'bake-cli-rollback-')))
  roots.push(root)
  const install = join(root, 'install')
  for (const [name, command] of Object.entries(releases)) {
    mkdirSync(join(install, 'versions', name, 'apps/cli/lib'), { recursive: true })
    writeFileSync(join(install, 'versions', name, 'apps/cli/lib/bin.js'), command)
  }
  symlinkSync(join(install, 'versions', current), join(install, 'current'))
  const env = { DSH_HOME: join(root, 'home') }
  const lines: string[] = []
  const layout = detectInstall(join(install, 'versions', current))
  const run = () => runRollback({ env, layout, out: line => lines.push(line), err: line => lines.push(line) })
  return { install, env, layout, lines, run }
}

const OLD = '0.1.0-aaaaaaaaaaaa'
const NEW = '0.2.0-bbbbbbbbbbbb'
const prints = (version: string): string => `console.log(${JSON.stringify(version)})\n`

describe.skipIf(process.platform === 'win32')('runRollback', () => {
  it('returns to the earlier release and says which sessions it affects', async () => {
    const { run, lines, install } = installed({ [OLD]: prints('0.1.0'), [NEW]: prints('0.2.0') }, NEW)
    await expect(run()).resolves.toBe(0)
    expect(readlinkSync(join(install, 'current'))).toBe(join(install, 'versions', OLD))
    expect(lines).toEqual([
      `Checking Bake 0.1.0 in ${OLD}…`,
      `Rolled back Bake 0.2.0 → 0.1.0 (${NEW} → ${OLD}). New sessions start 0.1.0; sessions already open keep 0.2.0.`,
      'bake update installs the newest release again.',
    ])
  })

  it('draws the check and a summary on a terminal, instead of the plain report', async () => {
    const { env, layout, lines } = installed({ [OLD]: prints('0.1.0'), [NEW]: prints('0.2.0') }, NEW)
    const writes: string[] = []
    const code = await runRollback({
      env: { ...env, LANG: 'en_US.UTF-8', TERM: 'xterm-256color' }, layout,
      out: line => lines.push(line), err: line => lines.push(line),
      terminal: { isTTY: true, columns: 100, write: (text) => { writes.push(text) } },
    })
    expect(code).toBe(0)
    const shown = writes.join('').replace(/\u001b\[[\d;]*m/gu, '')
    expect(shown).toContain('BAKE  rollback \u00b7 v0.2.0 \u2192 v0.1.0')
    expect(shown).toMatch(/\u2713 {2}Verified {12}v0\.1\.0 starts/u)
    expect(shown).toMatch(/Rolled back Bake 0\.2\.0 \u2192 0\.1\.0 in <?\d/u)
    expect(shown).toContain('bake update installs the newest release again.')
    expect(lines).toEqual([])
  })

  it('says when there is nothing to roll back to, and fails', async () => {
    const { run, lines, install } = installed({ [OLD]: prints('0.1.0') }, OLD)
    await expect(run()).resolves.toBe(1)
    expect(lines).toEqual(['Bake 0.1.0 is the oldest release installed; there is nothing to roll back to.'])
    expect(readlinkSync(join(install, 'current'))).toBe(join(install, 'versions', OLD))
  })

  it('keeps the current release when the earlier one fails its check, and says why', async () => {
    const { run, lines, install } = installed({ [OLD]: 'console.error(\'Error: boom\'); process.exitCode = 1\n', [NEW]: prints('0.2.0') }, NEW)
    await expect(run()).resolves.toBe(1)
    expect(lines.at(-1)).toBe('The earlier Bake 0.1.0 did not start; the current install is unchanged: Error: boom')
    expect(readlinkSync(join(install, 'current'))).toBe(join(install, 'versions', NEW))
  })

  it('refuses a release the updater does not manage', async () => {
    const lines: string[] = []
    await expect(runRollback({ layout: { kind: 'unmanaged', running: '/src/bake' }, out: line => lines.push(line), err: line => lines.push(line) }))
      .resolves.toBe(1)
    expect(lines[0]).toBe('This Bake runs from /src/bake, which the updater does not manage.')
  })
})
