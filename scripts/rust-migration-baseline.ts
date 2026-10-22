/**
 * Source provenance for the Rust migration's scope-00 TypeScript baseline.
 *
 * `capture` identifies the exact source of a clean git worktree: its commit
 * and tree, the product version, the Session writer format, the `bun.lock`
 * digest, and content digests of the profiles, presets, catalogs,
 * persistence descriptions, and model-surface snapshots a native port is
 * compared against. `check` re-derives that identity read-only and rejects a
 * dirty worktree, a missing authoritative input, or a record whose digests no
 * longer match. The only inputs a commit may lack are the ones
 * `KNOWN_ABSENCES` names for that exact commit, and the record lists each as
 * missing evidence with its reason. `run` executes one command in a verified worktree and records
 * its observed exit as a separate check result.
 *
 * A provenance record is not qualification evidence. It never says that a
 * build, test, or measurement passed; only a check result produced by `run`
 * records an observed outcome, and the scope's evidence ledger decides what
 * those results qualify. Records hold repository-relative paths, digests, and
 * tool versions only: no environment dump, absolute path, or command output.
 * @module scripts/rust-migration-baseline
 */

import { spawn } from 'node:child_process'
import { closeSync, openSync } from 'node:fs'
import { lstat, mkdir, readFile, readlink, realpath, rename, rm, writeFile } from 'node:fs/promises'
import { homedir, release } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { createHash } from 'node:crypto'
import { withoutRepositoryGitEnv } from './git-env.ts'

/** Commits the roadmap names for the comparison baseline; publication is verified elsewhere. */
export const ROADMAP_BASELINE = {
  develop: { ref: 'origin/develop', commit: 'ae5eb51ab61f2266b0fc2ff52f2f14b9f3a9a917' },
  release: { ref: 'v0.3.8', commit: 'dcb26d756e008a3d17c057a6b153933c018c275d' },
} as const

export const PROVENANCE_SCHEMA = 'bake/rust-migration/source-provenance'
export const CHECK_RESULT_SCHEMA = 'bake/rust-migration/check-result'
export const SCHEMA_VERSION = 1

/** Where `capture` writes and `check` reads by default, below the worktree's ignored `.preflight/`. */
export const DEFAULT_RECORD = '.preflight/rust-migration/source-provenance.json'

const PROVENANCE_STATEMENT =
  'Identifies source inputs only. It is not build, test, or performance evidence, and its existence does not show that any check passed.'

/** One named set of tracked files whose content the baseline pins. */
export interface InputSpec {
  readonly id: string
  readonly description: string
  /** Repository-relative patterns; `*` matches within one segment, `**` matches any segments. */
  readonly patterns: readonly string[]
}

/**
 * The authoritative inputs. Every pattern must match at least one file in the
 * commit's tree, so a renamed or removed input fails capture instead of
 * silently shrinking the baseline. {@link KNOWN_ABSENCES} names the only
 * exceptions.
 */
export const AUTHORITATIVE_INPUTS: readonly InputSpec[] = [
  { id: 'lockfile', description: 'Bun dependency lockfile', patterns: ['bun.lock'] },
  { id: 'product-manifests', description: 'Product version manifests', patterns: ['package.json', 'apps/cli/package.json'] },
  {
    id: 'profiles',
    description: 'Shipped profile compositions',
    patterns: ['packages/bundle/*/cordis.patch.yml', 'apps/tui/packages/app/cordis.patch.yml', 'apps/tui/packages/app/cordis.built.patch.yml'],
  },
  { id: 'presets', description: 'Agent presets', patterns: ['packages/preset/agent-presets/presets/**'] },
  { id: 'config-catalog', description: 'Generated configuration catalog', patterns: ['docs/config-catalog.md'] },
  { id: 'tool-catalog', description: 'Generated tool catalog', patterns: ['docs/tool-catalog.md'] },
  {
    id: 'persistence',
    description: 'Persistence catalog, schema, change records, and Session format catalog',
    patterns: ['docs/persistence-catalog.md', 'docs/persistence-schema.json', 'docs/persistence-changes/**', 'packages/session/session-format-catalog/src/**'],
  },
  { id: 'session-writer', description: 'Session writer format constant', patterns: ['packages/core/session/src/types.ts'] },
  {
    id: 'model-surface',
    description: 'Model-surface snapshots of the shipped profiles and presets',
    patterns: ['apps/cli/tests/profiles/expected/model-surface/*.md', 'apps/tui/packages/app/tests/expected/model-surface/*.md'],
  },
  { id: 'release-targets', description: 'Release manifest and artifact targets', patterns: ['packages/boot/updater/src/manifest.ts'] },
]

/**
 * One input pattern that a specific commit is known to predate. It exempts
 * only that pattern of that input at that exact commit; the record lists it
 * and its reason as missing evidence, and capture fails if the commit does
 * contain matching files, so a stale exemption cannot hide anything.
 */
export interface KnownAbsence {
  readonly commit: string
  readonly input: string
  readonly pattern: string
  readonly reason: string
}

const MODEL_SURFACE_PREDATES =
  'v0.3.8 predates the model-surface snapshots, which eac853e9940b6c2f5c35236e3a003d055eaf310d and e408faf28c71408f0c14dc5246113a3c3a6c9860 added afterwards; no snapshot of the release exists to pin'

/** The only inputs a pinned baseline may lack. Every other missing input fails capture and check. */
export const KNOWN_ABSENCES: readonly KnownAbsence[] = [
  { commit: ROADMAP_BASELINE.release.commit, input: 'model-surface', pattern: 'apps/cli/tests/profiles/expected/model-surface/*.md', reason: MODEL_SURFACE_PREDATES },
  { commit: ROADMAP_BASELINE.release.commit, input: 'model-surface', pattern: 'apps/tui/packages/app/tests/expected/model-surface/*.md', reason: MODEL_SURFACE_PREDATES },
]

const LOCKFILE = 'bun.lock'
const PRODUCT_MANIFESTS = ['package.json', 'apps/cli/package.json'] as const
const SESSION_WRITER = 'packages/core/session/src/types.ts'
const SESSION_WRITER_PATTERN = /^export const SESSION_FORMAT_VERSION = (\d+)$/mu

export interface InputFile {
  readonly path: string
  readonly mode: string
  /** Git blob id at the commit. */
  readonly blob: string
  readonly sha256: string
  readonly bytes: number
}

export interface InputGroup {
  readonly id: string
  readonly description: string
  readonly patterns: readonly string[]
  /** SHA-256 over each file's `path NUL sha256 LF`, in path order. */
  readonly digest: string
  readonly files: readonly InputFile[]
}

