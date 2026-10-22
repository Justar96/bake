/** The native release is assembled, signed, verified, and served on its own route, and never on the 0.3 one. */
import { afterEach, describe, expect, setDefaultTimeout, test } from 'bun:test'
import { createHash, createPublicKey, generateKeyPairSync, verify as verifySignature, type KeyObject } from 'node:crypto'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import {
  assembleNativeRelease, EPHEMERAL_PUBLIC_KEY, nativeSigningKey, type AssembleNativeOptions,
} from './assemble-rust.ts'
import { NATIVE_KEY_FILE, NATIVE_KEY_PLACEHOLDER } from './native-channel.ts'
import { nativeManifestBytes, parseNativeManifest, type NativeManifest } from './native-manifest.ts'
import { archiveName, deferredTargetMessage, type RustTarget } from './pack-rust.ts'

// A test starts a Node server and a verifier; a cold runner took over Bun's default 5 s.
setDefaultTimeout(30_000)

const ROOT = resolve(import.meta.dir, '../..')
const roots: string[] = []
const servers: ReturnType<typeof Bun.spawn>[] = []
afterEach(async () => {
  const stopping = servers.splice(0)
  for (const server of stopping) server.kill()
  await Promise.all(stopping.map(server => server.exited))
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-assemble-rust-'))
  roots.push(root)
  return root
}

const PAYLOAD = {
  LICENSE: 'MIT\n',
  'THIRD_PARTY_NOTICES.md': 'notices\n',
  'notices/bake-ai/NOTICE': 'Pi, MIT\n',
  'licenses/inventory.json': '{"schema":"bake-native-licenses/1"}\n',
}

/** A minimal archive with the native layout, under `dir` and named `name`. */
function nativeArchive(dir: string, name: string, files: Record<string, string> = { ...PAYLOAD, 'bin/bake': 'native binary\n' }): void {
  const stage = mkdtempSync(join(tmpdir(), 'bake-native-stage-'))
  roots.push(stage)
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(join(stage, path, '..'), { recursive: true })
    writeFileSync(join(stage, path), contents)
  }
  const metadata = process.platform === 'darwin' ? ['--no-mac-metadata', '--no-xattrs', '--no-acls', '--no-fflags'] : []
  mkdirSync(dir, { recursive: true })
  const top = [...new Set(Object.keys(files).map(file => file.split('/')[0] as string))]
  expect(Bun.spawnSync(['tar', ...metadata, '-czf', join(dir, name), '-C', stage, ...top]).exitCode).toBe(0)
}

const key = generateKeyPairSync('ed25519')
const testKey = key.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')

/** A source directory of native archives and a service payload holding a frozen 0.3 release. */
function fixture(version = '0.4.0', targets: RustTarget[] = ['linux-x64']): { root: string; sourceDir: string; publicRoot: string } {
  const root = scratch()
  const sourceDir = join(root, '.artifacts/bake-native-release', version)
  for (const target of targets) nativeArchive(sourceDir, archiveName(version, target))
  const publicRoot = join(root, 'distribution/host/public')
  mkdirSync(join(publicRoot, 'releases/0.3.8'), { recursive: true })
  writeFileSync(join(publicRoot, 'latest.json'), '{"version":"0.3.8","artifacts":{}}\n')
  writeFileSync(join(publicRoot, 'latest.json.sig'), 'frozen\n')
  writeFileSync(join(publicRoot, 'releases/0.3.8/bake-v0.3.8-linux-x64.tar.gz'), 'TypeScript archive\n')
  return { root, sourceDir, publicRoot }
}

function options(overrides: Partial<AssembleNativeOptions> & { root: string }): AssembleNativeOptions {
  return { version: '0.4.0', signingKey: key.privateKey, now: new Date('2026-10-20T00:00:00Z'), ...overrides }
}

