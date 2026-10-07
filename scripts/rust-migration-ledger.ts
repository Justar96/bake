/**
 * Offline validator for the Rust migration's qualification evidence ledger.
 *
 * The ledger holds one JSON record per scope qualification attempt under
 * `scope-NN/<id>.json`. `--check` reads every record and reports structural
 * errors, malformed digests and commits, dangling references, unexplained
 * omissions, and contradictions a record states about itself. It never runs
 * git, touches the network, or writes a file.
 *
 * A valid record is not qualification evidence that anything passed. The
 * validator cannot prove that a command ran, that a referenced URL or artifact
 * still exists, that a manifest digest matches the bytes at the candidate
 * commit, or that anyone accepted a scope. Scope acceptance stays a separate
 * roadmap decision, so no record status claims completion.
 * @module scripts/rust-migration-ledger
 */

import { createHash } from 'node:crypto'
import { constants } from 'node:fs'
import { lstat, open, readdir } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const RECORD_SCHEMA = 'bake/rust-migration/qualification-attempt'
export const RECORD_VERSION = 1
export const SCOPES = Array.from({ length: 18 }, (_, index) => String(index).padStart(2, '0'))
export const STATUSES = ['partial', 'failed', 'evidence-review'] as const
/** Records above this size are refused unread. */
export const MAX_RECORD_BYTES = 1024 * 1024
/** The checkout's ledger, which `--check` reads unless `--ledger` names another. */
export const DEFAULT_LEDGER = join(dirname(dirname(fileURLToPath(import.meta.url))), 'docs/roadmap/rust-0.4/ledger')

/** The fact applies but was not observed or recorded. */
export interface Missing { readonly missing: string }
/** The fact does not apply to this attempt. */
export interface NotApplicable { readonly notApplicable: string }
/** An `https://` URL or a literal repository-relative path. */
export type Reference = string

export type RecordStatus = (typeof STATUSES)[number]

export interface SupportEntry { readonly id: string; readonly reference: Reference }

/** Literal files, strictly sorted by path. */
export interface FixtureManifest {
  readonly files: readonly { readonly path: string; readonly role: 'shared-fixture-bytes' | 'fixture-definition-source'; readonly sha256: string }[]
  /** SHA-256 over each entry's `path NUL sha256 LF`, in order. */
  readonly digest: string
}

export interface Artifact { readonly id: string; readonly description: string; readonly sha256: string | Missing; readonly command: string }

export interface Command {
  readonly id: string
  readonly argv: readonly string[]
  readonly cwd: string
  readonly host: { readonly os: string; readonly arch: string; readonly runner: string }
  readonly tools: Readonly<Record<string, string | Missing>> | Missing
  /** The checkout the command tested, such as a pull request's synthetic merge; it may differ from the candidate. */
  readonly testedCommit: string | Missing
  readonly expectedExit: number
  readonly actualExit: number | Missing
  readonly counts: { readonly passed: number; readonly failed: number; readonly skipped: number; readonly warned: number } | Missing
  readonly evidence: { readonly reference: Reference; readonly sha256?: string } | Missing
}

export interface Result {
  readonly id: string
  readonly behavior: string
  readonly owner: string
  readonly report: Reference | Missing
  readonly commands: readonly string[]
  readonly outcome: 'pass' | 'fail' | 'skip' | 'missing'
  /** Required for `skip` and `missing`. */
  readonly reason?: string
}

export interface NegativeControl {
  readonly id: string
  readonly defect: string
  readonly assertion: string
  readonly observed: 'rejected' | 'accepted' | 'not-run'
  readonly commands: readonly string[]
  /** Required for `not-run`. */
  readonly reason?: string
}

export interface MissingEvidence { readonly what: string; readonly reason: string; readonly owner: string }

export interface Review { readonly reviewer: string; readonly state: 'requested' | 'commented' | 'changes-requested' | 'approved'; readonly reference: Reference }

/**
 * One qualification attempt. Evidence lists are non-empty or replaced by an
 * explicit absence; `missingEvidence` alone may be empty.
 */