/** The deterministic part of a record: equal for any capture of the same commit. */
export interface SourceSection {
  readonly commit: string
  readonly tree: string
  readonly product: { readonly version: string; readonly manifests: Readonly<Record<string, string>> }
  readonly sessionFormatVersion: number
  readonly lockfile: { readonly path: string; readonly sha256: string }
  readonly inputs: readonly InputGroup[]
  /** Input patterns this commit is known to lack, from {@link KNOWN_ABSENCES}; usually empty. */
  readonly absent: readonly { readonly input: string; readonly pattern: string; readonly reason: string }[]
}

/** The capturing host. Informational: `check` does not require the same machine. */
export interface EnvironmentSection {
  readonly platform: string
  readonly arch: string
  readonly osRelease: string
  /** First line of each tool's `--version`, or null when it could not run. */
  readonly tools: { readonly bun: string | null; readonly node: string | null; readonly git: string | null }
}

export interface ProvenanceRecord {
  readonly schema: typeof PROVENANCE_SCHEMA
  readonly schemaVersion: typeof SCHEMA_VERSION
  readonly scope: '00'
  readonly kind: 'source-provenance'
  readonly qualification: 'not-evaluated'
  readonly statement: string
  /** SHA-256 of the canonical JSON of {@link ProvenanceRecord.source}. */
  readonly sourceDigest: string
  readonly source: SourceSection
  readonly environment: EnvironmentSection
  readonly evidence: { readonly build: 'not-recorded'; readonly tests: 'not-recorded'; readonly performance: 'not-recorded' }
  readonly missingEvidence: readonly string[]
}

export type CheckOutcome = 'passed' | 'failed' | 'invalid'

/** One observed command execution, kept apart from the provenance it ran against. */
export interface CheckResult {
  readonly schema: typeof CHECK_RESULT_SCHEMA
  readonly schemaVersion: typeof SCHEMA_VERSION
  readonly scope: '00'
  readonly kind: 'check-result'
  readonly id: string
  /** The command with the worktree path shown as `<root>` and the home directory as `~`. */
  readonly argv: readonly string[]
  readonly cwd: '.'
  readonly source: { readonly commit: string; readonly sourceDigest: string }
  readonly environment: EnvironmentSection
  readonly startedAt: string
  readonly durationMs: number
  readonly timeoutMs: number
  readonly exitCode: number | null
  readonly signal: string | null
  readonly timedOut: boolean
  readonly interrupted: boolean
  /** Whether processes of the command's group outlived it and had to be killed. */
  readonly descendantsRemained: boolean
  /** Whether the worktree was still clean at the same source afterwards. */
  readonly sourceUnchanged: boolean
  readonly log: { readonly file: string; readonly sha256: string; readonly bytes: number }
  /** `passed` only for exit 0 with no signal, timeout, interruption, leftover process, or source change. */
  readonly outcome: CheckOutcome
}

/** A refusal with every problem found, so one run reports them all. */
export class BaselineError extends Error {
  constructor(readonly problems: readonly string[]) {
    super(problems.join('\n'))
    this.name = 'BaselineError'
  }
}

// ---------------------------------------------------------------------------
// Processes

interface Executed {
  readonly code: number | null
  readonly signal: string | null
  readonly stdout: Buffer
  readonly stderr: Buffer
  readonly timedOut: boolean
}

/** Run argv without a shell, awaiting the child's close even when it times out. */
function execute(
  argv: readonly string[],
  options: { cwd: string; env: NodeJS.ProcessEnv; input?: string; timeoutMs: number },
): Promise<Executed> {
  return new Promise((done, fail) => {
    const [command, ...args] = argv
    if (command === undefined) return fail(new Error('empty command'))
    const child = spawn(command, args, { cwd: options.cwd, env: options.env, stdio: ['pipe', 'pipe', 'pipe'], shell: false })
    const stdout: Buffer[] = []
    const stderr: Buffer[] = []
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      child.kill('SIGKILL')
    }, options.timeoutMs)
    child.stdout.on('data', (chunk: Buffer) => stdout.push(chunk))
    child.stderr.on('data', (chunk: Buffer) => stderr.push(chunk))
    child.on('error', (error) => {
      clearTimeout(timer)
      fail(error)
    })
    child.on('close', (code, signal) => {
      clearTimeout(timer)
      done({ code, signal, stdout: Buffer.concat(stdout), stderr: Buffer.concat(stderr), timedOut })
    })
    child.stdin.on('error', () => {})
    child.stdin.end(options.input ?? '')
  })
}

/** Git with the hook-bound repository variables removed and no optional index writes. */
function gitEnv(): NodeJS.ProcessEnv {
  return { ...withoutRepositoryGitEnv(process.env), GIT_OPTIONAL_LOCKS: '0', LC_ALL: 'C' }
}

async function git(root: string, args: readonly string[], input?: string): Promise<string> {
  const result = await execute(['git', '-C', root, ...args], { cwd: root, env: gitEnv(), input, timeoutMs: 120_000 })
  if (result.code !== 0) throw new BaselineError([`git ${args.join(' ')} failed: ${result.stderr.toString('utf8').trim() || `exit ${String(result.code)}`}`])
  return result.stdout.toString('utf8')
}

async function gitStatus(root: string, args: readonly string[]): Promise<number | null> {
  return (await execute(['git', '-C', root, ...args], { cwd: root, env: gitEnv(), timeoutMs: 120_000 })).code
}

// ---------------------------------------------------------------------------
// Paths and digests

function sha256(data: string | Uint8Array): string {
  return createHash('sha256').update(data).digest('hex')
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map(key => [key, sortKeys((value as Record<string, unknown>)[key])]))
  }
  return value
}

/** The digest of a source section, independent of key order. */
export function sourceDigest(source: SourceSection): string {
  return sha256(JSON.stringify(sortKeys(source)))
}

function groupDigest(files: readonly InputFile[]): string {
  return sha256(files.map(file => `${file.path}\0${file.sha256}\n`).join(''))
}

function segmentMatches(pattern: string, name: string): boolean {
  const expression = pattern.split('*').map(part => part.replace(/[.+?^${}()|[\]\\]/gu, '\\$&')).join('[^/]*')
  return new RegExp(`^${expression}$`, 'u').test(name)
}

/**
 * Whether a repository-relative path matches an input pattern.
 * @param pattern - `/`-separated; `*` stays within a segment, `**` spans zero or more segments.
 * @param path - `/`-separated repository path.
 */
export function matchesPattern(pattern: string, path: string): boolean {
  const match = (p: readonly string[], s: readonly string[]): boolean => {
    if (p.length === 0) return s.length === 0
    const [head, ...rest] = p
    if (head === '**') return rest.length === 0 ? s.length > 0 : s.some((_, index) => match(rest, s.slice(index))) || match(rest, [])
    const [name, ...names] = s
    return head !== undefined && name !== undefined && segmentMatches(head, name) && match(rest, names)
  }
  return match(pattern.split('/'), path.split('/'))
}

