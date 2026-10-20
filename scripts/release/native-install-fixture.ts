/**
 * What a native installer test needs: ephemeral signing keys, a native
 * release archive in `pack-rust.ts`'s layout, a `bake-native-release/1`
 * manifest, a loopback host serving them, and a private copy of the shipped
 * installer that trusts the ephemeral key.
 *
 * The key reaches the installer by replacing the one embedded line in a copy
 * of the published script, never through its environment: the published
 * installers trust exactly the key they carry, so nothing a caller sets can
 * steer them.
 */
import { spawnSync } from 'node:child_process'
import { createHash, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const HOST = resolve(import.meta.dir, '../../distribution/host')

/** An Ed25519 keypair, with the public half as the installers embed it. */
export interface EphemeralKey {
  readonly publicKey: string
  readonly privateKey: KeyObject
}

export function ephemeralKey(): EphemeralKey {
  const pair = generateKeyPairSync('ed25519')
  return { publicKey: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'), privateKey: pair.privateKey }
}

/** One platform's entry in a native manifest. */
export interface NativeArtifact {
  readonly file: string
  readonly sha256: string
  readonly size: number
}

/** A packed native release, as the host serves it. */
export interface NativeRelease {
  readonly version: string
  readonly target: string
  readonly file: string
  readonly bytes: Buffer
  readonly artifact: NativeArtifact
}

/** What the stub release's executable reports and how it behaves. */
export interface StubOptions {
  /** The version its `--self-check` record states; the manifest version by default. */
  readonly reportedVersion?: string
  /** The platform its `--self-check` record states; the archive's target by default. */
  readonly reportedTarget?: string
  /** Exit status of `--self-check`; 0 by default. */
  readonly selfCheckStatus?: number
  /** Seconds `--self-check` sleeps before answering; 0 by default. */
  readonly selfCheckDelay?: number
  /** An extra archive member, to check that unsafe members are refused. */
  readonly extraMember?: { readonly kind: 'symlink' | 'parent'; readonly name: string }
  /** The executable's name inside `bin/`; `bake` by default, `bake.exe` for a Windows archive. */
  readonly executableName?: string
}

/**
 * Build a native release archive holding `pack-rust.ts`'s layout, whose
 * `bin/bake` is a small script that answers `--version` and `--self-check` as
 * the real binary does.
 * @param directory - a private directory the archive is staged and written in.
 */
export function stubRelease(directory: string, version: string, target: string, options: StubOptions = {}): NativeRelease {
  const stage = join(directory, `stage-${version}`)
  mkdirSync(join(stage, 'bin'), { recursive: true })
  mkdirSync(join(stage, 'notices/bake-cli'), { recursive: true })
  mkdirSync(join(stage, 'licenses', target), { recursive: true })
  const record = JSON.stringify({
    kind: 'bake-rs-self-check',
    schema: 1,
    version: options.reportedVersion ?? version,
    runtime: 'rust',
    target: options.reportedTarget ?? target,
    docs: { cli: 'f'.repeat(64) },
  })
  const executable = options.executableName ?? 'bake'
  writeFileSync(join(stage, 'bin', executable), `#!/bin/sh
case \${1:-} in
  --self-check)
    sleep ${options.selfCheckDelay ?? 0}
    printf '%s\\n' '${record}'
    exit ${options.selfCheckStatus ?? 0}
    ;;
  --version) printf 'bake %s\\n' '${version}' ;;
  *) printf 'bake %s: %s\\n' '${version}' "\${*:-no arguments}" ;;
esac
`)
  chmodSync(join(stage, 'bin', executable), 0o755)
  writeFileSync(join(stage, 'LICENSE'), 'MIT\n')
  writeFileSync(join(stage, 'THIRD_PARTY_NOTICES.md'), '# Notices\n')
  writeFileSync(join(stage, 'notices/bake-cli/NOTICE'), 'bake-cli\n')
  writeFileSync(join(stage, 'licenses/inventory.json'), '{"schema":"bake-native-licenses/1"}\n')
  writeFileSync(join(stage, 'licenses', target, 'THIRD-PARTY-LICENSES.txt'), 'licenses\n')
  const members = ['bin', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'notices', 'licenses']
  if (options.extraMember?.kind === 'symlink') {
    spawnSync('ln', ['-s', '/etc/passwd', join(stage, options.extraMember.name)], { stdio: 'ignore' })
    members.push(options.extraMember.name)
  }
  const file = `bake-rs-v${version}-${target}.tar.gz`
  const archive = join(directory, file)
  const names = options.extraMember?.kind === 'parent' ? [...members, options.extraMember.name] : members
  const packed = spawnSync('tar', ['-czf', archive, '-C', stage, ...names], { encoding: 'utf8' })
  if (packed.status !== 0) throw new Error(`tar exited ${String(packed.status)}: ${packed.stderr}`)
  const bytes = readFileSync(archive)
  return {
    version, target, file, bytes,
    artifact: { file, sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.byteLength },
  }
}

/** The manifest members, in the order `bake-native-release/1` fixes. */
export interface NativeManifest {
  readonly schema?: string
  readonly channel?: string
  readonly version: string
  readonly published?: string
  readonly artifacts: Record<string, NativeArtifact>
  /** An unknown member, which the installer must refuse. */
  readonly extra?: unknown
}

/** Serialize a manifest as the release host serves it. */
export function manifestBytes(manifest: NativeManifest): Buffer {
  const body: Record<string, unknown> = {
    schema: manifest.schema ?? 'bake-native-release/1',
    channel: manifest.channel ?? 'stable',
    version: manifest.version,
    published: manifest.published ?? '2026-10-20T00:00:00Z',
    artifacts: manifest.artifacts,
  }
  if (manifest.extra !== undefined) body['extra'] = manifest.extra
  return Buffer.from(JSON.stringify(body))
}

/** What the loopback host serves. */
export interface HostFiles {
  readonly manifest: Buffer
  readonly signature: string
  readonly releases: ReadonlyMap<string, Buffer>
}

/** Sign a manifest's exact bytes, as the release owner's key does. */
export function signManifest(bytes: Buffer, key: EphemeralKey): string {
  return sign(null, bytes, key.privateKey).toString('base64')
}

/** The host the installer downloads from, which the test owns and closes. */
export interface ReleaseHost {
  readonly origin: string
  readonly requests: readonly string[]
  stop(): void
}

/**
 * Serve `/native/stable.json`, its signature, and `/native/releases/...` on
 * loopback, from files the caller may replace between installs.
 */
export function startReleaseHost(files: () => HostFiles): ReleaseHost {
  const requests: string[] = []
  const server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    fetch(request) {
      const { pathname } = new URL(request.url)
      requests.push(pathname)
      const served = files()
      if (pathname === '/native/stable.json') return new Response(new Uint8Array(served.manifest))
      if (pathname === '/native/stable.json.sig') return new Response(`${served.signature}\n`)
      const release = served.releases.get(pathname)
      return release === undefined ? new Response('not found', { status: 404 }) : new Response(new Uint8Array(release))
    },
  })
  return {
    origin: `http://127.0.0.1:${String(server.port)}`,
    requests,
    stop(): void { server.stop(true) },
  }
}