export interface QualificationAttempt {
  readonly schema: typeof RECORD_SCHEMA
  readonly version: typeof RECORD_VERSION
  readonly id: string
  readonly scope: string
  readonly status: RecordStatus
  readonly recordedOn: string
  readonly summary: string
  /** Earlier attempts in the same scope corrected or followed up here; their observations stay unchanged. */
  readonly supersedes: readonly string[]
  readonly provenance: { readonly base: string; readonly candidate: string; readonly oracle: string | Missing | NotApplicable }
  readonly support: readonly SupportEntry[] | Missing | NotApplicable
  readonly fixtures: FixtureManifest | Missing | NotApplicable
  readonly artifacts: readonly Artifact[] | Missing | NotApplicable
  readonly commands: readonly Command[] | Missing
  readonly results: readonly Result[] | Missing
  readonly negativeControls: readonly NegativeControl[] | Missing | NotApplicable
  readonly missingEvidence: readonly MissingEvidence[]
  readonly evalRecords: readonly Reference[] | Missing | NotApplicable
  readonly performanceRecords: readonly Reference[] | Missing | NotApplicable
  readonly rollback: { readonly observed: string; readonly commands: readonly string[] } | Missing | NotApplicable
  readonly review: Review | Missing
}

export interface LedgerReport {
  /** Records without problems of their own, in path order. */
  readonly records: readonly { readonly path: string; readonly record: QualificationAttempt }[]
  /** Sorted diagnostics, each prefixed by the ledger-relative record path. */
  readonly problems: readonly string[]
}

/** The ledger directory itself is unusable; no record was judged. */
export class LedgerSetupError extends Error {}

const SHA40 = /^[0-9a-f]{40}$/
const SHA64 = /^[0-9a-f]{64}$/
const ID = /^[a-z0-9]+(?:-[a-z0-9]+)*$/
const SEGMENT = /^[A-Za-z0-9._@+-]+$/
const URL_REFERENCE = /^https:\/\/[^\s]+$/
const DATE = /^\d{4}-\d{2}-\d{2}$/

type Absence = 'missing' | 'notApplicable'
type Fields = Record<string, unknown>