/** Show the worktree and home directory symbolically so committed output names no user path. */
function redactor(roots: readonly string[]): (text: string) => string {
  const home = homedir()
  const replacements: (readonly [string, string])[] = [...new Set(roots)].filter(Boolean).sort((a, b) => b.length - a.length).map(root => [root, '<root>'] as const)
  if (home.length > 1) replacements.push([home, '~'])
  return text => replacements.reduce((current, [from, to]) => current.split(from).join(to), text)
}

// ---------------------------------------------------------------------------
// Worktree inspection

interface TreeEntry {
  readonly mode: string
  readonly type: string
  readonly blob: string
}

/** Resolve and validate the worktree root: it must be the top level of a git worktree with a commit. */
async function worktreeRoot(root: string): Promise<string> {
  const canonical = await realpath(resolve(root)).catch(() => {
    throw new BaselineError([`worktree root does not exist: ${root}`])
  })
  const top = (await git(canonical, ['rev-parse', '--show-toplevel'])).trim()
  if ((await realpath(top)) !== canonical) throw new BaselineError(['--root must be the top level of a git worktree; this one\'s top level is elsewhere'])
  return canonical
}

/** Every reason the worktree does not exactly equal its commit. Ignored paths are not reasons. */
async function cleanlinessProblems(root: string): Promise<string[]> {
  const problems: string[] = []
  const status = await git(root, ['status', '--porcelain=v1', '-z', '--untracked-files=all', '--ignore-submodules=none'])
  const fields = status.split('\0')
  for (let index = 0; index < fields.length; index++) {
    const entry = fields[index]
    if (entry === undefined || entry === '') continue
    problems.push(`worktree is dirty: ${entry.slice(0, 2).trim()} ${entry.slice(3)}`)
    // Rename and copy entries carry their source path as the next field; reporting the target is enough.
    if (entry[0] === 'R' || entry[0] === 'C') index++
  }
  // Hidden index flags would let a modified file look clean to status.
  const flagged = await git(root, ['ls-files', '-v', '-z'])
  for (const entry of flagged.split('\0').filter(Boolean)) {
    const tag = entry.charAt(0)
    const hidden = tag === 'S' ? 'skip-worktree' : tag >= 'a' && tag <= 'z' ? 'assume-unchanged' : undefined
    if (hidden !== undefined) problems.push(`index hides changes with ${hidden}: ${entry.slice(2)}`)
  }
  return problems
}

async function headTree(root: string): Promise<{ commit: string; tree: string; entries: Map<string, TreeEntry> }> {
  const commit = (await git(root, ['rev-parse', '--verify', 'HEAD^{commit}'])).trim()
  const tree = (await git(root, ['rev-parse', '--verify', `${commit}^{tree}`])).trim()
  const entries = new Map<string, TreeEntry>()
  for (const line of (await git(root, ['ls-tree', '-r', '-z', '--full-tree', commit])).split('\0').filter(Boolean)) {
    const tab = line.indexOf('\t')
    const [mode, type, blob] = line.slice(0, tab).split(' ')
    if (tab < 0 || mode === undefined || type === undefined || blob === undefined) throw new BaselineError([`git ls-tree printed an unreadable entry: ${line}`])
    entries.set(line.slice(tab + 1), { mode, type, blob })
  }
  return { commit, tree, entries }
}

/**
 * Read blobs from the object store, so digests describe the commit itself
 * rather than a checkout that local filters or line-ending settings rewrote.
 */
async function readBlobs(root: string, blobs: readonly string[]): Promise<Map<string, Buffer>> {
  const unique = [...new Set(blobs)]
  const contents = new Map<string, Buffer>()
  if (unique.length === 0) return contents
  const result = await execute(['git', '-C', root, 'cat-file', '--batch'], { cwd: root, env: gitEnv(), input: unique.map(blob => `${blob}\n`).join(''), timeoutMs: 120_000 })
  if (result.code !== 0) throw new BaselineError([`git cat-file --batch failed: ${result.stderr.toString('utf8').trim() || `exit ${String(result.code)}`}`])
  const out = result.stdout
  let offset = 0
  for (const blob of unique) {
    const newline = out.indexOf(0x0a, offset)
    if (newline < 0) break
    const [oid, type, size] = out.subarray(offset, newline).toString('utf8').split(' ')
    if (oid !== blob || type !== 'blob' || size === undefined) throw new BaselineError([`git cat-file could not read blob ${blob}`])
    const start = newline + 1
    contents.set(blob, out.subarray(start, start + Number(size)))
    offset = start + Number(size) + 1
  }
  if (contents.size !== unique.length) throw new BaselineError(['git cat-file returned fewer blobs than requested'])
  return contents
}

/**
 * Derive the source section of a worktree, refusing anything that would make
 * it disagree with its commit.
 * @param root - top level of a git worktree.
 * @param inputs - authoritative inputs; tests may narrow them.
 * @param absences - known absences; only entries naming HEAD's exact commit apply. Tests may replace them.
 * @throws {BaselineError} listing every dirty path, hidden change, missing input, stale absence, and unreadable fact.
 */