/** A host whose native channel publishes `version`, or answers `status`. */
function host(version: string | undefined, status = 404): typeof fetch {
  return (async () => {
    if (version === undefined) return new Response('', { status })
    const manifest: NativeManifest = {
      schema: 'bake-native-release/1', channel: 'stable', version, published: '2026-10-20T00:00:00Z',
      artifacts: { 'linux-x64': { file: archiveName(version, 'linux-x64'), sha256: 'a'.repeat(64), size: 42 } },
    }
    return new Response(nativeManifestBytes(manifest).toString('utf8'))
  }) as unknown as typeof fetch
}

describe('assembling', () => {
  test('writes the manifest, its signature, and the archives on the native route alone', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    const result = await assembleNativeRelease(options({ root, sourceDir, publicRoot }))
    const artifact = result.manifest.artifacts['linux-x64']
    if (artifact === undefined) throw new Error('the manifest lists no linux-x64 archive')
    const bytes = readFileSync(join(publicRoot, 'native/stable.json'))
    expect(bytes.toString('utf8')).toBe(
      `{"schema":"bake-native-release/1","channel":"stable","version":"0.4.0","published":"2026-10-20T00:00:00Z","artifacts":{"linux-x64":{"file":"bake-v0.4.0-linux-x64.tar.gz","sha256":"${artifact.sha256}","size":${artifact.size}}}}\n`,
    )
    // Signed as written: these exact bytes, not a re-serialization.
    const signature = Buffer.from(readFileSync(join(publicRoot, 'native/stable.json.sig'), 'utf8').trim(), 'base64')
    expect(verifySignature(null, bytes, key.publicKey, signature)).toBe(true)
    const archive = join(publicRoot, 'native/releases/0.4.0', archiveName('0.4.0', 'linux-x64'))
    expect(createHash('sha256').update(readFileSync(archive)).digest('hex')).toBe(artifact.sha256)
    expect(readFileSync(archive).byteLength).toBe(artifact.size)
  })

  test('leaves the frozen 0.3 route exactly as it was', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    await assembleNativeRelease(options({ root, sourceDir, publicRoot }))
    expect(readFileSync(join(publicRoot, 'latest.json'), 'utf8')).toBe('{"version":"0.3.8","artifacts":{}}\n')
    expect(readFileSync(join(publicRoot, 'latest.json.sig'), 'utf8')).toBe('frozen\n')
    expect(readdirSync(join(publicRoot, 'releases'))).toEqual(['0.3.8'])
    expect(readdirSync(join(publicRoot, 'releases/0.3.8'))).toEqual(['bake-v0.3.8-linux-x64.tar.gz'])
    expect(readdirSync(publicRoot).sort()).toEqual(['latest.json', 'latest.json.sig', 'native', 'releases'])
  })

  test('publishes every platform of the version, in platform order', async () => {
    const { root, sourceDir, publicRoot } = fixture('0.4.1', ['linux-x64', 'linux-arm64'])
    const result = await assembleNativeRelease(options({ root, sourceDir, publicRoot, version: '0.4.1' }))
    expect(Object.keys(result.manifest.artifacts)).toEqual(['linux-arm64', 'linux-x64'])
    expect(readdirSync(join(publicRoot, 'native/releases/0.4.1')).sort()).toEqual([...result.files].sort())
  })

  test.each(['darwin-arm64', 'darwin-x64', 'win32-x64'] as const)('refuses a %s archive, naming 0.4.x, and writes nothing', async (deferred) => {
    const { root, sourceDir, publicRoot } = fixture('0.4.0', ['linux-x64', 'linux-arm64', deferred])
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot })))
      .rejects.toThrow(`${archiveName('0.4.0', deferred)} is refused: ${deferredTargetMessage(deferred)}`)
    expect(existsSync(join(publicRoot, 'native'))).toBe(false)
  })

  test('a complete release needs exactly linux-arm64 and linux-x64', async () => {
    for (const targets of [['linux-x64'], ['linux-arm64']] as RustTarget[][]) {
      const { root, sourceDir, publicRoot } = fixture('0.4.0', targets)
      const missing = targets[0] === 'linux-x64' ? 'linux-arm64' : 'linux-x64'
      await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot, complete: true })))
        .rejects.toThrow(`A complete release needs ${archiveName('0.4.0', missing)} in ${sourceDir}`)
      expect(existsSync(join(publicRoot, 'native'))).toBe(false)
    }
    const { root, sourceDir, publicRoot } = fixture('0.4.0', ['linux-x64', 'linux-arm64'])
    const result = await assembleNativeRelease(options({ root, sourceDir, publicRoot, complete: true }))
    expect(Object.keys(result.manifest.artifacts)).toEqual(['linux-arm64', 'linux-x64'])
    // Without --complete, one platform publishes alone.
    const single = fixture('0.4.0', ['linux-arm64'])
    await expect(assembleNativeRelease(options({ ...single }))).resolves.toMatchObject({ files: [archiveName('0.4.0', 'linux-arm64')] })
  })

  test.each([
    ['a version that is not a native version', (f: ReturnType<typeof fixture>) => ({ ...f, version: '0.4' }), /Not a native release version: 0\.4/],
    ['a non-canonical pre-release', (f: ReturnType<typeof fixture>) => ({ ...f, version: '0.4.0-rc.01' }), /Not a native release version: 0\.4\.0-rc\.01/],
    ['a missing source directory', (f: ReturnType<typeof fixture>) => ({ ...f, sourceDir: join(f.root, 'absent') }), /No archives at .*absent/],
  ])('refuses %s', async (_what, edit, message) => {
    const { root, sourceDir, publicRoot, version } = { version: '0.4.0', ...edit(fixture()) }
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot, version }))).rejects.toThrow(message)
    expect(existsSync(join(publicRoot, 'native/stable.json'))).toBe(false)
  })

  test('refuses an archive of another version, platform, or name', async () => {
    for (const name of [archiveName('0.4.1', 'linux-x64'), 'bake-v0.4.0-freebsd-x64.tar.gz', 'notes.txt']) {
      const { root, sourceDir, publicRoot } = fixture()
      nativeArchive(sourceDir, name)
      await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot })))
        .rejects.toThrow(/is not a native archive of 0\.4\.0 for a release platform/)
      expect(existsSync(join(publicRoot, 'native/stable.json'))).toBe(false)
    }
  })

  test('refuses an archive that is not a native archive', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    rmSync(join(sourceDir, archiveName('0.4.0', 'linux-x64')))
    nativeArchive(sourceDir, archiveName('0.4.0', 'linux-x64'), {
      'bin/bake': '#!/usr/bin/env node\n', 'package.json': '{"name":"bake"}\n', 'apps/cli/lib/bin.js': 'console.log()\n',
    })
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot })))
      .rejects.toThrow(/holds (apps\/cli\/lib\/bin\.js|package\.json) of a TypeScript release/)
    expect(existsSync(join(publicRoot, 'native'))).toBe(false)
  })

  test('refuses an empty set of archives', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    rmSync(join(sourceDir, archiveName('0.4.0', 'linux-x64')))
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot }))).rejects.toThrow(/No native archives found/)
  })

  test('ignores the throwaway public key beside the archives', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    writeFileSync(join(sourceDir, EPHEMERAL_PUBLIC_KEY), `${testKey}\n`)
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot }))).resolves.toMatchObject({ files: [archiveName('0.4.0', 'linux-x64')] })
  })
})