function isObject(value: unknown): value is Fields {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function manifestDigest(files: readonly { readonly path: string; readonly sha256: string }[]): string {
  return createHash('sha256').update(files.map(file => `${file.path}\0${file.sha256}\n`).join('')).digest('hex')
}

function safePath(value: unknown, allowDot = false): boolean {
  if (typeof value !== 'string') return false
  if (allowDot && value === '.') return true
  return value.split('/').every(segment => SEGMENT.test(segment) && segment !== '.' && segment !== '..')
}

/** Collects diagnostics for one record file. */
class RecordCheck {
  constructor(readonly file: string, readonly problems: string[]) {}

  add(path: string, message: string): void {
    this.problems.push(`${this.file}: ${path || '(record)'} ${message}`)
  }

  /** Exact keys: every required key present, nothing outside required and optional. */
  shape(path: string, value: unknown, required: readonly string[], optional: readonly string[] = []): value is Fields {
    if (!isObject(value)) {
      this.add(path, 'must be an object')
      return false
    }
    for (const key of required) if (!Object.hasOwn(value, key)) this.add(field(path, key), 'is required')
    for (const key of Object.keys(value).sort()) if (!required.includes(key) && !optional.includes(key)) this.add(field(path, key), 'is not a recognized field')
    return required.every(key => Object.hasOwn(value, key))
  }

  /** True when `value` is an explicit absence, reporting a disallowed kind or blank reason. */
  absence(path: string, value: unknown, allowed: readonly Absence[]): boolean {
    if (!isObject(value) || !(Object.hasOwn(value, 'missing') || Object.hasOwn(value, 'notApplicable'))) return false
    const keys = Object.keys(value)
    const kind = keys[0] as Absence
    if (keys.length !== 1) this.add(path, 'must hold exactly one of missing or notApplicable')
    else if (!allowed.includes(kind)) this.add(path, `cannot be ${kind}; use ${allowed.map(name => `{"${name}": reason}`).join(' or ')}`)
    else this.text(field(path, kind), value[kind])
    return true
  }

  text(path: string, value: unknown): value is string {
    if (typeof value === 'string' && value.trim() !== '') return true
    this.add(path, 'must be a non-blank string')
    return false
  }

  match(path: string, value: unknown, pattern: RegExp, expected: string): boolean {
    if (typeof value === 'string' && pattern.test(value)) return true
    this.add(path, `must be ${expected}`)
    return false
  }

  oneOf(path: string, value: unknown, allowed: readonly string[]): boolean {
    if (typeof value === 'string' && allowed.includes(value)) return true
    this.add(path, `must be one of ${allowed.join(', ')}`)
    return false
  }

  count(path: string, value: unknown): boolean {
    if (typeof value === 'number' && Number.isSafeInteger(value) && value >= 0) return true
    this.add(path, 'must be a non-negative integer')
    return false
  }

  reference(path: string, value: unknown): void {
    if (typeof value === 'string' && (URL_REFERENCE.test(value) || safePath(value))) return
    this.add(path, 'must be an https:// URL or a literal repository-relative path')
  }

  /** A non-empty list, or an absence of an allowed kind; an empty list is an unexplained omission. */
  list(path: string, value: unknown, allowed: readonly Absence[], each: (path: string, item: unknown) => void): void {
    if (this.absence(path, value, allowed)) return
    if (!Array.isArray(value)) {
      this.add(path, `must be an array${allowed.length > 0 ? ` or ${allowed.map(name => `{"${name}": reason}`).join(' or ')}` : ''}`)
      return
    }
    if (value.length === 0) this.add(path, `is empty; explain the omission with ${allowed.map(name => `{"${name}": reason}`).join(' or ')}`)
    value.forEach((item, index) => each(`${path}[${index}]`, item))
  }

  /** Ids of `items` that are objects with string ids; reports duplicates and malformed ids. */
  ids(path: string, items: unknown): Set<string> {
    const seen = new Set<string>()
    if (!Array.isArray(items)) return seen
    items.forEach((item, index) => {
      if (!isObject(item) || !Object.hasOwn(item, 'id')) return
      if (!this.match(`${path}[${index}].id`, item.id, ID, 'a lowercase id such as native-smoke')) return
      if (seen.has(item.id as string)) this.add(`${path}[${index}].id`, `duplicates ${String(item.id)}`)
      seen.add(item.id as string)
    })
    return seen
  }
}

function field(path: string, key: string): string {
  return path === '' ? key : `${path}.${key}`
}

function validDate(value: unknown): boolean {
  if (typeof value !== 'string' || !DATE.test(value)) return false
  const date = new Date(`${value}T00:00:00Z`)
  return !Number.isNaN(date.getTime()) && date.toISOString().slice(0, 10) === value
}

const RECORD_FIELDS = [
  'schema', 'version', 'id', 'scope', 'status', 'recordedOn', 'summary', 'supersedes', 'provenance', 'support', 'fixtures', 'artifacts',
  'commands', 'results', 'negativeControls', 'missingEvidence', 'evalRecords', 'performanceRecords', 'rollback', 'review',
]

/**
 * Validate one parsed record against its own content and its ledger location.
 * @param file - ledger-relative path `scope-NN/<name>.json`, used for diagnostics and placement.
 * @returns diagnostics; empty when the record is valid.
 */
export function recordProblems(file: string, record: unknown): string[] {
  const problems: string[] = []
  const check = new RecordCheck(file, problems)
  if (!check.shape('', record, RECORD_FIELDS)) return problems
  const [directory, name] = file.split('/')
  if (record.schema !== RECORD_SCHEMA) check.add('schema', `must be ${RECORD_SCHEMA}`)
  if (record.version !== RECORD_VERSION) check.add('version', `must be ${RECORD_VERSION}`)
  if (check.match('id', record.id, ID, 'a lowercase id such as 2026-10-07-native-fixture') && `${String(record.id)}.json` !== name) check.add('id', `must match the filename ${String(name)}`)
  if (typeof record.id === 'string' && record.id.length > 80) check.add('id', 'must be at most 80 characters')
  if (check.oneOf('scope', record.scope, SCOPES) && `scope-${String(record.scope)}` !== directory) check.add('scope', `must match the directory ${String(directory)}`)
  if (record.status === 'complete') check.add('status', 'complete is unsupported: a record holds evidence, and scope acceptance is the roadmap\'s separate decision')
  else check.oneOf('status', record.status, STATUSES)
  if (!validDate(record.recordedOn)) check.add('recordedOn', 'must be a calendar date YYYY-MM-DD')
  check.text('summary', record.summary)
  if (!Array.isArray(record.supersedes)) check.add('supersedes', 'must be an array of record ids')
  else {
    record.supersedes.forEach((id, index) => check.match(`supersedes[${index}]`, id, ID, 'a record id'))
    if (new Set(record.supersedes).size !== record.supersedes.length) check.add('supersedes', 'names a record more than once')
    if (record.supersedes.includes(record.id)) check.add('supersedes', 'names the record itself')
  }

  if (check.shape('provenance', record.provenance, ['base', 'candidate', 'oracle'])) {
    const provenance = record.provenance as Fields
    check.match('provenance.base', provenance.base, SHA40, 'a lowercase 40-hex commit')
    check.match('provenance.candidate', provenance.candidate, SHA40, 'a lowercase 40-hex commit')
    if (!check.absence('provenance.oracle', provenance.oracle, ['missing', 'notApplicable'])) check.match('provenance.oracle', provenance.oracle, SHA40, 'a lowercase 40-hex commit or an explicit absence')
  }

  check.list('support', record.support, ['missing', 'notApplicable'], (path, item) => {
    if (!check.shape(path, item, ['id', 'reference'])) return
    check.reference(`${path}.reference`, item.reference)
  })
  check.ids('support', record.support)

  if (!check.absence('fixtures', record.fixtures, ['missing', 'notApplicable']) && check.shape('fixtures', record.fixtures, ['files', 'digest'])) fixtureProblems(check, record.fixtures as Fields)

  const commandIds = check.ids('commands', record.commands)
  const commands = new Map<string, Fields>()
  if (Array.isArray(record.commands)) for (const item of record.commands) if (isObject(item) && typeof item.id === 'string') commands.set(item.id, item)
  /** Reports dangling command ids and returns the commands that resolve. */
  const cites = (path: string, value: unknown): [string, Fields][] => {
    if (!Array.isArray(value)) {
      check.add(path, 'must be an array of command ids')
      return []
    }
    const cited: [string, Fields][] = []
    value.forEach((id, index) => {
      const command = typeof id === 'string' && commandIds.has(id) ? commands.get(id) : undefined
      if (command === undefined) check.add(`${path}[${index}]`, `names no command in this record: ${JSON.stringify(id)}`)
      else cited.push([id as string, command])
    })
    return cited
  }

  check.list('artifacts', record.artifacts, ['missing', 'notApplicable'], (path, item) => {
    if (!check.shape(path, item, ['id', 'description', 'sha256', 'command'])) return
    check.text(`${path}.description`, item.description)
    if (!check.absence(`${path}.sha256`, item.sha256, ['missing'])) check.match(`${path}.sha256`, item.sha256, SHA64, 'a lowercase 64-hex SHA-256 or {"missing": reason}')
    if (typeof item.command !== 'string' || !commandIds.has(item.command)) check.add(`${path}.command`, `names no command in this record: ${JSON.stringify(item.command)}`)
  })
  check.ids('artifacts', record.artifacts)

  check.list('commands', record.commands, ['missing'], (path, item) => commandProblems(check, path, item))

  check.list('results', record.results, ['missing'], (path, item) => {
    if (!check.shape(path, item, ['id', 'behavior', 'owner', 'report', 'commands', 'outcome'], ['reason'])) return
    check.text(`${path}.behavior`, item.behavior)
    check.text(`${path}.owner`, item.owner)
    if (!check.absence(`${path}.report`, item.report, ['missing'])) check.reference(`${path}.report`, item.report)
    const cited = cites(`${path}.commands`, item.commands)
    check.oneOf(`${path}.outcome`, item.outcome, ['pass', 'fail', 'skip', 'missing'])
    if (item.outcome === 'skip' || item.outcome === 'missing' || Object.hasOwn(item, 'reason')) check.text(`${path}.reason`, item.reason)
    if (item.outcome === 'pass') {
      if (Array.isArray(item.commands) && item.commands.length === 0) check.add(`${path}.commands`, 'is empty; a pass result must cite the commands that observed it')
      for (const [id, command] of cited) {
        if (!observedExpectedExit(command)) check.add(path, `claims pass, but command ${id} did not observe its expected exit ${String(command.expectedExit)}`)
        else if (command.expectedExit === 0 && isObject(command.counts) && typeof command.counts.failed === 'number' && command.counts.failed > 0) {
          check.add(path, `claims pass, but command ${id} counts ${command.counts.failed} failed`)
        }
      }
    }
  })
  check.ids('results', record.results)

  check.list('negativeControls', record.negativeControls, ['missing', 'notApplicable'], (path, item) => {
    if (!check.shape(path, item, ['id', 'defect', 'assertion', 'observed', 'commands'], ['reason'])) return
    check.text(`${path}.defect`, item.defect)
    check.text(`${path}.assertion`, item.assertion)
    const cited = cites(`${path}.commands`, item.commands)
    check.oneOf(`${path}.observed`, item.observed, ['rejected', 'accepted', 'not-run'])
    if (item.observed === 'not-run' || Object.hasOwn(item, 'reason')) check.text(`${path}.reason`, item.reason)
    if (item.observed === 'rejected') {
      if (Array.isArray(item.commands) && item.commands.length === 0) check.add(`${path}.commands`, 'is empty; a rejected control must cite the commands that observed the rejection')
      for (const [id, command] of cited) {
        if (!observedExpectedExit(command)) check.add(path, `claims rejected, but command ${id} did not observe its expected exit ${String(command.expectedExit)}`)
      }
    }
  })
  check.ids('negativeControls', record.negativeControls)

  if (!Array.isArray(record.missingEvidence)) check.add('missingEvidence', 'must be an array')
  else record.missingEvidence.forEach((item, index) => {
    const path = `missingEvidence[${index}]`
    if (!check.shape(path, item, ['what', 'reason', 'owner'])) return
    check.text(`${path}.what`, item.what)
    check.text(`${path}.reason`, item.reason)
    check.text(`${path}.owner`, item.owner)
  })

  for (const key of ['evalRecords', 'performanceRecords']) check.list(key, record[key], ['missing', 'notApplicable'], (path, item) => check.reference(path, item))

  if (!check.absence('rollback', record.rollback, ['missing', 'notApplicable']) && check.shape('rollback', record.rollback, ['observed', 'commands'])) {
    const rollback = record.rollback as Fields
    check.text('rollback.observed', rollback.observed)
    cites('rollback.commands', rollback.commands)
  }

  if (!check.absence('review', record.review, ['missing']) && check.shape('review', record.review, ['reviewer', 'state', 'reference'])) {
    const review = record.review as Fields
    check.text('review.reviewer', review.reviewer)
    check.oneOf('review.state', review.state, ['requested', 'commented', 'changes-requested', 'approved'])
    check.reference('review.reference', review.reference)
  }
  return problems
}

function observedExpectedExit(command: Fields): boolean {
  return typeof command.actualExit === 'number' && command.actualExit === command.expectedExit
}

function fixtureProblems(check: RecordCheck, fixtures: Fields): void {
  const { files } = fixtures
  if (!Array.isArray(files) || files.length === 0) {
    check.add('fixtures.files', 'must be a non-empty array; use {"missing": reason} for the whole manifest instead')
    return
  }
  let wellFormed = true
  let previous: string | undefined
  files.forEach((entry, index) => {
    const path = `fixtures.files[${index}]`
    if (!check.shape(path, entry, ['path', 'role', 'sha256'])) {
      wellFormed = false
      return
    }
    if (!safePath(entry.path)) {
      check.add(`${path}.path`, 'must be a literal repository-relative file path, not a glob')
      wellFormed = false
    } else if (previous !== undefined && !(previous < (entry.path as string))) {
      check.add(`${path}.path`, `must sort strictly after ${previous}`)
      wellFormed = false
    }
    if (typeof entry.path === 'string') previous = entry.path
    check.oneOf(`${path}.role`, entry.role, ['shared-fixture-bytes', 'fixture-definition-source'])
    if (!check.match(`${path}.sha256`, entry.sha256, SHA64, 'a lowercase 64-hex SHA-256')) wellFormed = false
  })
  if (!check.match('fixtures.digest', fixtures.digest, SHA64, 'a lowercase 64-hex SHA-256') || !wellFormed) return
  const actual = manifestDigest(files as { path: string; sha256: string }[])
  if (actual !== fixtures.digest) check.add('fixtures.digest', `does not match its files: recorded ${String(fixtures.digest)}, computed ${actual}`)
}

function commandProblems(check: RecordCheck, path: string, item: unknown): void {
  if (!check.shape(path, item, ['id', 'argv', 'cwd', 'host', 'tools', 'testedCommit', 'expectedExit', 'actualExit', 'counts', 'evidence'])) return
  if (!Array.isArray(item.argv) || item.argv.length === 0 || !item.argv.every(arg => typeof arg === 'string')) check.add(`${path}.argv`, 'must be a non-empty array of strings')
  else if ((item.argv[0] as string).trim() === '') check.add(`${path}.argv[0]`, 'must name a program')
  if (!safePath(item.cwd, true)) check.add(`${path}.cwd`, 'must be . or a repository-relative directory')
  if (check.shape(`${path}.host`, item.host, ['os', 'arch', 'runner'])) {
    const host = item.host as Fields
    for (const key of ['os', 'arch', 'runner']) check.text(`${path}.host.${key}`, host[key])
  }
  if (!check.absence(`${path}.tools`, item.tools, ['missing'])) {
    if (!isObject(item.tools) || Object.keys(item.tools).length === 0) check.add(`${path}.tools`, 'must name at least one tool, or be {"missing": reason}')
    else for (const [tool, version] of Object.entries(item.tools).sort(([a], [b]) => (a < b ? -1 : 1))) {
      if (!check.absence(`${path}.tools.${tool}`, version, ['missing'])) check.text(`${path}.tools.${tool}`, version)
    }
  }
  if (!check.absence(`${path}.testedCommit`, item.testedCommit, ['missing'])) check.match(`${path}.testedCommit`, item.testedCommit, SHA40, 'a lowercase 40-hex commit or {"missing": reason}')
  check.count(`${path}.expectedExit`, item.expectedExit)
  if (!check.absence(`${path}.actualExit`, item.actualExit, ['missing'])) check.count(`${path}.actualExit`, item.actualExit)
  if (!check.absence(`${path}.counts`, item.counts, ['missing']) && check.shape(`${path}.counts`, item.counts, ['passed', 'failed', 'skipped', 'warned'])) {
    const counts = item.counts as Fields
    for (const key of ['passed', 'failed', 'skipped', 'warned']) check.count(`${path}.counts.${key}`, counts[key])
  }
  if (!check.absence(`${path}.evidence`, item.evidence, ['missing']) && check.shape(`${path}.evidence`, item.evidence, ['reference'], ['sha256'])) {
    const evidence = item.evidence as Fields
    check.reference(`${path}.evidence.reference`, evidence.reference)
    if (Object.hasOwn(evidence, 'sha256')) check.match(`${path}.evidence.sha256`, evidence.sha256, SHA64, 'a lowercase 64-hex SHA-256')
  }
}

/** Reads a regular file without following a final symlink, refusing oversized input. */
async function readRecordText(path: string): Promise<string | { problem: string }> {
  let handle
  try {
    handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0))
  } catch {
    return { problem: 'cannot be opened as a regular file' }
  }
  try {
    const stat = await handle.stat()
    if (!stat.isFile()) return { problem: 'is not a regular file' }
    if (stat.size > MAX_RECORD_BYTES) return { problem: `exceeds ${MAX_RECORD_BYTES} bytes` }
    const bytes = await handle.readFile()
    if (bytes.length > MAX_RECORD_BYTES) return { problem: `exceeds ${MAX_RECORD_BYTES} bytes` }
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    } catch {
      return { problem: 'is not valid UTF-8' }
    }
  } finally {
    await handle.close()
  }
}