export async function deriveSource(
  root: string,
  inputs: readonly InputSpec[] = AUTHORITATIVE_INPUTS,
  absences: readonly KnownAbsence[] = KNOWN_ABSENCES,
): Promise<SourceSection> {
  const canonical = await worktreeRoot(root)
  const problems = await cleanlinessProblems(canonical)
  const { commit, tree, entries } = await headTree(canonical)
  const paths = [...entries.keys()].sort()
  // Every path looked up below came from `entries`, so a miss is an internal error rather than a source problem.
  const at = (path: string): TreeEntry => {
    const entry = entries.get(path)
    if (entry === undefined) throw new Error(`no tree entry for ${path}`)
    return entry
  }
  const applicable = absences.filter(absence => absence.commit === commit)
  for (const absence of applicable) {
    if (!inputs.some(spec => spec.id === absence.input && spec.patterns.includes(absence.pattern))) {
      problems.push(`known absence names no authoritative input pattern: ${absence.input} ${absence.pattern}`)
    }
  }

  const absent: SourceSection['absent'][number][] = []
  const selections: { spec: InputSpec; paths: string[] }[] = []
  for (const spec of inputs) {
    const selected = new Set<string>()
    for (const pattern of spec.patterns) {
      const matches = paths.filter(path => matchesPattern(pattern, path))
      const known = applicable.find(absence => absence.input === spec.id && absence.pattern === pattern)
      if (known !== undefined && matches.length > 0) problems.push(`known absence is stale: ${commit} has ${matches.length} file(s) matching ${pattern} of ${spec.id}`)
      else if (known !== undefined) absent.push({ input: spec.id, pattern, reason: known.reason })
      else if (matches.length === 0) problems.push(`missing authoritative input ${spec.id}: nothing in ${commit} matches ${pattern}`)
      for (const path of matches) selected.add(path)
    }
    const files: string[] = []
    for (const path of [...selected].sort()) {
      const entry = at(path)
      if (entry.type === 'blob') files.push(path)
      else problems.push(`authoritative input ${spec.id} includes a ${entry.type}, not a file: ${path}`)
    }
    selections.push({ spec, paths: files })
  }
  const facts = [...PRODUCT_MANIFESTS, SESSION_WRITER, LOCKFILE].filter((path) => {
    if (entries.get(path)?.type === 'blob') return true
    problems.push(`missing authoritative input: ${path} is not in ${commit}`)
    return false
  })
  const read = [...new Set([...selections.flatMap(selection => selection.paths), ...facts])].sort()
  const blobs = await readBlobs(canonical, read.map(path => at(path).blob))
  const content = (path: string): Buffer => {
    const data = blobs.get(at(path).blob)
    if (data === undefined) throw new Error(`blob of ${path} was not read`)
    return data
  }

  // Status can be fooled by stat caching or hidden index flags; hashing the
  // worktree through git's own filters ties each read file to its blob.
  const regular = read.filter(path => at(path).mode !== '120000')
  const present: string[] = []
  for (const path of regular) {
    if ((await lstat(join(canonical, ...path.split('/'))).catch(() => undefined))?.isFile()) present.push(path)
    else problems.push(`authoritative input ${path} is in ${commit} but not a file in the worktree`)
  }
  if (present.length > 0) {
    const hashed = (await git(canonical, ['hash-object', '--stdin-paths'], present.map(path => `${path}\n`).join(''))).split('\n')
    present.forEach((path, index) => {
      if (hashed[index] !== at(path).blob) problems.push(`worktree content of ${path} differs from ${commit}`)
    })
  }
  for (const path of read.filter(path => at(path).mode === '120000')) {
    const target = await readlink(join(canonical, ...path.split('/'))).catch(() => undefined)
    if (target === undefined || !Buffer.from(target, 'utf8').equals(content(path))) problems.push(`worktree content of ${path} differs from ${commit}`)
  }

  const groups: InputGroup[] = selections.map(({ spec, paths: selected }) => {
    const files = selected.map((path) => {
      const data = content(path)
      const { mode, blob } = at(path)
      return { path, mode, blob, sha256: sha256(data), bytes: data.length }
    })
    return { id: spec.id, description: spec.description, patterns: [...spec.patterns], digest: groupDigest(files), files }
  })
  const required = (path: string): string | undefined => (facts.includes(path) ? content(path).toString('utf8') : undefined)

  const manifests: Record<string, string> = {}
  for (const path of PRODUCT_MANIFESTS) {
    const text = required(path)
    if (text === undefined) continue
    let version: unknown
    try {
      version = (JSON.parse(text) as { version?: unknown }).version
    } catch {
      problems.push(`${path} is not valid JSON`)
      continue
    }
    if (typeof version !== 'string' || version === '') problems.push(`${path} names no version`)
    else manifests[path] = version
  }
  const versions = [...new Set(Object.values(manifests))]
  if (versions.length > 1) problems.push(`product manifests disagree on the version: ${PRODUCT_MANIFESTS.map(path => `${path}=${manifests[path] ?? '?'}`).join(', ')}`)

  const writer = required(SESSION_WRITER)
  const writerMatch = writer === undefined ? null : SESSION_WRITER_PATTERN.exec(writer)
  if (writer !== undefined && writerMatch === null) problems.push(`${SESSION_WRITER} declares no SESSION_FORMAT_VERSION`)

  const [version] = versions
  const format = writerMatch?.[1]
  if (problems.length > 0) throw new BaselineError(problems)
  // Unreachable once the checks above pass; kept so a future edit cannot record an undefined fact.
  if (version === undefined || format === undefined) throw new BaselineError(['product version or session format could not be read'])
  return {
    commit,
    tree,
    product: { version, manifests },
    sessionFormatVersion: Number(format),
    lockfile: { path: LOCKFILE, sha256: sha256(content(LOCKFILE)) },
    inputs: groups,
    absent,
  }
}

// ---------------------------------------------------------------------------
// Environment

/** The probes `environment` runs; tests inject fixed answers. */
export type ToolProbe = (tool: 'bun' | 'node' | 'git') => Promise<string | null>

async function probeTool(tool: 'bun' | 'node' | 'git'): Promise<string | null> {
  try {
    const result = await execute([tool, '--version'], { cwd: process.cwd(), env: withoutRepositoryGitEnv(process.env), timeoutMs: 15_000 })
    const line = result.stdout.toString('utf8').split('\n')[0]?.trim()
    return result.code === 0 && line ? line.slice(0, 200) : null
  } catch {
    return null
  }
}

/** Describe the host and its toolchain without reading any other environment variable. */
export async function captureEnvironment(probe: ToolProbe = probeTool, roots: readonly string[] = []): Promise<EnvironmentSection> {
  const redact = redactor(roots)
  const [bun, node, gitVersion] = await Promise.all([probe('bun'), probe('node'), probe('git')])
  const clean = (value: string | null) => (value === null ? null : redact(value))
  return {
    platform: process.platform,
    arch: process.arch,
    osRelease: release(),
    tools: { bun: clean(bun), node: clean(node), git: clean(gitVersion) },
  }
}

// ---------------------------------------------------------------------------
// Records

export interface CaptureOptions {
  readonly inputs?: readonly InputSpec[]
  readonly absences?: readonly KnownAbsence[]
  readonly probe?: ToolProbe
  readonly expectCommit?: string
}

/**
 * Capture the source provenance of a clean worktree.
 * @throws {BaselineError} when the worktree is dirty, an input is missing, or HEAD is not the expected commit.
 */
export async function captureProvenance(root: string, options: CaptureOptions = {}): Promise<ProvenanceRecord> {
  const canonical = await worktreeRoot(root)
  const source = await deriveSource(canonical, options.inputs, options.absences)
  if (options.expectCommit !== undefined && source.commit !== options.expectCommit) {
    throw new BaselineError([`HEAD is ${source.commit}, not the expected ${options.expectCommit}`])
  }
  const environment = await captureEnvironment(options.probe, [canonical, resolve(root)])
  // A concurrent writer could change the worktree while it was hashed.
  const after = await deriveSource(canonical, options.inputs, options.absences)
  if (sourceDigest(after) !== sourceDigest(source)) throw new BaselineError(['worktree changed while it was captured'])
  const missingEvidence = [
    ...source.absent.map(({ input, pattern, reason }) => `${input}: nothing matches ${pattern} at ${source.commit}; ${reason}`),
    'build: no build was run or observed by source provenance',
    'tests: no test was run or observed by source provenance',
    'performance: no measurement was taken by source provenance',
    'publication: whether this commit was released is verified separately',
    ...Object.entries(environment.tools).filter(([, version]) => version === null).map(([tool]) => `environment: ${tool} --version could not be run`),
  ]
  return {
    schema: PROVENANCE_SCHEMA,
    schemaVersion: SCHEMA_VERSION,
    scope: '00',
    kind: 'source-provenance',
    qualification: 'not-evaluated',
    statement: PROVENANCE_STATEMENT,
    sourceDigest: sourceDigest(source),
    source,
    environment,
    evidence: { build: 'not-recorded', tests: 'not-recorded', performance: 'not-recorded' },
    missingEvidence,
  }
}