describe('the channel never moves backward', () => {
  /** Publish `version` locally, then try to publish `next`. */
  async function republish(published: string, next: string, live?: { base: string; fetch: typeof fetch }): Promise<void> {
    const { root, publicRoot } = fixture(published)
    await assembleNativeRelease(options({ root, sourceDir: join(root, '.artifacts/bake-native-release', published), publicRoot, version: published }))
    const sourceDir = join(root, '.artifacts/bake-native-release', next)
    nativeArchive(sourceDir, archiveName(next, 'linux-x64'))
    await assembleNativeRelease(options({ root, sourceDir, publicRoot, version: next, ...live }))
  }

  test('accepts a strictly newer version', async () => {
    await expect(republish('0.4.0', '0.4.1')).resolves.toBeUndefined()
  })

  test.each([['0.4.0', 'the same version'], ['0.3.9', 'an older version']])('refuses %s, %s', async (next) => {
    await expect(republish('0.4.0', next)).rejects.toThrow(/is not newer than the native release 0\.4\.0 published at .*stable\.json/)
  })

  test('refuses a version the live channel already published', async () => {
    await expect(republish('0.4.0', '0.4.1', { base: 'https://bake.test', fetch: host('0.4.2') }))
      .rejects.toThrow(/0\.4\.1 is not newer than the native release 0\.4\.2 published at https:\/\/bake\.test\/native\/stable\.json/)
  })

  test('publishes the first native release when the live channel has none', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot, base: 'https://bake.test', fetch: host(undefined) })))
      .resolves.toMatchObject({ manifest: { version: '0.4.0' } })
  })

  test('refuses to publish when the live channel cannot be read', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot, base: 'https://bake.test', fetch: host(undefined, 503) })))
      .rejects.toThrow(/answered 503; pass --offline/)
    expect(existsSync(join(publicRoot, 'native'))).toBe(false)
  })

  test('checks no live channel when offline', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    const refuse = (() => { throw new Error('the network was used') }) as unknown as typeof fetch
    await expect(assembleNativeRelease(options({ root, sourceDir, publicRoot, fetch: refuse })))
      .resolves.toMatchObject({ manifest: { version: '0.4.0' } })
  })
})