async function entries(path: string): Promise<{ name: string; kind: 'file' | 'directory' | 'symlink' | 'other' }[]> {
  const list = await readdir(path, { withFileTypes: true })
  return list
    .map(entry => ({ name: entry.name, kind: entry.isSymbolicLink() ? 'symlink' as const : entry.isFile() ? 'file' as const : entry.isDirectory() ? 'directory' as const : 'other' as const }))
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
}

/**
 * Validate every record in a ledger directory. Read-only: no git, network, or writes.
 * @param directory - the ledger root holding `scope-NN/` directories and an optional `README.md`.
 * @returns valid records and sorted diagnostics. An empty ledger is a diagnostic.
 * @throws LedgerSetupError when the directory is missing, not a directory, or a symlink.
 */
export async function verifyLedger(directory: string): Promise<LedgerReport> {
  const root = resolve(directory)
  const stat = await lstat(root).catch(() => undefined)
  if (stat === undefined) throw new LedgerSetupError(`ledger ${directory} does not exist`)
  if (stat.isSymbolicLink()) throw new LedgerSetupError(`ledger ${directory} is a symlink; name the real directory`)
  if (!stat.isDirectory()) throw new LedgerSetupError(`ledger ${directory} is not a directory`)

  const problems: string[] = []
  const parsed: { path: string; record: Fields }[] = []
  for (const top of await entries(root)) {
    if (top.name === 'README.md' && top.kind === 'file') continue
    if (top.kind === 'symlink') {
      problems.push(`${top.name}: is a symlink; records and scope directories must be real`)
      continue
    }
    const scope = /^scope-(\d{2})$/.exec(top.name)?.[1]
    if (top.kind !== 'directory' || scope === undefined || !SCOPES.includes(scope)) {
      problems.push(`${top.name}: is not a scope-00 to scope-17 directory or the ledger README.md`)
      continue
    }
    const files = await entries(join(root, top.name))
    if (files.length === 0) problems.push(`${top.name}: holds no records`)
    for (const entry of files) {
      const file = `${top.name}/${entry.name}`
      if (entry.kind === 'symlink') problems.push(`${file}: is a symlink; records must be regular files`)
      else if (entry.kind !== 'file' || !entry.name.endsWith('.json')) problems.push(`${file}: is not a <id>.json record file`)
      else {
        const text = await readRecordText(join(root, file))
        if (typeof text !== 'string') {
          problems.push(`${file}: ${text.problem}`)
          continue
        }
        let record: unknown
        try {
          record = JSON.parse(text)
        } catch {
          problems.push(`${file}: is not valid JSON`)
          continue
        }
        problems.push(...recordProblems(file, record))
        if (isObject(record)) parsed.push({ path: file, record })
      }
    }
  }
  if (parsed.length === 0 && problems.length === 0) problems.push('(ledger) holds no records')
  problems.push(...supersedesProblems(parsed))
  const records = parsed
    .filter(entry => !problems.some(problem => problem.startsWith(`${entry.path}: `)))
    .map(entry => ({ path: entry.path, record: entry.record as unknown as QualificationAttempt }))
  return { records, problems: [...new Set(problems)].sort() }
}

