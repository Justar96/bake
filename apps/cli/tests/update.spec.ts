/** `bake update` reports through its exit status and changes only an install the updater manages. */
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { detectInstall, hostTarget } from '@deepseek-ai/dsh-updater'
import { runUpdate, UPDATE_AVAILABLE_EXIT } from '../src/update.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

const target = hostTarget()!

/** A release host serving `version`, signed by a throwaway key, and a managed 0.1.0 install. */
function fixture(version: string) {
  const root = mkdtempSync(join(tmpdir(), 'bake-cli-update-'))
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
  const env = { BAKE_RELEASE_BASE_URL: 'https://releases.test', BAKE_RELEASE_PUBLIC_KEY: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64') }
  const lines: string[] = []
  const run = (check: boolean, layout = detectInstall(running)) =>
    runUpdate(check, '0.1.0', { env, fetch, layout, out: line => lines.push(line), err: line => lines.push(line) })
  return { install, running, sha256, run, lines }
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