describe('the signing key', () => {
  test('is a throwaway key whose public half is left beside the archives', () => {
    const sourceDir = join(scratch(), 'archives')
    const signing = nativeSigningKey({ root: scratch(), ephemeral: true, sourceDir })
    const published = readFileSync(join(sourceDir, EPHEMERAL_PUBLIC_KEY), 'utf8').trim()
    expect(createPublicKey(signing).export({ type: 'spki', format: 'der' }).toString('base64')).toBe(published)
  })

  test('is refused while the committed key is a placeholder', () => {
    const root = scratch()
    mkdirSync(join(root, 'distribution/host'), { recursive: true })
    writeFileSync(join(root, NATIVE_KEY_PLACEHOLDER), 'placeholder\n')
    expect(() => nativeSigningKey({ root, ephemeral: false, sourceDir: root, keyFile: 'unused' })).toThrow(/still a placeholder/)
  })

  test('must be named, Ed25519, and the committed key', () => {
    const root = scratch()
    mkdirSync(join(root, 'distribution/host'), { recursive: true })
    writeFileSync(join(root, NATIVE_KEY_FILE), `${testKey}\n`)
    expect(() => nativeSigningKey({ root, ephemeral: false, sourceDir: root })).toThrow(/Name the native release signing key/)
    const other = join(root, 'other.pem')
    writeFileSync(other, generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'pem' }) as string)
    expect(() => nativeSigningKey({ root, ephemeral: false, sourceDir: root, keyFile: other }))
      .toThrow(/does not match distribution\/host\/native-release-key\.pub/)
    const rsa = join(root, 'rsa.pem')
    writeFileSync(rsa, generateKeyPairSync('rsa', { modulusLength: 2048 }).privateKey.export({ type: 'pkcs8', format: 'pem' }) as string)
    expect(() => nativeSigningKey({ root, ephemeral: false, sourceDir: root, keyFile: rsa })).toThrow(/is not Ed25519/)
    const owner = join(root, 'owner.pem')
    writeFileSync(owner, key.privateKey.export({ type: 'pkcs8', format: 'pem' }) as string)
    expect((nativeSigningKey({ root, ephemeral: false, sourceDir: root, keyFile: owner }) as KeyObject).asymmetricKeyType).toBe('ed25519')
  })
})

