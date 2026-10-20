/**
 * The shipped `distribution/host/install.sh`, run end to end against a
 * loopback release host with ephemeral signing keys: a fresh install, the
 * `bake` takeover beside a simulated 0.3 install and its restoration, a
 * user-owned command, refused manifests and archives, a failed self-check,
 * and the `<root>/install.lock` directory the Rust updater shares.
 *
 * Every scenario owns its home, command directory, install root, and host.
 * No Node runs inside the installer: it is POSIX sh over curl, tar,
 * sha256sum, and openssl.
 */
import { afterEach, expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import {
  chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  embeddedKey, ephemeralKey, installerTrusting, manifestBytes, releasePath, signManifest, startReleaseHost, stubRelease,
  type HostFiles, type NativeRelease, type ReleaseHost, type StubOptions,
} from './native-install-fixture.ts'

const roots: string[] = []
const hosts: ReleaseHost[] = []
afterEach(() => {
  for (const host of hosts.splice(0)) host.stop()
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

const TARGET = `${process.platform === 'darwin' ? 'darwin' : 'linux'}-${process.arch === 'arm64' ? 'arm64' : 'x64'}`

/** One scenario's private world: a home, a command directory, a host, and the installer that trusts its key. */
interface Scenario {
  readonly directory: string
  readonly home: string
  readonly root: string
  readonly binDir: string
  readonly installer: string
  readonly host: ReleaseHost
  /** Replace what the host serves. */
  serve(release: NativeRelease, manifest?: Buffer, signature?: string): void
  /** Serve a release and its manifest, signed by the ephemeral key. */
  publish(version: string, options?: StubOptions): NativeRelease
  /** The ephemeral key's signature over `bytes`, for a manifest changed after signing. */
  sign(bytes: Buffer): string
  run(args?: readonly string[], env?: Record<string, string>): Promise<InstallerRun>
}

/** What one installer run reported. */
interface InstallerRun {
  readonly status: number
  readonly stdout: string
  readonly stderr: string
}

/** No installer run in these tests may take longer; the host is on loopback and the releases are tiny. */
const RUN_TIMEOUT_MS = 60_000

function scenario(): Scenario {
  const directory = mkdtempSync(join(tmpdir(), 'bake-native-install-'))
  roots.push(directory)
  const key = ephemeralKey()
  const home = join(directory, 'home')
  const root = join(home, '.local/share/bake-rs')
  const binDir = join(home, '.local/bin')
  mkdirSync(binDir, { recursive: true })
  let files: HostFiles = { manifest: Buffer.from('{}'), signature: '', releases: new Map() }
  const host = startReleaseHost(() => files)
  hosts.push(host)
  const installer = installerTrusting('install.sh', key.publicKey, directory)
  return {
    directory, home, root, binDir, installer, host,
    serve(release, manifest, signature): void {
      const bytes = manifest ?? manifestBytes({ version: release.version, artifacts: { [release.target]: release.artifact } })
      files = {
        manifest: bytes,
        signature: signature ?? signManifest(bytes, key),
        releases: new Map([[releasePath(release), release.bytes]]),
      }
    },
    sign(bytes): string {
      return signManifest(bytes, key)
    },
    publish(version, options): NativeRelease {
      const release = stubRelease(directory, version, TARGET, options)
      this.serve(release)
      return release
    },
    // Asynchronous: the release host runs in this process, so a blocking
    // spawn would stop it answering the installer's own download.
    async run(args = [], env = {}): Promise<InstallerRun> {
      const child = Bun.spawn(['sh', installer, ...args], {
        stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', detached: true,
        env: {
          PATH: process.env['PATH'] ?? '/usr/bin:/bin',
          HOME: home,
          TMPDIR: join(directory, 'tmp'),
          BAKE_DOWNLOAD_URL: host.origin,
          BAKE_NATIVE_ROOT: root,
          BAKE_BIN_DIR: binDir,
          ...env,
        },
      })
      let killed = false
      const watchdog = setTimeout(() => {
        killed = true
        try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
      }, RUN_TIMEOUT_MS)
      const [status, stdout, stderr] = await Promise.all([
        child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
      ])
      clearTimeout(watchdog)
      if (killed) throw new Error(`The installer did not finish within ${String(RUN_TIMEOUT_MS)} ms\n${stdout}\n${stderr}`)
      return { status, stdout, stderr }
    },
  }
}

function setup(directory: string): void {
  mkdirSync(join(directory, 'tmp'), { recursive: true })
}

/** A Bake 0.3 install: its own root, its `bake` and `dsh` links, and its data. */
function legacyInstall(home: string): { root: string; target: string } {
  const root = join(home, '.local/share/bake')
  const release = join(root, 'versions/0.3.8-abcdef012345')
  mkdirSync(join(release, 'bin'), { recursive: true })
  writeFileSync(join(release, 'bin/bake'), '#!/bin/sh\nprintf \'bake 0.3.8\\n\'\n')
  chmodSync(join(release, 'bin/bake'), 0o755)
  mkdirSync(join(home, '.bake'), { recursive: true })
  writeFileSync(join(home, '.bake/settings.json'), '{"model":"kept"}\n')
  symlinkSync(release, join(root, 'current'))
  const target = join(root, 'current/bin/bake')
  const binDir = join(home, '.local/bin')
  mkdirSync(binDir, { recursive: true })
  symlinkSync(target, join(binDir, 'bake'))
  symlinkSync(target, join(binDir, 'dsh'))
  return { root, target }
}

function installedVersion(root: string): string {
  return readlinkSync(join(root, 'current')).replace(/^releases\//u, '')
}

function runCommand(path: string, args: readonly string[]): string {
  const result = spawnSync(path, [...args], { encoding: 'utf8', timeout: 30_000 })
  if (result.status !== 0) throw new Error(`${path} exited ${String(result.status)}: ${result.stderr ?? ''}`)
  return result.stdout ?? ''
}

test('installs from an empty home and links bake to the current release', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0')
  const result = await world.run()
  expect(result.stderr).toBe('')
  expect(result.status).toBe(0)
  expect(installedVersion(world.root)).toBe('0.4.0')
  expect(readlinkSync(join(world.root, 'current'))).toBe('releases/0.4.0')
  expect(existsSync(join(world.root, 'previous'))).toBe(false)
  expect(readlinkSync(join(world.binDir, 'bake'))).toBe(join(world.root, 'current/bin/bake'))
  expect(existsSync(join(world.binDir, 'bake-rs'))).toBe(false)
  expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.0\n')
  expect(JSON.parse(readFileSync(join(world.root, 'links.json'), 'utf8'))).toEqual({
    schema: 'bake-native-links/1',
    links: [{ path: join(world.binDir, 'bake'), replaced: null }],
  })
  expect(existsSync(join(world.root, 'install.lock'))).toBe(false)
})

test('a second release becomes current, records the one it replaced, and keeps only those two', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0')
  expect((await world.run()).status).toBe(0)
  world.publish('0.4.1')
  expect((await world.run()).status).toBe(0)
  expect(installedVersion(world.root)).toBe('0.4.1')
  expect(readlinkSync(join(world.root, 'previous'))).toBe('releases/0.4.0')
  world.publish('0.4.2')
  expect((await world.run()).status).toBe(0)
  expect(installedVersion(world.root)).toBe('0.4.2')
  expect(readlinkSync(join(world.root, 'previous'))).toBe('releases/0.4.1')
  expect(existsSync(join(world.root, 'releases/0.4.0'))).toBe(false)
  expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.2\n')
})

test('refuses a manifest older than the installed release', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.1')
  expect((await world.run()).status).toBe(0)
  world.publish('0.4.0')
  const result = await world.run()
  expect(result.status).not.toBe(0)
  expect(result.stderr).toContain('older than the installed 0.4.1')
  expect(installedVersion(world.root)).toBe('0.4.1')
})

test('takes over a Bake-managed 0.3 bake, leaves dsh and the 0.3 install alone, and restores them', async () => {
  const world = scenario()
  setup(world.directory)
  const legacy = legacyInstall(world.home)
  world.publish('0.4.0')
  const result = await world.run()
  expect(result.status).toBe(0)
  expect(result.stdout).toContain('Taking over')
  expect(readlinkSync(join(world.binDir, 'bake'))).toBe(join(world.root, 'current/bin/bake'))
  expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.0\n')
  // The 0.3 install keeps its command, its release, and its data.
  expect(readlinkSync(join(world.binDir, 'dsh'))).toBe(legacy.target)
  expect(runCommand(join(world.binDir, 'dsh'), [])).toBe('bake 0.3.8\n')
  expect(readlinkSync(join(legacy.root, 'current'))).toBe(join(legacy.root, 'versions/0.3.8-abcdef012345'))
  expect(readFileSync(join(world.home, '.bake/settings.json'), 'utf8')).toBe('{"model":"kept"}\n')
  expect(JSON.parse(readFileSync(join(world.root, 'links.json'), 'utf8'))).toEqual({
    schema: 'bake-native-links/1',
    links: [{ path: join(world.binDir, 'bake'), replaced: { kind: 'symlink', target: legacy.target } }],
  })

  const restored = await world.run(['--uninstall-links'])
  expect(restored.status).toBe(0)
  expect(readlinkSync(join(world.binDir, 'bake'))).toBe(legacy.target)
  expect(runCommand(join(world.binDir, 'bake'), [])).toBe('bake 0.3.8\n')
  expect(readlinkSync(join(world.binDir, 'dsh'))).toBe(legacy.target)
  expect(existsSync(join(world.root, 'releases/0.4.0'))).toBe(true)
  expect(existsSync(join(world.root, 'links.json'))).toBe(false)
})

test('keeps a user-owned bake, installs the release, and replaces it only when asked', async () => {
  const world = scenario()
  setup(world.directory)
  const command = join(world.binDir, 'bake')
  writeFileSync(command, '#!/bin/sh\nprintf \'someone else\\n\'\n')
  chmodSync(command, 0o755)
  world.publish('0.4.0')

  const kept = await world.run()
  expect(kept.status).not.toBe(0)
  expect(kept.stderr).toContain(command)
  expect(kept.stderr).toContain('--replace-command')
  expect(lstatSync(command).isSymbolicLink()).toBe(false)
  expect(readFileSync(command, 'utf8')).toContain('someone else')
  expect(existsSync(join(world.root, 'links.json'))).toBe(false)
  expect(installedVersion(world.root)).toBe('0.4.0')

  const replaced = await world.run([], { BAKE_REPLACE_COMMAND: '1' })
  expect(replaced.status).toBe(0)
  expect(readlinkSync(command)).toBe(join(world.root, 'current/bin/bake'))
  const links = JSON.parse(readFileSync(join(world.root, 'links.json'), 'utf8')) as {
    links: { path: string; replaced: { kind: string; backup: string } }[]
  }
  const [link] = links.links
  expect(link?.replaced.kind).toBe('file')
  expect(readFileSync(link?.replaced.backup ?? '', 'utf8')).toContain('someone else')

  expect((await world.run(['--uninstall-links'])).status).toBe(0)
  expect(lstatSync(command).isSymbolicLink()).toBe(false)
  expect(readFileSync(command, 'utf8')).toContain('someone else')
})

test('a tampered signature, hash, or truncated archive leaves the working install as it was', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0')
  expect((await world.run()).status).toBe(0)

  const next = stubRelease(world.directory, '0.4.1', TARGET)
  const manifest = manifestBytes({ version: next.version, artifacts: { [next.target]: next.artifact } })
  const working = (): void => {
    expect(installedVersion(world.root)).toBe('0.4.0')
    expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.0\n')
  }

  world.serve(next, manifest, Buffer.alloc(64, 7).toString('base64'))
  expect((await world.run()).stderr).toContain('signature did not verify')
  working()

  // Changed after signing: the host serves the signature of the manifest it no longer holds.
  world.serve(next, Buffer.from(manifest.toString('utf8').replace('"size"', '"size" ')), world.sign(manifest))
  expect((await world.run()).stderr).toContain('signature did not verify')
  working()

  const wrongHash = manifestBytes({
    version: next.version,
    artifacts: { [next.target]: { ...next.artifact, sha256: 'a'.repeat(64) } },
  })
  world.serve(next, wrongHash)
  expect((await world.run()).stderr).toContain("did not match the manifest's SHA-256")
  working()

  const truncated: NativeRelease = { ...next, bytes: next.bytes.subarray(0, next.bytes.byteLength - 64) }
  world.serve(truncated, manifest)
  expect((await world.run()).stderr).toContain('not the')
  working()
  expect(existsSync(join(world.root, 'releases/0.4.1'))).toBe(false)
})

