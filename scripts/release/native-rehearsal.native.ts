/**
 * A native release rehearsal on this host: the real `bake` this checkout
 * builds is archived in `pack-rust.ts`'s layout, published to a loopback host
 * with an ephemeral key, and installed by the shipped `distribution/host/
 * install.sh`. The installed command then answers `--version` and runs the
 * real `--self-check`, so the self-check the installer requires before the
 * pointer moves is the binary's own, not a fixture's.
 *
 * A second, fixture version follows it, to leave `previous` naming the real
 * release the rollback returns to. Only the fixture's self-check is faked:
 * a second real version would need a second Cargo version of this checkout.
 *
 * The rehearsal's archive carries the real binary and the repository's
 * LICENSE and THIRD_PARTY_NOTICES.md; its license bundle files are
 * placeholders, because generating the real bundle needs cargo-about, which
 * `bun run release:pack:rust` and its own check (`test:rust:package`) cover.
 * Keyless: no model, provider, or network host outside loopback. Run with
 * `bun run test:rust:install`, after the Rust workspace is built.
 *
 * `BAKE_REHEARSAL_ARCHIVE` names an archive `bun run release:pack:rust` made
 * on this host instead, which must be this checkout's version for this
 * platform: the release workflow installs the archive it is about to
 * publish this way, real license bundle included, on each release platform.
 *
 * What this rehearsal does not cover, and release qualification still needs:
 * an update and rollback driven by `bake update` itself, and the D46 Wasm veto
 * plugin run against the installed artifact. The Windows installer's run on
 * Windows waits for the 0.4.x Windows release.
 */