describe('a round trip through the host', () => {
  /** Start the download service on `root`, and answer with its base URL. */
  async function serve(root: string): Promise<string> {
    const server = Bun.spawn(['node', join(root, 'server.mjs')], { env: { ...process.env, PORT: '0' }, stdout: 'pipe', stderr: 'inherit' })
    servers.push(server)
    const reader = server.stdout.getReader()
    let output = ''
    while (!output.includes('\n')) {
      const next = await reader.read()
      if (next.done) throw new Error('server exited before listening')
      output += new TextDecoder().decode(next.value)
    }
    reader.releaseLock()
    const port = /listening on (\d+)/.exec(output)?.[1]
    if (port === undefined) throw new Error(`unexpected server output: ${output}`)
    return `http://127.0.0.1:${port}`
  }

  test('packs, assembles, verifies, serves, and is read back', async () => {
    const { root, sourceDir, publicRoot } = fixture()
    const host = join(root, 'distribution/host')
    for (const file of ['server.mjs', 'handler.mjs', 'verify-native-manifest.mjs']) copyFileSync(join(ROOT, 'distribution/host', file), join(host, file))
    copyFileSync(join(ROOT, NATIVE_KEY_FILE), join(host, 'native-release-key.pub'))
    const signingKey = nativeSigningKey({ root, ephemeral: true, sourceDir })
    const ephemeralKey = readFileSync(join(sourceDir, EPHEMERAL_PUBLIC_KEY), 'utf8').trim()
    const result = await assembleNativeRelease(options({ root, sourceDir, publicRoot, signingKey }))

    // The deploy's gate, told to trust the throwaway key as a local check does.
    const gate = Bun.spawnSync(['node', join(host, 'verify-native-manifest.mjs')], {
      cwd: host, stdout: 'pipe', stderr: 'pipe', timeout: 20_000,
      env: { ...process.env, BAKE_NATIVE_RELEASE_PUBLIC_KEY: ephemeralKey },
    })
    expect(gate.stderr.toString()).toBe('')
    expect(gate.exitCode).toBe(0)
    expect(gate.stdout.toString()).toContain('Verified native Bake 0.4.0 release: linux-x64')

    const base = await serve(host)
    const manifest = await fetch(`${base}/native/stable.json`)
    expect(manifest.headers.get('content-type')).toBe('application/json; charset=utf-8')
    const bytes = new Uint8Array(await manifest.arrayBuffer())
    expect(Buffer.from(bytes).equals(result.bytes)).toBe(true)
    const signature = Buffer.from(await (await fetch(`${base}/native/stable.json.sig`)).text(), 'base64')
    const trusted = createPublicKey({ key: Buffer.from(ephemeralKey, 'base64'), format: 'der', type: 'spki' })
    expect(verifySignature(null, bytes, trusted, signature)).toBe(true)
    const parsed = parseNativeManifest(new TextDecoder().decode(bytes))
    const artifact = parsed.artifacts['linux-x64']
    if (artifact === undefined) throw new Error('the manifest lists no linux-x64 archive')
    const archive = await fetch(`${base}/native/releases/${parsed.version}/${artifact.file}`)
    expect(archive.headers.get('cache-control')).toBe('public, max-age=31536000, immutable')
    const downloaded = Buffer.from(new Uint8Array(await archive.arrayBuffer()))
    expect(downloaded.byteLength).toBe(artifact.size)
    expect(createHash('sha256').update(downloaded).digest('hex')).toBe(artifact.sha256)
    // The 0.3 route still answers its frozen manifest, and never the native archive.
    expect(await (await fetch(`${base}/latest.json`)).text()).toBe('{"version":"0.3.8","artifacts":{}}\n')
    expect((await fetch(`${base}/releases/0.4.0/${artifact.file}`)).status).toBe(404)
  })
})
