#!/usr/bin/env bun
/**
 * Build `bake-rs` with the pinned Cargo toolchain and archive it for this
 * host: a preview of the native 0.4 package layout, not a release.
 *
 * The archive holds the binary, the repository license and notices, and the
 * NOTICE of each Bake crate linked into the binary, and nothing else: no
 * Node, Bun, TypeScript product code, `node_modules`, or launcher. Bun runs
 * this build tool only. The staged binary's `--self-check` and `--version`
 * must name the Cargo version and this host's platform, and its embedded
 * documentation must match the sources, before and after the archive is
 * unpacked; only then does the archive replace the destination, by rename,
 * so a failed run leaves an earlier archive as it was. The output lives
 * under `.artifacts/bake-rs-release/`, apart from the TypeScript release's
 * `.artifacts/bake-release/` and `distribution/host/public/`, and nothing
 * here signs, publishes, installs, or links a command.
 *
 * See distribution/README.md, "Native preview archive".
 */

import { createHash, randomUUID } from 'node:crypto'
import {
  chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { parseArgs } from 'node:util'
import { tarEntries, SHARED_WRITE } from './permissions.ts'

/** The five release platforms, as Node names them; `bake-rs --self-check` uses the same names. */
export const RUST_TARGETS = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64'] as const
export type RustTarget = typeof RUST_TARGETS[number]

/** The record `bake-rs --self-check` prints, schema 1 (`rust/crates/bake-cli/src/self_check.rs`). */
export interface SelfCheckRecord {
  readonly kind: 'bake-rs-self-check'
  readonly schema: 1
  readonly version: string
  readonly runtime: 'rust'
  readonly target: RustTarget
  /** SHA-256 of each embedded documentation topic, by topic name. */
  readonly docs: Readonly<Record<string, string>>
}

/** What a staged binary must report. */
export interface Expected {
  readonly version: string
  readonly target: RustTarget
  readonly docs: Readonly<Record<string, string>>
}

/** One file of the archive: where it comes from, where it goes, and its mode. */
export interface PayloadFile {
  readonly source: string
  /** `/`-separated path inside the archive. */
  readonly path: string
  readonly mode: number
}

export interface PackOptions {
  /** The built `bake-rs` executable. */
  readonly binary: string
  /** The repository root, for `LICENSE` and `THIRD_PARTY_NOTICES.md`. */
  readonly root: string
  /** The `docs/` directory the binary's documentation was compiled from. */
  readonly docs: string
  /** NOTICE files by crate name, each archived as `notices/<crate>/NOTICE`. */
  readonly notices: Readonly<Record<string, string>>
  readonly version: string
  readonly target: string
  /** The directory the archive is written into, created when missing; {@link checkOutputDir} must accept it. */
  readonly outputDir: string
  /** The bound on each run of the binary; the default is 30 s. */
  readonly timeoutMs?: number
}

export interface PackResult {
  readonly archive: string
  readonly record: SelfCheckRecord
  /** Every archive entry as `<octal mode> <path>`, directories ending in `/`, in archive order. */
  readonly listing: readonly string[]
}

const RECORD_KEYS = ['docs', 'kind', 'runtime', 'schema', 'target', 'version']
/** Each run's stdout and stderr are cut here; a self-check record is under 1 KiB. */
const OUTPUT_LIMIT = 1 << 20
const BINARY_MODE = 0o755
const FILE_MODE = 0o644
const DIRECTORY_MODE = 0o755

/** A Cargo version usable in a file name: SemVer with an optional pre-release, no build metadata. */
export function isNativeVersion(value: unknown): value is string {
  return typeof value === 'string' && /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u.test(value)
}

export function isRustTarget(value: unknown): value is RustTarget {
  return typeof value === 'string' && (RUST_TARGETS as readonly string[]).includes(value)
}

/** This host's platform name, whether or not Bake releases for it. */
export function hostTarget(): string {
  return `${process.platform}-${process.arch}`
}

/** The preview archive's name; the `bake-rs-` prefix keeps it apart from the TypeScript `bake-v…` archives. */
export function archiveName(version: string, target: RustTarget): string {
  return `bake-rs-v${version}-${target}.tar.gz`
}

function sha256(bytes: Uint8Array | string): string {
  return createHash('sha256').update(bytes).digest('hex')
}

/**
 * The SHA-256 of every `*.md` file in `dir`, by file name without `.md`,
 * which is how `bake-rs --self-check` names its topics.
 */
export function docsDigests(dir: string): Record<string, string> {
  const digests: Record<string, string> = {}
  for (const name of readdirSync(dir).sort()) {
    if (!name.endsWith('.md')) continue
    const path = join(dir, name)
    if (!lstatSync(path).isFile()) throw new Error(`${path} is not a regular file`)
    digests[name.slice(0, -3)] = sha256(readFileSync(path))
  }
  if (Object.keys(digests).length === 0) throw new Error(`${dir} holds no documentation`)
  return digests
}

/**
 * Parse and check `bake-rs --self-check`'s standard output against what this
 * build must report.
 * @throws naming the first difference: not exactly one JSON line, another
 * kind, schema, or member set, or a version, target, or documentation hash
 * other than expected.
 */
export function parseSelfCheck(stdout: string, expected: Expected): SelfCheckRecord {
  if (!stdout.endsWith('\n') || stdout.indexOf('\n') !== stdout.length - 1) throw new Error(`The self-check printed ${JSON.stringify(stdout.slice(0, 200))}, not one line`)
  let value: unknown
  try {
    value = JSON.parse(stdout)
  } catch {
    throw new Error(`The self-check printed ${JSON.stringify(stdout.slice(0, 200))}, which is not JSON`)
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error('The self-check record is not a JSON object')
  const record = value as Record<string, unknown>
  const keys = Object.keys(record).sort()
  if (keys.join() !== RECORD_KEYS.join()) throw new Error(`The self-check record has members ${keys.join(', ')}, not ${RECORD_KEYS.join(', ')}`)
  if (record.kind !== 'bake-rs-self-check') throw new Error(`The self-check record's kind is ${JSON.stringify(record.kind)}`)
  if (record.schema !== 1) throw new Error(`The self-check record's schema is ${JSON.stringify(record.schema)}; this packager reads schema 1`)
  if (record.runtime !== 'rust') throw new Error(`The self-check record's runtime is ${JSON.stringify(record.runtime)}`)
  if (record.version !== expected.version) throw new Error(`The binary reports version ${JSON.stringify(record.version)}; Cargo builds ${expected.version}`)
  if (record.target !== expected.target) throw new Error(`The binary reports platform ${JSON.stringify(record.target)}; this build is for ${expected.target}`)
  const docs = record.docs
  if (typeof docs !== 'object' || docs === null || Array.isArray(docs)) throw new Error('The self-check record\'s docs is not an object')
  const reported = docs as Record<string, unknown>
  for (const name of new Set([...Object.keys(reported), ...Object.keys(expected.docs)])) {
    if (!(name in expected.docs)) throw new Error(`The binary embeds documentation topic '${name}', which the sources lack`)
    if (!(name in reported)) throw new Error(`The binary lacks documentation topic '${name}'`)
    if (reported[name] !== expected.docs[name]) throw new Error(`The binary's documentation topic '${name}' differs from its source`)
  }
  return record as unknown as SelfCheckRecord
}

/** An empty home, an empty PATH, and nothing else a Windows process cannot start without. */
function isolatedEnv(home: string, path: string): Record<string, string> {
  const env: Record<string, string> = {
    PATH: path, HOME: home, USERPROFILE: home, BAKE_HOME: join(home, '.bake'), DSH_HOME: join(home, '.dsh'),
  }
  if (process.platform === 'win32') {
    for (const name of ['SystemRoot', 'windir']) {
      const value = process.env[name]
      if (value !== undefined) env[name] = value
    }
  }
  return env
}

/** A read cancelled because the run was halted; the halt's cause is the error to report. */
class StoppedRead extends Error {
  constructor() { super('The output read was stopped') }
}

/**
 * Read `stream` to its end, at most {@link OUTPUT_LIMIT} bytes. `stop`
 * cancels the read, so a pipe a descendant still holds cannot keep it open.
 * @throws when the limit is passed (after calling `overflow`) or the read was stopped.
 */
async function readLimited(stream: ReadableStream<Uint8Array>, stop: AbortSignal, overflow: () => void): Promise<string> {
  const reader = stream.getReader()
  // Kept so the read does not settle before the pipe's cancellation has.
  let cancelled: Promise<void> | undefined
  const cancel = (): void => { cancelled = reader.cancel().catch(() => {}) }
  stop.addEventListener('abort', cancel, { once: true })
  try {
    const chunks: Uint8Array[] = []
    let size = 0
    for (;;) {
      if (stop.aborted) throw new StoppedRead()
      const { done, value } = await reader.read()
      if (done) break
      if (size + value.byteLength > OUTPUT_LIMIT) {
        overflow()
        throw new Error(`The binary wrote more than ${OUTPUT_LIMIT} bytes`)
      }
      chunks.push(value)
      size += value.byteLength
    }
    // A cancelled read ends like a finished one; output cut short is not output.
    if (stop.aborted) throw new StoppedRead()
    return Buffer.concat(chunks).toString('utf8')
  } finally {
    stop.removeEventListener('abort', cancel)
    await cancelled
    reader.releaseLock()
  }
}

interface Ran { readonly code: number; readonly stdout: string; readonly stderr: string }

/**
 * Run `binary` with `args` in a private empty home and PATH, then require
 * that the home and PATH are still empty.
 *
 * At the deadline, or when one stream passes the output limit, the child is
 * killed and both output reads are cancelled. Both reads and the child's
 * exit are awaited before the temporary directory is removed or an error is
 * reported. Only the immediate child is killed: a descendant that escapes
 * keeps running, though it can no longer hold this run open through the
 * pipes. `bake-rs --version` and `--self-check` start no process.
 */
async function runIsolated(binary: string, args: readonly string[], timeoutMs: number): Promise<Ran> {
  const command = `${basename(binary)} ${args.join(' ')}`
  const scratch = mkdtempSync(join(tmpdir(), 'bake-rs-pack-run-'))
  try {
    const home = join(scratch, 'home')
    const path = join(scratch, 'path')
    mkdirSync(home)
    mkdirSync(path)
    const child = Bun.spawn([binary, ...args], { cwd: scratch, env: isolatedEnv(home, path), stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' })
    const stop = new AbortController()
    let timedOut = false
    const halt = (): void => {
      child.kill('SIGKILL')
      stop.abort()
    }
    // Not Bun's `timeout`: that kills the child but leaves a read open while a descendant holds the pipe.
    const deadline = setTimeout(() => {
      timedOut = true
      halt()
    }, timeoutMs)
    let settled: PromiseSettledResult<string>[]
    let code: number
    try {
      settled = await Promise.allSettled([readLimited(child.stdout, stop.signal, halt), readLimited(child.stderr, stop.signal, halt)])
      code = await child.exited
    } finally {
      clearTimeout(deadline)
    }
    if (timedOut) throw new Error(`${command} did not finish within ${timeoutMs} ms`)
    const failures = settled.flatMap(result => result.status === 'rejected' ? [result.reason as unknown] : [])
    const failed = failures.find(reason => !(reason instanceof StoppedRead)) ?? failures[0]
    if (failed !== undefined) throw failed
    if (child.signalCode !== null) throw new Error(`${command} was stopped by ${child.signalCode}`)
    const written = [...readdirSync(home), ...readdirSync(path)]
    if (written.length > 0) throw new Error(`${command} wrote ${written.join(', ')} into its empty home or PATH`)
    const [stdout, stderr] = settled.map(result => (result as PromiseFulfilledResult<string>).value) as [string, string]
    return { code, stdout, stderr }
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
}

/**
 * Check a binary as a release would run it: `--version` and `--self-check`,
 * without Node, Bun, or a Bake home.
 */
export async function checkBinary(binary: string, expected: Expected, timeoutMs = 30_000): Promise<SelfCheckRecord> {
  const version = await runIsolated(binary, ['--version'], timeoutMs)
  if (version.code !== 0 || version.stdout !== `bake-rs ${expected.version}\n`) {
    throw new Error(`bake-rs --version exited ${version.code} with ${JSON.stringify(version.stdout.slice(0, 200))}; expected "bake-rs ${expected.version}"`)
  }
  const check = await runIsolated(binary, ['--self-check'], timeoutMs)
  if (check.code !== 0) throw new Error(`bake-rs --self-check exited ${check.code}: ${check.stderr.trim().slice(0, 500)}`)
  return parseSelfCheck(check.stdout, expected)
}

/** The archive's files for `options`, in a fixed order. */
export function payload(options: Pick<PackOptions, 'binary' | 'root' | 'notices'>, target: RustTarget): PayloadFile[] {
  const files: PayloadFile[] = [
    { source: options.binary, path: target === 'win32-x64' ? 'bin/bake-rs.exe' : 'bin/bake-rs', mode: BINARY_MODE },
    { source: join(options.root, 'LICENSE'), path: 'LICENSE', mode: FILE_MODE },
    { source: join(options.root, 'THIRD_PARTY_NOTICES.md'), path: 'THIRD_PARTY_NOTICES.md', mode: FILE_MODE },
  ]
  for (const crate of Object.keys(options.notices).sort()) {
    if (!/^[a-z][a-z0-9-]*$/u.test(crate)) throw new Error(`Crate name ${JSON.stringify(crate)} cannot name a notice directory`)
    files.push({ source: options.notices[crate] as string, path: `notices/${crate}/NOTICE`, mode: FILE_MODE })
  }
  return files
}

/** Every directory the payload's paths need, parents first. */
function directories(files: readonly PayloadFile[]): string[] {
  const dirs = new Set<string>()
  for (const file of files) {
    const parts = file.path.split('/').slice(0, -1)
    for (let index = 1; index <= parts.length; index++) dirs.add(parts.slice(0, index).join('/'))
  }
  return [...dirs].sort()
}

/** Copy the payload into `stage`, refusing any source that is not a regular file. */
function stagePayload(files: readonly PayloadFile[], stage: string): void {
  for (const dir of directories(files)) {
    mkdirSync(join(stage, dir))
    chmodSync(join(stage, dir), DIRECTORY_MODE)
  }
  for (const file of files) {
    // A link could carry a file from anywhere into the archive; lstat does not follow it.
    const stats = lstatSync(file.source)
    if (!stats.isFile()) throw new Error(`${file.source} is ${stats.isSymbolicLink() ? 'a symbolic link' : 'not a regular file'}; the archive takes regular files only`)
    if (stats.size === 0) throw new Error(`${file.source} is empty`)
    const destination = join(stage, file.path)
    copyFileSync(file.source, destination)
    chmodSync(destination, file.mode)
  }
}

/**
 * Read `archive` back and check it holds exactly `files` and their
 * directories, as plain files and directories, with the binary executable
 * and nothing writable by group or others. Windows `tar.exe` invents modes,
 * so modes are not checked there.
 * @returns the listing, `<octal mode> <path>` per entry.
 * @throws on a truncated or corrupt archive, a link, a duplicate, or an entry missing or unexpected.
 */
export async function verifyArchive(
  archive: string, files: readonly PayloadFile[], platform: NodeJS.Platform = process.platform,
): Promise<string[]> {
  const expected = new Map<string, number | 'dir'>()
  for (const dir of directories(files)) expected.set(dir, 'dir')
  for (const file of files) expected.set(file.path, file.mode)
  const seen = new Set<string>()
  const listing: string[] = []
  const stream = Bun.file(archive).stream().pipeThrough(new DecompressionStream('gzip'))
  try {
    for await (const entry of tarEntries(stream)) {
      const path = entry.path.replace(/^\.\//u, '').replace(/\/$/u, '')
      if (path === '' || path === '.') continue
      if (seen.has(path)) throw new Error(`The archive holds ${path} twice`)
      seen.add(path)
      const want = expected.get(path)
      if (want === undefined) throw new Error(`The archive holds ${path}, which is not part of the package`)
      if (want === 'dir' ? entry.type !== '5' : entry.type !== '0') throw new Error(`The archive's ${path} has tar type ${JSON.stringify(entry.type)}; a ${want === 'dir' ? 'directory' : 'regular file'} was staged`)
      if (platform !== 'win32') {
        if ((entry.mode & SHARED_WRITE) !== 0) throw new Error(`The archive's ${path} is writable by group or others (${entry.mode.toString(8)})`)
        if (want === BINARY_MODE && (entry.mode & 0o111) !== 0o111) throw new Error(`The archive's ${path} is not executable (${entry.mode.toString(8)})`)
      }
      listing.push(`${entry.mode.toString(8).padStart(4, '0')} ${path}${want === 'dir' ? '/' : ''}`)
    }
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('The archive')) throw error
    throw new Error(`${archive} is not a readable gzipped tar archive: ${error instanceof Error ? error.message : String(error)}`)
  }
  const missing = [...expected.keys()].filter(path => !seen.has(path))
  if (missing.length > 0) throw new Error(`The archive lacks ${missing.join(', ')}`)
  return listing
}

async function run(argv: readonly string[], cwd: string): Promise<void> {
  const child = Bun.spawn([...argv], { cwd, stdin: 'ignore', stdout: 'inherit', stderr: 'inherit' })
  const code = await child.exited
  if (code !== 0) throw new Error(`${argv.join(' ')} exited ${code}`)
}

/**
 * Stage, check, archive, unpack, and check again; then move the archive into
 * `outputDir` as {@link archiveName}. Every temporary path is private to the
 * call and removed on return, and the archive is renamed into place only
 * after both checks pass, so an earlier archive of the same name survives
 * any failure.
 * @throws when the target is not this host's release platform, the version
 * is not a native version, a payload file is missing, empty, or a link, or
 * a check fails.
 */
export async function packRust(options: PackOptions): Promise<PackResult> {
  const { target, version } = options
  if (!isRustTarget(target)) throw new Error(`${target} is not a Bake release platform (${RUST_TARGETS.join(', ')})`)
  if (target !== hostTarget()) throw new Error(`This host is ${hostTarget()}; a ${target} archive must be built and checked on ${target}`)
  if (!isNativeVersion(version)) throw new Error(`${JSON.stringify(version)} is not a native release version`)
  const timeoutMs = options.timeoutMs ?? 30_000
  const expected: Expected = { version, target, docs: docsDigests(options.docs) }
  const files = payload(options, target)
  const binaryPath = files[0]?.path as string

  const name = archiveName(version, target)
  const outputDir = checkOutputDir(options.root, options.outputDir)
  const destination = join(outputDir, name)
  // Windows's TEMP can be an 8.3 short path; resolve it as pack.ts does.
  const scratch = realpathSync.native(mkdtempSync(join(tmpdir(), 'bake-rs-pack-')))
  // Beside the destination, so the final rename stays on one file system.
  const partial = join(outputDir, `.${name}.${process.pid}-${randomUUID()}.partial`)
  try {
    mkdirSync(outputDir, { recursive: true })
    // A link swapped in while the directories were made would move the output elsewhere.
    if (checkOutputDir(options.root, outputDir) !== outputDir) throw new Error(`${outputDir} changed while it was created`)
    const stage = join(scratch, 'stage')
    mkdirSync(stage)
    stagePayload(files, stage)
    const record = await checkBinary(join(stage, binaryPath), expected, timeoutMs)

    // System tar, as pack.ts uses: it records the staged modes. Only the
    // payload's top-level names are given, so nothing else in the stage can enter.
    const top = [...new Set(files.map(file => file.path.split('/')[0] as string))].sort()
    await run(['tar', '-czf', partial, '-C', stage, ...top], scratch)
    const listing = await verifyArchive(partial, files)

    const unpacked = join(scratch, 'unpacked')
    mkdirSync(unpacked)
    await run(['tar', '-xzf', partial, '-C', unpacked], scratch)
    for (const file of files) {
      const path = join(unpacked, ...file.path.split('/'))
      if (!lstatSync(path).isFile() || sha256(readFileSync(path)) !== sha256(readFileSync(join(stage, file.path)))) throw new Error(`The unpacked ${file.path} differs from the staged file`)
    }
    const unpackedRecord = await checkBinary(join(unpacked, ...binaryPath.split('/')), expected, timeoutMs)
    if (JSON.stringify(unpackedRecord) !== JSON.stringify(record)) throw new Error('The unpacked binary\'s self-check differs from the staged binary\'s')

    renameSync(partial, destination)
    return { archive: destination, record, listing }
  } finally {
    rmSync(partial, { force: true })
    rmSync(scratch, { recursive: true, force: true })
  }
}

interface CargoPackage {
  readonly name: string
  readonly version: string
  readonly manifest_path: string
  readonly dependencies: readonly { readonly name: string; readonly kind: string | null; readonly path?: string }[]
}

/** What the packager reads from `cargo metadata`. */
export interface WorkspaceInputs {
  /** `bake-cli`'s Cargo version, which the binary must report. */
  readonly version: string
  /** The `docs/` directory compiled into the binary. */
  readonly docs: string
  /** NOTICE files of the workspace crates `bake-cli` links, by crate name. */
  readonly notices: Record<string, string>
  readonly targetDir: string
}

/**
 * `bake-cli`'s version, its documentation directory, and the NOTICE of each
 * workspace crate it links, from `cargo metadata`.
 */
export function workspaceInputs(metadata: { packages: readonly CargoPackage[]; target_directory: string }): WorkspaceInputs {
  const byName = new Map(metadata.packages.map(pkg => [pkg.name, pkg]))
  const cli = byName.get('bake-cli')
  if (cli === undefined) throw new Error('cargo metadata lists no bake-cli package')
  const linked = new Set<string>()
  const visit = (pkg: CargoPackage): void => {
    if (linked.has(pkg.name)) return
    linked.add(pkg.name)
    // Normal dependencies only: build and dev dependencies are not linked.
    for (const dep of pkg.dependencies) {
      const member = byName.get(dep.name)
      if (dep.kind === null && dep.path !== undefined && member !== undefined) visit(member)
    }
  }
  visit(cli)
  const notices: Record<string, string> = {}
  for (const crate of [...linked].sort()) {
    const notice = join(dirname((byName.get(crate) as CargoPackage).manifest_path), 'NOTICE')
    if (existsSync(notice)) notices[crate] = notice
  }
  return { version: cli.version, docs: join(dirname(cli.manifest_path), 'docs'), notices, targetDir: metadata.target_directory }
}

/**
 * `path` with its deepest existing ancestor resolved through symbolic links
 * and junctions, and the rest, which does not exist yet, appended.
 * @throws when an ancestor is a link whose target does not exist, which a
 * later `mkdir` would follow.
 */
export function canonicalPath(path: string): string {
  const rest: string[] = []
  let current = resolve(path)
  for (;;) {
    try {
      return join(realpathSync.native(current), ...[...rest].reverse())
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
    }
    let dangling = true
    try {
      lstatSync(current)
    } catch {
      dangling = false
    }
    if (dangling) throw new Error(`${current} is a link to a path that does not exist`)
    const parent = dirname(current)
    if (parent === current) return resolve(path)
    rest.push(basename(current))
    current = parent
  }
}

/** Directories of the TypeScript release that a native archive must never enter, under the repository root. */
export const PROTECTED_OUTPUTS = ['.artifacts/bake-release', 'distribution/host/public'] as const

/**
 * The canonical directory a native archive may be written into for
 * `outputDir`: outside the repository, or under its
 * `.artifacts/bake-rs-release/`, and never inside a
 * {@link PROTECTED_OUTPUTS} directory, through whatever links lead there.
 * @throws naming the refused directory.
 */
export function checkOutputDir(root: string, outputDir: string): string {
  const out = canonicalPath(outputDir)
  for (const name of PROTECTED_OUTPUTS) {
    if (inside(canonicalPath(join(root, name)), out)) throw new Error(`--out ${outputDir} resolves to ${out}, inside the TypeScript release's ${name}/`)
  }
  if (inside(canonicalPath(root), out) && !inside(canonicalPath(join(root, '.artifacts/bake-rs-release')), out)) {
    throw new Error(`--out ${outputDir} resolves to ${out}, in the repository but not under .artifacts/bake-rs-release/`)
  }
  return out
}

/** Whether `child` is `parent` or under it. */
export function inside(parent: string, child: string): boolean {
  const rel = relative(resolve(parent), resolve(child))
  return rel === '' || (rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel))
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: { profile: { type: 'string', default: 'release' }, out: { type: 'string' } },
    strict: true, allowPositionals: false,
  })
  const profile = values.profile
  if (profile !== 'release' && profile !== 'dev') throw new Error(`--profile must be release or dev, not ${String(profile)}`)
  const root = resolve(import.meta.dir, '../..')
  const rust = join(root, 'rust')
  const metadata = Bun.spawnSync(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], { cwd: rust, stdout: 'pipe', stderr: 'inherit' })
  if (metadata.exitCode !== 0) throw new Error(`cargo metadata exited ${metadata.exitCode}`)
  const inputs = workspaceInputs(JSON.parse(metadata.stdout.toString()))
  const outputDir = resolve(values.out ?? join(root, '.artifacts/bake-rs-release', inputs.version))
  // Before building: packRust checks again, but a refused directory should cost no build.
  checkOutputDir(root, outputDir)
  // From rust/, so rust-toolchain.toml selects the pinned toolchain.
  await run(['cargo', 'build', '--locked', '-p', 'bake-cli', '--bin', 'bake-rs', ...profile === 'release' ? ['--release'] : []], rust)
  const binary = join(inputs.targetDir, profile === 'release' ? 'release' : 'debug', process.platform === 'win32' ? 'bake-rs.exe' : 'bake-rs')
  const { docs, notices, version } = inputs
  const result = await packRust({ binary, root, docs, notices, version, target: hostTarget(), outputDir })
  console.log(result.listing.join('\n'))
  console.log(JSON.stringify(result.record))
  console.log(result.archive)
}

if (import.meta.main) {
  await main().catch((error: unknown) => {
    console.error(`release:pack:rust: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(1)
  })
}