test('refuses a manifest with an unknown member, another schema, or a renamed archive', async () => {
  const world = scenario()
  setup(world.directory)
  const release = stubRelease(world.directory, '0.4.0', TARGET)
  const artifacts = { [release.target]: release.artifact }

  world.serve(release, manifestBytes({ version: '0.4.0', artifacts, extra: 'unexpected' }))
  expect((await world.run()).stderr).toContain('not a bake-native-release/1 manifest')

  world.serve(release, manifestBytes({ schema: 'bake-release/1', version: '0.4.0', artifacts }))
  expect((await world.run()).stderr).toContain('not a bake-native-release/1 manifest')

  world.serve(release, manifestBytes({ channel: 'beta', version: '0.4.0', artifacts }))
  expect((await world.run()).stderr).toContain('not a bake-native-release/1 manifest')

  world.serve(release, manifestBytes({
    version: '0.4.0',
    artifacts: { [release.target]: { ...release.artifact, sha256: 'NOTHEX'.padEnd(64, 'f') } },
  }))
  expect((await world.run()).stderr).toContain('not a bake-native-release/1 manifest')

  // The TypeScript route's archive name, and a name from another version.
  for (const file of [`bake-v0.4.0-${release.target}.tar.gz`, `bake-rs-v0.4.1-${release.target}.tar.gz`]) {
    world.serve(release, manifestBytes({ version: '0.4.0', artifacts: { [release.target]: { ...release.artifact, file } } }))
    expect((await world.run()).stderr).toContain('names the archive')
  }
  expect(existsSync(join(world.root, 'current'))).toBe(false)
})