import { afterEach, expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { archiveName, hostTarget, isRustTarget, payload, workspaceInputs, type RustTarget } from './pack-rust.ts'
import {
  ephemeralKey, installerTrusting, manifestBytes, runInstaller, signManifest, startReleaseHost, stubRelease,
  type HostFiles, type NativeArtifact, type ReleaseHost,
} from './native-install-fixture.ts'
import { createHash } from 'node:crypto'

const ROOT = resolve(import.meta.dir, '../..')
const roots: string[] = []
const hosts: ReleaseHost[] = []
afterEach(() => {
  for (const host of hosts.splice(0)) host.stop()
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

/** What this checkout builds, read from Cargo. */
function workspace(): { version: string; target: RustTarget; binary: string; notices: Record<string, string> } {
  const metadata = spawnSync('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1'], {
    cwd: join(ROOT, 'rust'), encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
  })
  if (metadata.status !== 0) throw new Error(`cargo metadata exited ${String(metadata.status)}: ${metadata.stderr ?? ''}`)
  const inputs = workspaceInputs(JSON.parse(metadata.stdout) as Parameters<typeof workspaceInputs>[0])
  const target = hostTarget()
  if (!isRustTarget(target)) throw new Error(`This host is ${target}, which Bake publishes no native release for`)
  const binary = join(inputs.targetDir, 'debug', process.platform === 'win32' ? 'bake.exe' : 'bake')
  return { version: inputs.version, target, binary, notices: inputs.notices }
}

/**
 * A copy of the built binary without its debug information.
 *
 * This checkout's debug `bake` unpacks to about 760 MB, past the 512 MiB
 * an install accepts; a release binary is a fraction of that. The rehearsal
 * needs the binary to run its own `--self-check`, not to be debuggable, and
 * `strip -S` removes debug symbols with both GNU binutils and Apple's strip.
 */
function strippedBinary(directory: string, binary: string): string {
  const copy = join(directory, basename(binary))
  copyFileSync(binary, copy)
  chmodSync(copy, 0o755)
  const stripped = spawnSync('strip', ['-S', copy], { encoding: 'utf8' })
  if (stripped.status !== 0) {
    throw new Error(`strip -S exited ${String(stripped.status)}: ${stripped.stderr ?? ''}`)
  }
  return copy
}

/** An archive staged for the rehearsal's host: its name, where it is, and what the manifest says of it. */
interface Rehearsed {
  readonly file: string
  readonly path: string
  readonly artifact: NativeArtifact
}

/** The manifest entry for the archive at `path`, named `file`. */
function rehearsed(file: string, path: string): Rehearsed {
  const bytes = readFileSync(path)
  return { file, path, artifact: { file, sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.byteLength } }
}

/** Archive the built binary in the release layout, as `pack-rust.ts` lays it out. */
function packRehearsal(directory: string, build: ReturnType<typeof workspace>): Rehearsed {
  const stage = join(directory, 'stage')
  const binary = strippedBinary(directory, build.binary)
  const files = payload({ binary, root: ROOT, notices: build.notices }, build.target, {
    text: `Rehearsal placeholder for ${build.target}; the release bundle comes from gen-native-licenses.\n`,
    inventory: '{"schema":"bake-native-licenses/1","note":"rehearsal placeholder"}\n',
    notices: {},
  })
  for (const file of files) {
    const destination = join(stage, file.path)
    mkdirSync(dirname(destination), { recursive: true })
    if (file.contents === undefined) {
      if (!lstatSync(file.source).isFile()) throw new Error(`${file.source} is not a regular file`)
      copyFileSync(file.source, destination)
    } else {
      writeFileSync(destination, file.contents)
    }
    chmodSync(destination, file.mode)
  }
  const file = archiveName(build.version, build.target)
  const archive = join(directory, file)
  const packed = spawnSync('tar', ['-czf', archive, '-C', stage, ...new Set(files.map(one => one.path.split('/')[0] ?? ''))], {
    encoding: 'utf8',
  })
  if (packed.status !== 0) throw new Error(`tar exited ${String(packed.status)}: ${packed.stderr}`)
  return rehearsed(file, archive)
}

/** A release archive `release:pack:rust` made, which must be this checkout's archive for this host. */
function packedArchive(path: string, build: ReturnType<typeof workspace>): Rehearsed {
  const file = archiveName(build.version, build.target)
  if (basename(path) !== file) throw new Error(`BAKE_REHEARSAL_ARCHIVE names ${basename(path)}, not ${file}, this checkout's archive for this host`)
  return rehearsed(file, path)
}

/** What one run of an installed command reported. */
interface CommandRun {
  readonly status: number
  readonly stdout: string
  readonly stderr: string
}

/** Run an installed command, which reaches no loopback host of this process. */
function run(command: string, args: readonly string[], env: Record<string, string> = {}): CommandRun {
  const result = spawnSync(command, [...args], {
    encoding: 'utf8', timeout: 120_000, env: { PATH: process.env['PATH'] ?? '', ...env },
  })
  return { status: result.status ?? -1, stdout: result.stdout ?? '', stderr: result.stderr ?? '' }
}

test.skipIf(process.platform === 'win32')('installs the real bake this checkout builds, then keeps it as the release a rollback returns to', async () => {
  const build = workspace()
  const archive = process.env['BAKE_REHEARSAL_ARCHIVE']
  if (archive === undefined && !existsSync(build.binary)) {
    throw new Error(`${build.binary} is missing; build it first: cd rust && cargo build --locked -p bake-cli --bin bake`)
  }
  const directory = mkdtempSync(join(tmpdir(), 'bake-native-rehearsal-'))
  roots.push(directory)
  const real = archive === undefined ? packRehearsal(directory, build) : packedArchive(resolve(archive), build)
  const key = ephemeralKey()
  const home = join(directory, 'home')
  const nativeRoot = join(home, '.local/share/bake-native')
  const binDir = join(home, '.local/bin')
  mkdirSync(binDir, { recursive: true })
  mkdirSync(join(directory, 'tmp'), { recursive: true })

  let files: HostFiles = { manifest: Buffer.alloc(0), signature: '', releases: new Map() }
  const host = startReleaseHost(() => files)
  hosts.push(host)
  const publish = (version: string, artifact: NativeArtifact, bytes: Buffer): void => {
    const manifest = manifestBytes({ version, artifacts: { [build.target]: artifact } })
    files = {
      manifest,
      signature: signManifest(manifest, key),
      releases: new Map([[`/native/releases/${version}/${artifact.file}`, bytes]]),
    }
  }
  publish(build.version, real.artifact, readFileSync(real.path))
  const installer = installerTrusting('install.sh', key.publicKey, directory)
  const env = {
    HOME: home,
    TMPDIR: join(directory, 'tmp'),
    BAKE_DOWNLOAD_URL: host.origin,
    BAKE_NATIVE_ROOT: nativeRoot,
    BAKE_BIN_DIR: binDir,
  }

  const installed = await runInstaller(['sh', installer], { PATH: process.env['PATH'] ?? '', ...env }, 300_000)
  expect(installed.stderr).toBe('')
  expect(installed.status).toBe(0)
  expect(readlinkSync(join(nativeRoot, 'current'))).toBe(`releases/${build.version}`)

  // The installed command is the real binary, through the link the installer made.
  const command = join(binDir, 'bake')
  const version = run(command, ['--version'], { HOME: home, BAKE_HOME: join(home, '.bake') })
  expect(version.status).toBe(0)
  expect(version.stdout).toContain(build.version)
  const selfCheck = run(command, ['--self-check'], { HOME: home, BAKE_HOME: join(home, '.bake') })
  expect(selfCheck.status).toBe(0)
  const record = JSON.parse(selfCheck.stdout) as { kind: string; version: string; target: string; runtime: string }
  expect(record.kind).toBe('bake-self-check')
  expect(record.runtime).toBe('rust')
  expect(record.version).toBe(build.version)
  expect(record.target).toBe(build.target)

  // A second, fixture version takes over, leaving the real release as the one
  // `previous` names; the Rust updater's `--rollback` is what returns to it.
  const nextVersion = build.version.replace(/^(\d+)\.(\d+)\.(\d+)(.*)$/u,
    (_, major: string, minor: string, patch: string, rest: string) => `${major}.${minor}.${String(Number(patch) + 1)}${rest}`)
  const next = stubRelease(directory, nextVersion, build.target)
  publish(nextVersion, next.artifact, next.bytes)
  const updated = await runInstaller(['sh', installer], { PATH: process.env['PATH'] ?? '', ...env }, 300_000)
  expect(updated.stderr).toBe('')
  expect(updated.status).toBe(0)
  expect(readlinkSync(join(nativeRoot, 'current'))).toBe(`releases/${nextVersion}`)
  expect(readlinkSync(join(nativeRoot, 'previous'))).toBe(`releases/${build.version}`)

  // The release a rollback returns to is intact and still passes its own self-check.
  const kept = join(nativeRoot, 'releases', build.version, 'bin/bake')
  const keptCheck = run(kept, ['--self-check'], { HOME: home, BAKE_HOME: join(home, '.bake') })
  expect(keptCheck.status).toBe(0)
  expect((JSON.parse(keptCheck.stdout) as { version: string }).version).toBe(build.version)
  console.info(`Native rehearsal: installed ${build.version} (${build.target}) from ${real.file}, then the fixture ${nextVersion}`)
}, 600_000)