/** Supersedes links name existing records in the same scope and never form a cycle. */
function supersedesProblems(parsed: readonly { path: string; record: Fields }[]): string[] {
  const problems: string[] = []
  const byId = new Map<string, { path: string; record: Fields }>()
  for (const entry of parsed) {
    if (typeof entry.record.id !== 'string') continue
    const other = byId.get(entry.record.id)
    if (other !== undefined) problems.push(`${entry.path}: id duplicates ${other.path}`)
    else byId.set(entry.record.id, entry)
  }
  const edges = new Map<string, string[]>()
  for (const [id, entry] of byId) {
    const targets = Array.isArray(entry.record.supersedes) ? entry.record.supersedes.filter((target): target is string => typeof target === 'string' && target !== id) : []
    for (const target of targets) {
      const other = byId.get(target)
      if (other === undefined) problems.push(`${entry.path}: supersedes names no record: ${target}`)
      else if (other.record.scope !== entry.record.scope) problems.push(`${entry.path}: supersedes ${target}, which is in another scope (${other.path})`)
    }
    edges.set(id, targets.filter(target => byId.has(target)).sort())
  }
  const state = new Map<string, 'active' | 'done'>()
  const cycles = new Set<string>()
  const visit = (id: string, stack: string[]): void => {
    state.set(id, 'active')
    stack.push(id)
    for (const next of edges.get(id) ?? []) {
      if (state.get(next) === 'active') {
        // Every member is reported, each from its own position, so none of them reads as valid.
        const cycle = stack.slice(stack.indexOf(next))
        cycle.forEach((member, index) => {
          const rotated = [...cycle.slice(index), ...cycle.slice(0, index), member]
          cycles.add(`${byId.get(member)?.path ?? member}: supersedes cycle ${rotated.join(' -> ')}`)
        })
      } else if (state.get(next) === undefined) visit(next, stack)
    }
    stack.pop()
    state.set(id, 'done')
  }
  for (const id of [...edges.keys()].sort()) if (state.get(id) === undefined) visit(id, [])
  return [...problems, ...cycles]
}