test('refuses an archive holding a link', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0', { extraMember: { kind: 'symlink', name: 'escape' } })
  const result = await world.run()
  expect(result.status).not.toBe(0)
  expect(result.stderr).toContain('link')
  expect(existsSync(join(world.root, 'current'))).toBe(false)
})

test('a failed self-check leaves the pointer where it was', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0')
  expect((await world.run()).status).toBe(0)

  world.publish('0.4.1', { selfCheckStatus: 1 })
  expect((await world.run()).stderr).toContain('failed its self-check')
  expect(installedVersion(world.root)).toBe('0.4.0')

  world.publish('0.4.2', { reportedVersion: '9.9.9' })
  expect((await world.run()).stderr).toContain('does not report version 0.4.2')
  expect(installedVersion(world.root)).toBe('0.4.0')

  world.publish('0.4.3', { reportedTarget: 'win32-x64' })
  expect((await world.run()).stderr).toContain('does not report platform')
  expect(installedVersion(world.root)).toBe('0.4.0')
  expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.0\n')
})

test('the install lock refuses a second install, is taken over when its owner is gone, and waits out a lock with no owner', async () => {
  const world = scenario()
  setup(world.directory)
  world.publish('0.4.0')
  const lock = join(world.root, 'install.lock')
  mkdirSync(lock, { recursive: true })
  writeFileSync(join(lock, 'owner'), `${String(process.pid)} ${String(Math.floor(Date.now() / 1000))} install\n`)

  const refused = await world.run()
  expect(refused.status).not.toBe(0)
  expect(refused.stderr).toContain('another Bake install holds')
  expect(existsSync(join(world.root, 'current'))).toBe(false)
  expect(existsSync(join(lock, 'owner'))).toBe(true)

  // A lock whose owner process is gone is taken over once. Process id 2^22 is
  // above every Linux and macOS pid, so no live process answers for it.
  writeFileSync(join(lock, 'owner'), `4194304 ${String(Math.floor(Date.now() / 1000))} update\n`)
  expect((await world.run()).status).toBe(0)
  expect(installedVersion(world.root)).toBe('0.4.0')
  expect(existsSync(lock)).toBe(false)

  // No owner file: held for ten seconds, then stale.
  mkdirSync(lock, { recursive: true })
  world.publish('0.4.1')
  const waited = await world.run()
  expect(waited.status).toBe(0)
  expect(installedVersion(world.root)).toBe('0.4.1')
}, 60_000)