/** The release path a packed archive is served from. */
export function releasePath(release: NativeRelease): string {
  return `/native/releases/${release.version}/${release.file}`
}

/** What one installer run reported. */
export interface InstallerRun {
  readonly status: number
  readonly stdout: string
  readonly stderr: string
}

/** No installer run in these tests may take longer; the host is on loopback. */
export const INSTALLER_RUN_TIMEOUT_MS = 60_000

/**
 * Run an installer, bounded, and collect both streams.
 *
 * Asynchronous on purpose: the release host runs in this process, so a
 * blocking spawn would stop it answering the installer's own download.
 * @param argv - the command and its arguments.
 * @param env - the whole environment the installer runs with.
 * @param timeoutMs - how long the run may take before its process group is killed.
 */
export async function runInstaller(
  argv: readonly string[], env: Record<string, string>, timeoutMs = INSTALLER_RUN_TIMEOUT_MS,
): Promise<InstallerRun> {
  const child = Bun.spawn([...argv], {
    stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', detached: process.platform !== 'win32', env,
  })
  let killed = false
  const watchdog = setTimeout(() => {
    killed = true
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
  }, timeoutMs)
  const [status, stdout, stderr] = await Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
  ])
  clearTimeout(watchdog)
  if (killed) throw new Error(`${argv.join(' ')} did not finish within ${String(timeoutMs)} ms\n${stdout}\n${stderr}`)
  return { status, stdout, stderr }
}

/** The line each installer embeds the native release key in. */
const KEY_LINES = {
  'install.sh': /^native_release_key='[^']*'$/mu,
  'install.ps1': /^\$nativeReleaseKey = '[^']*'$/mu,
} as const

/** The key an installer carries, read from its one embedded line. */
export function embeddedKey(installer: keyof typeof KEY_LINES): string {
  const source = readFileSync(join(HOST, installer), 'utf8')
  const line = KEY_LINES[installer].exec(source)?.[0]
  if (line === undefined) throw new Error(`${installer} has no embedded native release key line`)
  return line.slice(line.indexOf('\'') + 1, -1)
}

/** The region of `install.ps1` holding its Ed25519 verifier, which the tests dot-source. */
export function ed25519VerifierRegion(): string {
  const source = readFileSync(join(HOST, 'install.ps1'), 'utf8')
  const region = /# BEGIN ED25519 VERIFIER\n([\s\S]*?)# END ED25519 VERIFIER/u.exec(source)?.[1]
  if (region === undefined) throw new Error('install.ps1 has no Ed25519 verifier region')
  return region
}

/**
 * A private copy of the shipped installer that trusts `key`: the one embedded
 * key line is replaced, and nothing else.
 * @returns the copy's path.
 */
export function installerTrusting(installer: keyof typeof KEY_LINES, key: string, directory: string): string {
  const source = readFileSync(join(HOST, installer), 'utf8')
  const replacement = installer === 'install.sh' ? `native_release_key='${key}'` : `$nativeReleaseKey = '${key}'`
  const next = source.replace(KEY_LINES[installer], replacement)
  if (next === source) throw new Error(`${installer} has no embedded native release key line`)
  const path = join(directory, installer)
  writeFileSync(path, next)
  chmodSync(path, 0o755)
  return path
}