const USAGE = `Usage: bun scripts/rust-migration-ledger.ts --check [--ledger <directory>]

Validate the Rust migration's qualification-attempt records offline.

Options:
  --check               validate every scope-NN/<id>.json record
  --ledger <directory>  ledger root (default: docs/roadmap/rust-0.4/ledger in this checkout)
  --help                print this help

The check reads files only: no git, network, or writes. A valid record may
describe a partial or failed attempt; validity is not qualification success,
and scope acceptance remains the roadmap's separate decision.
Exit status: 0 when every record is valid; 1 when a record is invalid or the
ledger holds none; 2 on a usage error or an unusable ledger directory.`

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
  let check = false
  let help = false
  let ledger = DEFAULT_LEDGER
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]
    if (arg === '--check') check = true
    else if (arg === '--help' || arg === '-h') help = true
    else if (arg === '--ledger') {
      const value = args[++index]
      if (value === undefined || value.startsWith('--')) {
        output.error('rust-migration-ledger: --ledger needs a directory')
        return 2
      }
      ledger = value
    } else {
      output.error(`rust-migration-ledger: unknown argument ${String(arg)}; see --help`)
      return 2
    }
  }
  if (help) {
    output.log(USAGE)
    return 0
  }
  if (!check) {
    output.error(USAGE)
    return 2
  }
  let report: LedgerReport
  try {
    report = await verifyLedger(ledger)
  } catch (error) {
    if (!(error instanceof LedgerSetupError)) throw error
    output.error(`rust-migration-ledger: ${error.message}`)
    return 2
  }
  if (report.problems.length > 0) {
    for (const problem of report.problems) output.error(`rust-migration-ledger: ${problem}`)
    output.error(`rust-migration-ledger: ${report.problems.length} problem${report.problems.length === 1 ? '' : 's'}; records invalid`)
    return 1
  }
  for (const { path, record } of report.records) output.log(`${path}: ${record.status}`)
  output.log(`rust-migration-ledger: ${report.records.length} record${report.records.length === 1 ? '' : 's'} valid; record validity is not qualification success or scope acceptance`)
  return 0
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