type Json = Record<string, unknown>

function isObject(value: unknown): value is Json {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

const HEX40 = /^[0-9a-f]{40}$|^[0-9a-f]{64}$/u
const HEX64 = /^[0-9a-f]{64}$/u

function environmentProblems(value: unknown, where: string): string[] {
  if (!isObject(value)) return [`${where}.environment is missing`]
  const problems: string[] = []
  for (const key of ['platform', 'arch', 'osRelease']) if (typeof value[key] !== 'string') problems.push(`${where}.environment.${key} is missing`)
  if (!isObject(value.tools)) problems.push(`${where}.environment.tools is missing`)
  else for (const tool of ['bun', 'node', 'git']) if (!(tool in value.tools) || !(value.tools[tool] === null || typeof value.tools[tool] === 'string')) problems.push(`${where}.environment.tools.${tool} is missing`)
  return problems
}

/**
 * Structural problems of a parsed provenance record, including any claim that
 * it qualifies something. An empty list means the shape is complete.
 */
export function provenanceShapeProblems(value: unknown): string[] {
  if (!isObject(value)) return ['record is not a JSON object']
  const problems: string[] = []
  const expect = (key: string, wanted: unknown) => {
    if (value[key] !== wanted) problems.push(`record.${key} must be ${JSON.stringify(wanted)}, found ${JSON.stringify(value[key])}`)
  }
  expect('schema', PROVENANCE_SCHEMA)
  expect('schemaVersion', SCHEMA_VERSION)
  expect('scope', '00')
  expect('kind', 'source-provenance')
  expect('qualification', 'not-evaluated')
  expect('statement', PROVENANCE_STATEMENT)
  if (typeof value.sourceDigest !== 'string' || !HEX64.test(value.sourceDigest)) problems.push('record.sourceDigest is missing')
  const evidence = value.evidence
  if (!isObject(evidence) || ['build', 'tests', 'performance'].some(key => evidence[key] !== 'not-recorded') || Object.keys(evidence).length !== 3) {
    problems.push('record.evidence must mark build, tests, and performance not-recorded; results belong in check-result records')
  }
  if (!Array.isArray(value.missingEvidence) || !value.missingEvidence.every(item => typeof item === 'string')) problems.push('record.missingEvidence is missing')
  problems.push(...environmentProblems(value.environment, 'record'))
  const source = value.source
  if (!isObject(source)) return [...problems, 'record.source is missing']
  if (typeof source.commit !== 'string' || !HEX40.test(source.commit)) problems.push('record.source.commit is missing')
  if (typeof source.tree !== 'string' || !HEX40.test(source.tree)) problems.push('record.source.tree is missing')
  if (!isObject(source.product) || typeof source.product.version !== 'string' || !isObject(source.product.manifests)) problems.push('record.source.product is incomplete')
  if (!Number.isSafeInteger(source.sessionFormatVersion)) problems.push('record.source.sessionFormatVersion is missing')
  if (!isObject(source.lockfile) || source.lockfile.path !== LOCKFILE || typeof source.lockfile.sha256 !== 'string' || !HEX64.test(source.lockfile.sha256)) problems.push('record.source.lockfile is incomplete')
  const absent = Array.isArray(source.absent) ? source.absent : undefined
  if (absent === undefined || !absent.every(item => isObject(item) && typeof item.input === 'string' && typeof item.pattern === 'string' && typeof item.reason === 'string' && item.reason !== '')) {
    problems.push('record.source.absent is missing')
  }
  const absentPatterns = new Set((absent ?? []).filter(isObject).map(item => `${String(item.input)}\0${String(item.pattern)}`))
  if (!Array.isArray(source.inputs) || source.inputs.length === 0) problems.push('record.source.inputs is missing')
  else {
    for (const [index, group] of source.inputs.entries()) {
      if (!isObject(group) || typeof group.id !== 'string' || typeof group.digest !== 'string' || !Array.isArray(group.files) || !Array.isArray(group.patterns)) {
        problems.push(`record.source.inputs[${index}] is incomplete`)
        continue
      }
      // A group may be empty only when the record names every one of its patterns absent.
      if (group.files.length === 0 && !group.patterns.every(pattern => absentPatterns.has(`${group.id}\0${String(pattern)}`))) {
        problems.push(`record.source.inputs[${group.id}] has no files and no recorded absence`)
      }
      for (const file of group.files) {
        if (!isObject(file) || typeof file.path !== 'string' || typeof file.sha256 !== 'string' || !HEX64.test(file.sha256) || typeof file.blob !== 'string' || !Number.isSafeInteger(file.bytes)) {
          problems.push(`record.source.inputs[${group.id}] has an incomplete file entry`)
        }
      }
    }
  }
  return problems
}

/** Field-level differences between a recorded source and the worktree's. */
function sourceDifferences(recorded: SourceSection, actual: SourceSection): string[] {
  const problems: string[] = []
  if (recorded.commit !== actual.commit) problems.push(`commit: record ${recorded.commit}, worktree ${actual.commit}`)
  if (recorded.tree !== actual.tree) problems.push(`tree: record ${recorded.tree}, worktree ${actual.tree}`)
  if (JSON.stringify(sortKeys(recorded.product)) !== JSON.stringify(sortKeys(actual.product))) problems.push(`product version: record ${JSON.stringify(recorded.product)}, worktree ${JSON.stringify(actual.product)}`)
  if (recorded.sessionFormatVersion !== actual.sessionFormatVersion) problems.push(`session format: record ${recorded.sessionFormatVersion}, worktree ${actual.sessionFormatVersion}`)
  if (recorded.lockfile.sha256 !== actual.lockfile.sha256) problems.push(`${LOCKFILE} sha256: record ${recorded.lockfile.sha256}, worktree ${actual.lockfile.sha256}`)
  if (JSON.stringify(sortKeys(recorded.absent)) !== JSON.stringify(sortKeys(actual.absent))) problems.push(`known absences: record ${JSON.stringify(recorded.absent)}, worktree ${JSON.stringify(actual.absent)}`)
  const actualGroups = new Map(actual.inputs.map(group => [group.id, group]))
  const recordedIds = new Set(recorded.inputs.map(group => group.id))
  for (const id of actualGroups.keys()) if (!recordedIds.has(id)) problems.push(`input ${id}: missing from the record`)
  for (const group of recorded.inputs) {
    const current = actualGroups.get(group.id)
    if (current === undefined) {
      problems.push(`input ${group.id}: not an authoritative input of this tool`)
      continue
    }
    if (groupDigest(group.files) !== group.digest) problems.push(`input ${group.id}: recorded digest does not match its recorded files`)
    if (JSON.stringify(group.patterns) !== JSON.stringify(current.patterns)) problems.push(`input ${group.id}: patterns differ from this tool's`)
    const files = new Map(current.files.map(file => [file.path, file]))
    const seen = new Set<string>()
    for (const file of group.files) {
      seen.add(file.path)
      const now = files.get(file.path)
      if (now === undefined) problems.push(`input ${group.id}: ${file.path} is recorded but absent`)
      else if (now.sha256 !== file.sha256 || now.blob !== file.blob || now.bytes !== file.bytes || now.mode !== file.mode) problems.push(`input ${group.id}: ${file.path} differs (record sha256 ${file.sha256}, worktree ${now.sha256})`)
    }
    for (const path of files.keys()) if (!seen.has(path)) problems.push(`input ${group.id}: ${path} is present but not recorded`)
    if (group.digest !== current.digest && problems.every(problem => !problem.startsWith(`input ${group.id}:`))) problems.push(`input ${group.id}: digest differs`)
  }
  return problems
}

/**
 * Verify a provenance record against a worktree without writing anything.
 * @returns every problem found; an empty list means the record identifies exactly this clean worktree.
 */
export async function verifyProvenance(
  root: string,
  record: unknown,
  inputs: readonly InputSpec[] = AUTHORITATIVE_INPUTS,
  absences: readonly KnownAbsence[] = KNOWN_ABSENCES,
): Promise<string[]> {
  const shape = provenanceShapeProblems(record)
  if (shape.length > 0) return shape
  const recorded = record as ProvenanceRecord
  const problems: string[] = []
  if (sourceDigest(recorded.source) !== recorded.sourceDigest) problems.push('record.sourceDigest does not match the recorded source')
  let actual: SourceSection
  try {
    actual = await deriveSource(root, inputs, absences)
  } catch (error) {
    if (error instanceof BaselineError) return [...problems, ...error.problems]
    throw error
  }
  return [...problems, ...sourceDifferences(recorded.source, actual)]
}

/** The outcome a result's own observations imply. */
export function impliedOutcome(result: Pick<CheckResult, 'exitCode' | 'signal' | 'timedOut' | 'interrupted' | 'descendantsRemained' | 'sourceUnchanged'>): CheckOutcome {
  if (!result.sourceUnchanged) return 'invalid'
  const clean = result.exitCode === 0 && result.signal === null && !result.timedOut && !result.interrupted && !result.descendantsRemained
  return clean ? 'passed' : 'failed'
}

/**
 * Verify a check result against its provenance and its log, read-only.
 * @param resultPath - the result file; its log lies beside it.
 */
export async function verifyCheckResult(resultPath: string, result: unknown, provenance: ProvenanceRecord): Promise<string[]> {
  const where = basename(resultPath)
  if (!isObject(result)) return [`${where}: not a JSON object`]
  const problems: string[] = []
  const expect = (key: string, wanted: unknown) => {
    if (result[key] !== wanted) problems.push(`${where}: ${key} must be ${JSON.stringify(wanted)}, found ${JSON.stringify(result[key])}`)
  }
  expect('schema', CHECK_RESULT_SCHEMA)
  expect('schemaVersion', SCHEMA_VERSION)
  expect('scope', '00')
  expect('kind', 'check-result')
  expect('cwd', '.')
  if (typeof result.id !== 'string' || result.id === '') problems.push(`${where}: id is missing`)
  if (!Array.isArray(result.argv) || result.argv.length === 0 || !result.argv.every(arg => typeof arg === 'string')) problems.push(`${where}: argv is missing`)
  if (typeof result.startedAt !== 'string' || Number.isNaN(Date.parse(result.startedAt))) problems.push(`${where}: startedAt is missing`)
  for (const key of ['durationMs', 'timeoutMs']) if (typeof result[key] !== 'number' || !Number.isFinite(result[key]) || (result[key] as number) < 0) problems.push(`${where}: ${key} is missing`)
  if (!(result.exitCode === null || Number.isSafeInteger(result.exitCode))) problems.push(`${where}: exitCode is missing`)
  if (!(result.signal === null || typeof result.signal === 'string')) problems.push(`${where}: signal is missing`)
  if (result.exitCode === null && result.signal === null) problems.push(`${where}: neither an exit code nor a signal was observed`)
  for (const key of ['timedOut', 'interrupted', 'descendantsRemained', 'sourceUnchanged']) if (typeof result[key] !== 'boolean') problems.push(`${where}: ${key} is missing`)
  problems.push(...environmentProblems(result.environment, where))
  const source = result.source
  if (!isObject(source) || source.commit !== provenance.source.commit || source.sourceDigest !== provenance.sourceDigest) {
    problems.push(`${where}: ran against a different source than the provenance record`)
  }
  if (problems.length === 0) {
    const implied = impliedOutcome(result as unknown as CheckResult)
    if (result.outcome !== implied) problems.push(`${where}: outcome ${JSON.stringify(result.outcome)} contradicts its observations, which imply ${implied}`)
  }
  const log = result.log
  if (!isObject(log) || typeof log.file !== 'string' || typeof log.sha256 !== 'string' || basename(log.file) !== log.file) problems.push(`${where}: log reference is missing`)
  else {
    const content = await readFile(join(dirname(resultPath), log.file)).catch(() => undefined)
    if (content === undefined) problems.push(`${where}: log ${log.file} is missing`)
    else if (sha256(content) !== log.sha256 || content.length !== log.bytes) problems.push(`${where}: log ${log.file} does not match its recorded sha256`)
  }
  return problems
}

// ---------------------------------------------------------------------------
// Output files

/** Resolve symlinks in the longest existing ancestor, so a path compares with the canonical root. */
async function canonicalPath(absolute: string): Promise<string> {
  const parent = dirname(absolute)
  if (parent === absolute) return absolute
  const resolvedParent = await realpath(parent).catch(() => undefined)
  return join(resolvedParent ?? await canonicalPath(parent), basename(absolute))
}

/**
 * Refuse an output path that would dirty the worktree or replace a file unasked.
 * @returns the absolute path.
 */
async function prepareOutput(root: string, path: string, force: boolean): Promise<string> {
  const target = await canonicalPath(resolve(path))
  const inside = relative(root, target)
  // A name such as `..notes` is inside the worktree; only `..` itself or a `../` prefix leaves it.
  if (inside === '' || (inside !== '..' && !inside.startsWith(`..${sep}`) && !isAbsolute(inside))) {
    const rel = inside.split(sep).join('/')
    if ((await gitStatus(root, ['check-ignore', '-q', '--', rel])) !== 0) {
      throw new BaselineError([`output ${rel} is inside the worktree but not ignored by git; use an ignored directory such as .preflight/ or a path outside it`])
    }
  }
  const existing = await lstat(target).catch(() => undefined)
  if (existing !== undefined && !force) throw new BaselineError([`output ${basename(target)} already exists; pass --force to replace it`])
  if (existing !== undefined && !existing.isFile()) throw new BaselineError([`output ${basename(target)} exists and is not a file`])
  await mkdir(dirname(target), { recursive: true })
  return target
}

async function writeAtomically(path: string, content: string): Promise<void> {
  const temporary = `${path}.${process.pid}.tmp`
  try {
    await writeFile(temporary, content, { flag: 'wx' })
    await rename(temporary, path)
  } finally {
    await rm(temporary, { force: true })
  }
}

/** Serialize a record with one final newline. */
export function serialize(record: ProvenanceRecord | CheckResult): string {
  return `${JSON.stringify(record, null, 2)}\n`
}

// ---------------------------------------------------------------------------
// Running one check

export interface RunOptions {
  readonly id: string
  readonly argv: readonly string[]
  readonly out: string
  readonly timeoutMs?: number
  readonly force?: boolean
  readonly inputs?: readonly InputSpec[]
  readonly absences?: readonly KnownAbsence[]
  readonly probe?: ToolProbe
  /** Grace between SIGTERM and SIGKILL for a timed-out or interrupted command. */
  readonly killGraceMs?: number
}

/** Whether any process remains in a POSIX process group. */
function groupAlive(pgid: number): boolean {
  try {
    process.kill(-pgid, 0)
    return true
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM'
  }
}

/** Signal a command's process group; a command that never spawned has no pid and nothing to signal. */
function signalGroup(pid: number | undefined, signal: NodeJS.Signals): void {
  if (pid === undefined) return
  try {
    if (process.platform === 'win32') process.kill(pid, signal)
    else process.kill(-pid, signal)
  } catch {
    // Already gone.
  }
}

/**
 * Run one command in a worktree the provenance record identifies, and record
 * what was observed. The command runs without a shell in its own process
 * group; a timeout or interruption terminates the whole group, and the result
 * is written only after the group has exited.
 * @throws {BaselineError} when the record does not verify or the output path is unsafe; nothing runs then.
 */
export async function runCheck(root: string, provenance: unknown, options: RunOptions): Promise<{ result: CheckResult; path: string }> {
  const canonical = await worktreeRoot(root)
  const problems = await verifyProvenance(canonical, provenance, options.inputs, options.absences)
  if (problems.length > 0) throw new BaselineError(problems)
  const record = provenance as ProvenanceRecord
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/u.test(options.id)) throw new BaselineError([`--id must be letters, digits, '.', '_', or '-': ${options.id}`])
  const [command, ...args] = options.argv
  if (command === undefined) throw new BaselineError(['run needs a command after --'])
  const resultPath = await prepareOutput(canonical, options.out, options.force ?? false)
  const logPath = await prepareOutput(canonical, resultPath.replace(/\.json$/u, '') + '.log', options.force ?? false)
  const timeoutMs = options.timeoutMs ?? 3_600_000
  const graceMs = options.killGraceMs ?? 5_000
  const redact = redactor([canonical, resolve(root)])
  const argv = options.argv.map(redact)
  const environment = await captureEnvironment(options.probe, [canonical, resolve(root)])

  await writeFile(logPath, `$ ${argv.join(' ')}\n`)
  const log = openSync(logPath, 'a')
  const startedAt = new Date().toISOString()
  const started = performance.now()
  let timedOut = false
  let interrupted = false
  let descendantsRemained = false
  let observed: { code: number | null; signal: string | null }
  try {
    const child = spawn(command, args, {
      cwd: canonical,
      env: { ...withoutRepositoryGitEnv(process.env), FORCE_COLOR: '0' },
      stdio: ['ignore', log, log],
      detached: process.platform !== 'win32',
      shell: false,
    })
    let killTimer: ReturnType<typeof setTimeout> | undefined
    const stop = () => {
      signalGroup(child.pid, 'SIGTERM')
      killTimer ??= setTimeout(() => signalGroup(child.pid, 'SIGKILL'), graceMs)
    }
    const deadline = setTimeout(() => {
      timedOut = true
      stop()
    }, timeoutMs)
    const onSignal = () => {
      interrupted = true
      stop()
    }
    process.on('SIGINT', onSignal)
    process.on('SIGTERM', onSignal)
    try {
      observed = await new Promise((done) => {
        child.on('error', (error) => {
          writeFile(logPath, `\n${redact(String(error))}\n`, { flag: 'a' }).finally(() => done({ code: 127, signal: null }))
        })
        child.on('exit', (code, signal) => done({ code, signal }))
      })
      if (process.platform !== 'win32' && child.pid !== undefined && groupAlive(child.pid)) {
        descendantsRemained = true
        signalGroup(child.pid, 'SIGKILL')
        // A killed process can still write until it is gone, so the source check waits for the group to empty.
        const deadline = performance.now() + Math.max(graceMs, 5_000)
        while (groupAlive(child.pid) && performance.now() < deadline) await new Promise(done => setTimeout(done, 20))
        if (groupAlive(child.pid)) throw new BaselineError([`processes of ${options.id} survived SIGKILL; no result was written`])
      }
    } finally {
      clearTimeout(deadline)
      clearTimeout(killTimer)
      process.off('SIGINT', onSignal)
      process.off('SIGTERM', onSignal)
    }
  } finally {
    closeSync(log)
  }
  const durationMs = Math.round(performance.now() - started)

  let sourceUnchanged = false
  try {
    sourceUnchanged = sourceDigest(await deriveSource(canonical, options.inputs, options.absences)) === record.sourceDigest
  } catch (error) {
    if (!(error instanceof BaselineError)) throw error
  }
  const logContent = await readFile(logPath)
  const observations = { exitCode: observed.code, signal: observed.signal, timedOut, interrupted, descendantsRemained, sourceUnchanged }
  const result: CheckResult = {
    schema: CHECK_RESULT_SCHEMA,
    schemaVersion: SCHEMA_VERSION,
    scope: '00',
    kind: 'check-result',
    id: options.id,
    argv,
    cwd: '.',
    source: { commit: record.source.commit, sourceDigest: record.sourceDigest },
    environment,
    startedAt,
    durationMs,
    timeoutMs,
    ...observations,
    log: { file: basename(logPath), sha256: sha256(logContent), bytes: logContent.length },
    outcome: impliedOutcome(observations),
  }
  await writeAtomically(resultPath, serialize(result))
  return { result, path: resultPath }
}