test('two installs at once: one takes the lock and the other is refused', async () => {
  const world = scenario()
  setup(world.directory)
  // The self-check holds the lock long enough for the second install to meet it.
  world.publish('0.4.0', { selfCheckDelay: 5 })
  const results = await Promise.all([world.run(), world.run()])
  expect(results.filter(one => one.status === 0)).toHaveLength(1)
  expect(results.find(one => one.status !== 0)?.stderr).toContain('another Bake install holds')
  expect(installedVersion(world.root)).toBe('0.4.0')
  expect(existsSync(join(world.root, 'install.lock'))).toBe(false)
  expect(runCommand(join(world.binDir, 'bake'), ['--version'])).toBe('bake 0.4.0\n')
}, 60_000)

test('both installers embed the same native release key, and the committed key when it exists', async () => {
  const shell = embeddedKey('install.sh')
  expect(shell).toMatch(/^MCowBQYDK2Vw[A-Za-z0-9+/]+=$/u)
  expect(embeddedKey('install.ps1')).toBe(shell)
  // distribution/host/native-release-key.pub is the published key; until it
  // exists the installers carry a placeholder whose private half was
  // discarded. A committed key must equal every embedded copy.
  const file = join(import.meta.dir, '../../distribution/host/native-release-key.pub')
  if (existsSync(file)) expect(readFileSync(file, 'utf8').trim()).toBe(shell)
})