// ---------------------------------------------------------------------------
// CLI

export const USAGE = `Usage: bun scripts/rust-migration-baseline.ts <command> [options]

Source provenance for the Rust migration's scope-00 TypeScript baseline.
A provenance record identifies source only; it never records a passing build,
test, or measurement. Check results from \`run\` are separate records.

Commands:
  capture   record the source provenance of a clean worktree
  check     verify a provenance record, and any check results, read-only
  run       run one command in a verified worktree and record its outcome
  help      print this text

Options:
  --root <dir>           worktree top level (default: current directory)
  --out <file>           capture: record to write (default: <root>/${DEFAULT_RECORD})
                         run: check result to write; its log is written beside it
  --record <file>        check, run: provenance record (default: <root>/${DEFAULT_RECORD})
  --result <file>        check: a check result to validate (repeatable)
  --expect-commit <sha>  capture, check: require HEAD to be this commit
  --id <name>            run: result identifier
  --timeout-ms <n>       run: deadline for the command (default: 3600000)
  --force                replace an existing output file
  -- <argv...>           run: the command, executed without a shell

Outputs inside the worktree must be git-ignored, such as .preflight/.
Roadmap baselines: ${ROADMAP_BASELINE.develop.ref} ${ROADMAP_BASELINE.develop.commit},
                   ${ROADMAP_BASELINE.release.ref} ${ROADMAP_BASELINE.release.commit}.
${ROADMAP_BASELINE.release.ref} predates the model-surface snapshots; its record lists them as
missing evidence. Any other missing input is refused.
Exit status: 0 on success; 1 when run records, or check verifies, a check result
whose outcome is not passed; 2 on refusal or usage error.`

interface Cli {
  command: string
  root: string
  out?: string
  record?: string
  results: string[]
  expectCommit?: string
  id?: string
  timeoutMs?: number
  force: boolean
  argv: string[]
}

function parseCli(args: readonly string[]): Cli {
  const first = args[0] ?? 'help'
  const cli: Cli = { command: first === '--help' || first === '-h' ? 'help' : first, root: '.', results: [], force: false, argv: [] }
  for (let index = 1; index < args.length; index++) {
    const arg = args[index] ?? ''
    if (arg === '--') {
      cli.argv = args.slice(index + 1)
      break
    }
    const value = () => {
      const next = args[++index]
      if (next === undefined || next.startsWith('--')) throw new BaselineError([`${arg} needs a value`])
      return next
    }
    if (arg === '--root') cli.root = value()
    else if (arg === '--out') cli.out = value()
    else if (arg === '--record') cli.record = value()
    else if (arg === '--result') cli.results.push(value())
    else if (arg === '--expect-commit') cli.expectCommit = value()
    else if (arg === '--id') cli.id = value()
    else if (arg === '--timeout-ms') {
      const raw = value()
      const parsed = Number(raw)
      if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new BaselineError([`--timeout-ms must be a positive integer: ${raw}`])
      cli.timeoutMs = parsed
    } else if (arg === '--force') cli.force = true
    else if (arg === '--help' || arg === '-h') cli.command = 'help'
    else throw new BaselineError([`unknown option ${arg}`])
  }
  return cli
}

async function readJson(path: string): Promise<unknown> {
  const text = await readFile(path, 'utf8').catch(() => {
    throw new BaselineError([`cannot read ${basename(path)}`])
  })
  try {
    return JSON.parse(text)
  } catch {
    throw new BaselineError([`${basename(path)} is not valid JSON`])
  }
}

/**
 * Run the CLI.
 * @param args - arguments after the script path.
 * @param output - receives report lines; defaults to stdout and stderr.
 * @returns the process exit status.
 */
export async function main(
  args: readonly string[],
  output: { log: (line: string) => void; error: (line: string) => void } = console,
): Promise<number> {
  try {
    const cli = parseCli(args)
    if (cli.command === 'help') {
      output.log(USAGE)
      return 0
    }
    const root = await worktreeRoot(cli.root)
    const recordPath = resolve(cli.record ?? join(root, DEFAULT_RECORD))
    if (cli.command === 'capture') {
      const record = await captureProvenance(root, { expectCommit: cli.expectCommit })
      const path = await prepareOutput(root, cli.out ?? join(root, DEFAULT_RECORD), cli.force)
      await writeAtomically(path, serialize(record))
      output.log(`captured source provenance of ${record.source.commit} (bake ${record.source.product.version}, session format ${record.source.sessionFormatVersion})`)
      output.log(`sourceDigest ${record.sourceDigest}`)
      output.log(`wrote ${redactor([root])(path)}; qualification not evaluated`)
      return 0
    }
    if (cli.command === 'check') {
      const record = await readJson(recordPath)
      const problems = await verifyProvenance(root, record)
      if (problems.length === 0 && cli.expectCommit !== undefined && (record as ProvenanceRecord).source.commit !== cli.expectCommit) {
        problems.push(`record identifies ${(record as ProvenanceRecord).source.commit}, not the expected ${cli.expectCommit}`)
      }
      if (problems.length === 0) {
        for (const path of cli.results) {
          problems.push(...await verifyCheckResult(resolve(path), await readJson(resolve(path)), record as ProvenanceRecord))
        }
      }
      if (problems.length > 0) throw new BaselineError(problems)
      const verified = record as ProvenanceRecord
      output.log(`source provenance verified: ${verified.source.commit}, sourceDigest ${verified.sourceDigest}`)
      let allPassed = true
      for (const path of cli.results) {
        const result = (await readJson(resolve(path))) as CheckResult
        output.log(`check result ${result.id}: ${result.outcome}`)
        if (result.outcome !== 'passed') allPassed = false
      }
      return allPassed ? 0 : 1
    }
    if (cli.command === 'run') {
      if (cli.id === undefined || cli.out === undefined) throw new BaselineError(['run needs --id and --out'])
      const options = { id: cli.id, argv: cli.argv, out: cli.out, timeoutMs: cli.timeoutMs, force: cli.force }
      const { result, path } = await runCheck(root, await readJson(recordPath), options)
      output.log(`check ${result.id}: ${result.outcome} (exit ${String(result.exitCode)}${result.signal ? `, signal ${result.signal}` : ''}${result.timedOut ? ', timed out' : ''}); wrote ${redactor([root])(path)}`)
      return result.outcome === 'passed' ? 0 : 1
    }
    throw new BaselineError([`unknown command ${cli.command}; see help`])
  } catch (error) {
    if (!(error instanceof BaselineError)) throw error
    for (const problem of error.problems) output.error(`rust-migration-baseline: ${problem}`)
    return 2
  }
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
