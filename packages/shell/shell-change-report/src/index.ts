/**
 * The workspace files a shell command changed while it ran, read from git
 * before and after the command, for display only.
 *
 * Both reads compare the working tree with one copy of the index taken before
 * the command, so a commit, stash, or checkout the command makes cannot hide a
 * change. A file that was already dirty is compared by its `lstat` signature
 * and the bytes captured before the command. A file that was clean takes its
 * previous content from the index blob. Every phase has a time budget, and
 * running out of time degrades the report instead of the command.
 * @module bake-shell-change-report
 */

import { createHash } from 'node:crypto'
import { copyFile, lstat, mkdtemp, open, realpath, rm } from 'node:fs/promises'
import type { BigIntStats } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, relative, resolve } from 'node:path'
import { deadline, timeoutOf } from 'bake-timeout'
import { createGitRead, parseStatus } from './git.ts'
import type { ConfineArgv, GitRead, StatusEntry, StatusSnapshot } from './git.ts'
import { boundedHunks } from './hunks.ts'
import type { ChangeHunk } from './hunks.ts'

export { GIT_HARDENING, parseStatus } from './git.ts'
export type { ConfineArgv, StatusEntry, StatusSnapshot } from './git.ts'
export { boundedHunks } from './hunks.ts'
export type { ChangeHunk, FileHunks } from './hunks.ts'
export { openChangeReport, terminalChanges, withRunningJobs } from './tool.ts'

/** How one file changed while the command ran. */
export type ShellFileChangeStatus =
  | 'created' | 'modified' | 'deleted' | 'renamed' | 'mode' | 'symlink' | 'binary' | 'too-large' | 'unknown-before'

/** One changed file, with its path relative to the session's working directory. */
export type ShellFileChange = {
  path: string
  status: ShellFileChangeStatus
  /** A renamed file's previous path. */
  from?: string
  added: number
  removed: number
  /** Contextual hunks; absent past the report's bounds. */
  hunks?: ChangeHunk[]
}

/** The report a shell tool attaches to its result as `meta.shellChanges`. */
export type ShellChanges = {
  version: 1
  /** Changed files in path order. */
  files: ShellFileChange[]
  /** Changed files left out by the bounds. */
  omittedFiles?: number
  /** The comparison ran out of time; `files` may be incomplete. */
  timedOut?: true
  /** Another report's window overlapped this one in the same repository. */
  concurrent?: true
  /** HEAD moved during the command. */
  headChanged?: true
}

const FILE_STATUSES: ReadonlySet<string> = new Set<ShellFileChangeStatus>(
  ['created', 'modified', 'deleted', 'renamed', 'mode', 'symlink', 'binary', 'too-large', 'unknown-before'])

/**
 * Narrow a shell tool's logged `meta` to its change report. Malformed or
 * future-version metadata reads as no report, so replay never throws.
 * @param meta - a `tool/result.meta` value, live or replayed.
 * @returns the report under `meta.shellChanges`, or `undefined`.
 */
export function shellChangesOf(meta: unknown): ShellChanges | undefined {
  if (typeof meta !== 'object' || meta === null) return undefined
  const report = (meta as { shellChanges?: unknown }).shellChanges
  if (typeof report !== 'object' || report === null) return undefined
  const { version, files } = report as { version?: unknown; files?: unknown }
  if (version !== 1 || !Array.isArray(files) || files.length === 0 || !files.every(isFileChange)) return undefined
  return report as ShellChanges
}

function isFileChange(value: unknown): value is ShellFileChange {
  if (typeof value !== 'object' || value === null) return false
  const file = value as Record<string, unknown>
  return typeof file['path'] === 'string' && typeof file['status'] === 'string' && FILE_STATUSES.has(file['status'])
    && Number.isInteger(file['added']) && Number.isInteger(file['removed'])
    && (file['from'] === undefined || typeof file['from'] === 'string')
    && (file['hunks'] === undefined || (Array.isArray(file['hunks']) && file['hunks'].every(isHunk)))
}

function isHunk(value: unknown): value is ChangeHunk {
  if (typeof value !== 'object' || value === null) return false
  const hunk = value as Record<string, unknown>
  return (hunk['oldText'] === null || typeof hunk['oldText'] === 'string') && typeof hunk['newText'] === 'string'
    && Number.isInteger(hunk['oldStart']) && Number.isInteger(hunk['newStart'])
}

/** Budgets and bounds for one report. */
export interface ChangeReportLimits {
  /** Milliseconds for everything before the command runs. */
  beforeMs: number
  /** Milliseconds for the comparison after it. */
  afterMs: number
  /** Files that carry hunks; later files list their path and status only. */
  maxHunkFiles: number
  /** Files listed at all; the rest are counted in `omittedFiles`. */
  maxListedFiles: number
  /** Changed paths beyond this are listed from git's status alone, without reading content. */
  maxCandidates: number
  /** Already-dirty files whose bytes are captured before the command. */
  maxCapturedFiles: number
  maxCapturedBytes: number
  /** Largest file whose content is read or captured. */
  maxFileBytes: number
  /** Serialized hunk text per file; beyond it the file keeps its counts only. */
  maxHunkBytesPerFile: number
  /** The serialized report; hunks are dropped from the largest files first to fit. */
  maxReportBytes: number
  /** Milliseconds for one file's diff, and for all of them. */
  diffMsPerFile: number
  diffMsTotal: number
  /** A `git status` reply larger than this abandons the report. */
  statusMaxBytes: number
}

/** The default bounds. A rewrite of 10,000 lines stays within the diff budget by giving up its hunks. */
export const DEFAULT_CHANGE_REPORT_LIMITS: Readonly<ChangeReportLimits> = Object.freeze({
  // Two reads under macOS Seatbelt on a cold first call can exceed 200 ms.
  beforeMs: 500,
  afterMs: 750,
  maxHunkFiles: 20,
  maxListedFiles: 200,
  maxCandidates: 1_000,
  maxCapturedFiles: 256,
  maxCapturedBytes: 16 * 1024 * 1024,
  maxFileBytes: 1024 * 1024,
  maxHunkBytesPerFile: 64 * 1024,
  maxReportBytes: 256 * 1024,
  diffMsPerFile: 100,
  diffMsTotal: 400,
  statusMaxBytes: 8 * 1024 * 1024,
})

/** What a report needs from its caller. */
export interface ChangeReportOptions {
  /** Confinement for a sandboxed session; omitted for an unconfined one. */
  confine?: ConfineArgv
  /** Where the command runs. */
  workdir: string
  /** The session's working directory; reported paths are relative to it. */
  displayRoot: string
  /** The call's cancellation. */
  signal: AbortSignal
  limits?: Partial<ChangeReportLimits>
  /** The git executable. */
  git?: string
}

/** The open interval around one command. */
export interface ChangeWindow {
  /**
   * Compare the workspace with the state before the command, then release
   * the window. Never throws.
   * @param signal - the call's cancellation; the after budget also applies.
   * @returns the report, or `undefined` when nothing changed or the comparison failed.
   */
  finish(signal: AbortSignal): Promise<ShellChanges | undefined>
  /** Release the window without comparing, as an aborted command does. Idempotent. */
  release(): Promise<void>
}

/** Arguments for both status reads; submodules are left out so their own configuration never runs. */
const STATUS_ARGS = ['status', '--porcelain=v2', '--branch', '-z', '--untracked-files=all', '--no-renames', '--ignore-submodules=all']
const BEFORE_TIMEOUT = 'SHELL_CHANGE_BEFORE'
const AFTER_TIMEOUT = 'SHELL_CHANGE_AFTER'
/** Consecutive before-phase timeouts after which a workdir is skipped for a while. */
const SLOW_STRIKES = 2
const SLOW_PAUSE_MS = 10 * 60_000
/** Bytes sampled for a NUL that marks a file as binary. */
const BINARY_SAMPLE_BYTES = 8_192
const ZERO_OID = /^0+$/
/** Files signed or read at once while capturing the state before a command. */
const CAPTURE_BATCH = 32

/** A dirty path's state before the command. */
interface Captured {
  signature: string
  /** Its bytes, when it was a regular file within the capture bounds. */
  bytes?: Buffer
}

/** Live state of one open window, shared with overlapping windows of the same repository. */
interface WindowState {
  overlapped: boolean
}

/** Open windows per repository top level, so overlapping ones are marked. */
const openWindows = new Map<string, Set<WindowState>>()
/** Workdirs whose before phase timed out repeatedly, and when they may be tried again. */
const slowWorkdirs = new Map<string, { strikes: number; resumeAt: number }>()

/**
 * Take the before-command snapshot. Never throws: a workspace outside git, a
 * failed or slow read, or cancellation resolves `undefined`, and the command
 * runs without a report.
 * @param options - the call's confinement, directories, and cancellation.
 * @returns the open window, or `undefined` when no report can be made.
 */
export async function beginChangeReport(options: ChangeReportOptions): Promise<ChangeWindow | undefined> {
  const limits = { ...DEFAULT_CHANGE_REPORT_LIMITS, ...options.limits }
  const workdir = resolve(options.workdir)
  const slow = slowWorkdirs.get(workdir)
  if (slow !== undefined && slow.strikes >= SLOW_STRIKES && Date.now() < slow.resumeAt) return undefined
  const read = createGitRead(options.confine, options.git)
  using before = deadline(options.signal, limits.beforeMs, BEFORE_TIMEOUT)
  let temp: string | undefined
  try {
    const located = await read(['rev-parse', '--show-toplevel', '--git-path', 'index'], { cwd: workdir, signal: before.signal, maxBytes: 65_536 })
    const [top, indexPath] = located?.toString('utf8').split('\n') ?? []
    if (top === undefined || top === '' || indexPath === undefined || indexPath === '') {
      noteSlow(workdir, before.signal)
      return undefined
    }
    temp = await mkdtemp(join(tmpdir(), 'bake-shell-change-'))
    const index = join(temp, 'index')
    // A repository with no index yet reads as an empty one.
    await copyFile(resolve(workdir, indexPath), index).catch((error: unknown) => {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
    })
    const text = await read(STATUS_ARGS, { cwd: top, signal: before.signal, maxBytes: limits.statusMaxBytes, indexFile: index })
    if (text === undefined) {
      noteSlow(workdir, before.signal)
      await rm(temp, { recursive: true, force: true })
      return undefined
    }
    const status = parseStatus(text.toString('utf8'))
    const dirty = await captureDirty(top, status, limits, before.signal)
    slowWorkdirs.delete(workdir)
    // git reports the top level with symlinks resolved, as macOS's /var -> /private/var,
    // so the display root must be resolved the same way for relative paths to hold.
    const root = resolve(options.displayRoot)
    const displayRoot = await realpath(root).catch(() => root)
    return openWindow({ read, top, index, temp, status, dirty, limits, displayRoot })
  } catch {
    noteSlow(workdir, before.signal)
    if (temp !== undefined) await rm(temp, { recursive: true, force: true }).catch(() => {})
    return undefined
  }
}

/** Count a before-phase timeout against the workdir; other failures do not count. */
function noteSlow(workdir: string, signal: AbortSignal): void {
  if (timeoutOf(signal, BEFORE_TIMEOUT) === undefined) return
  const strikes = (slowWorkdirs.get(workdir)?.strikes ?? 0) + 1
  slowWorkdirs.set(workdir, { strikes, resumeAt: Date.now() + SLOW_PAUSE_MS })
}

/**
 * Record the `lstat` signature of every path that already differs from the
 * index, and capture the bytes of those within the bounds.
 */
async function captureDirty(
  top: string, status: StatusSnapshot, limits: ChangeReportLimits, signal: AbortSignal,
): Promise<Map<string, Captured>> {
  const dirty = new Map<string, Captured>()
  const paths = [...status.entries.values()].filter(entry => entry.worktree !== '.').map(entry => entry.path)
  // Signatures first, in batches, so the capture budget is spent in path order.
  const stats: (BigIntStats | undefined)[] = []
  for (let start = 0; start < paths.length; start += CAPTURE_BATCH) {
    signal.throwIfAborted()
    stats.push(...await Promise.all(paths.slice(start, start + CAPTURE_BATCH)
      .map(path => lstat(join(top, path), { bigint: true }).catch(() => undefined))))
  }
  let files = 0
  let bytes = 0
  const reads: { path: string; captured: Captured }[] = []
  for (const [position, path] of paths.entries()) {
    const stat = stats[position]
    const captured: Captured = { signature: signatureOf(stat) }
    dirty.set(path, captured)
    const size = stat === undefined ? 0 : Number(stat.size)
    const withinBudget = files < limits.maxCapturedFiles && bytes + size <= limits.maxCapturedBytes
    if (stat?.isFile() === true && size <= limits.maxFileBytes && withinBudget) {
      files++
      bytes += size
      reads.push({ path, captured })
    }
  }
  for (let start = 0; start < reads.length; start += CAPTURE_BATCH) {
    signal.throwIfAborted()
    await Promise.all(reads.slice(start, start + CAPTURE_BATCH).map(async ({ path, captured }) => {
      const content = await readBounded(join(top, path), limits.maxFileBytes)
      if (content !== undefined) captured.bytes = content
    }))
  }
  return dirty
}

/** A path's identity for change detection; `missing` for an absent path. */
function signatureOf(stats: BigIntStats | undefined): string {
  if (stats === undefined) return 'missing'
  return `${stats.dev}:${stats.ino}:${stats.size}:${stats.mtimeNs}:${stats.ctimeNs}:${stats.mode}`
}

/** Read a file's bytes, or `undefined` when it cannot be read or exceeds `maxBytes`. */
async function readBounded(path: string, maxBytes: number): Promise<Buffer | undefined> {
  let handle
  try {
    handle = await open(path, 'r')
    const buffer = Buffer.alloc(maxBytes + 1)
    let length = 0
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, length)
      if (bytesRead === 0) break
      length += bytesRead
    }
    return length > maxBytes ? undefined : buffer.subarray(0, length)
  } catch {
    return undefined
  } finally {
    await handle?.close().catch(() => {})
  }
}

/** Decode bytes as text, or `undefined` for binary content: a NUL near the start, or invalid UTF-8. */
function textOf(bytes: Buffer): string | undefined {
  if (bytes.subarray(0, BINARY_SAMPLE_BYTES).includes(0)) return undefined
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes).replace(/\r\n/g, '\n')
  } catch {
    return undefined
  }
}

/** Everything one open window holds. */
interface WindowInput {
  read: GitRead
  top: string
  index: string
  temp: string
  status: StatusSnapshot
  dirty: Map<string, Captured>
  limits: ChangeReportLimits
  displayRoot: string
}

function openWindow(input: WindowInput): ChangeWindow {
  const state: WindowState = { overlapped: false }
  let others = openWindows.get(input.top)
  if (others === undefined) openWindows.set(input.top, others = new Set())
  for (const other of others) other.overlapped = true
  if (others.size > 0) state.overlapped = true
  others.add(state)
  let released = false
  const release = async (): Promise<void> => {
    if (released) return
    released = true
    const open = openWindows.get(input.top)
    open?.delete(state)
    if (open?.size === 0) openWindows.delete(input.top)
    await rm(input.temp, { recursive: true, force: true }).catch(() => {})
  }
  return {
    release,
    async finish(signal) {
      if (released) return undefined
      try {
        using after = deadline(signal, input.limits.afterMs, AFTER_TIMEOUT)
        return await compare(input, state, after.signal)
      } catch {
        return undefined
      } finally {
        await release()
      }
    },
  }
}

/** One file's content on one side of the command. */
type Side = { kind: 'text'; text: string } | { kind: 'binary' } | { kind: 'too-large' } | { kind: 'absent' } | { kind: 'unknown' }

async function compare(input: WindowInput, state: WindowState, signal: AbortSignal): Promise<ShellChanges | undefined> {
  const { read, top, index, limits, dirty } = input
  const text = await read(STATUS_ARGS, { cwd: top, signal, maxBytes: limits.statusMaxBytes, indexFile: index })
  if (text === undefined) return undefined
  const now = parseStatus(text.toString('utf8'))
  const candidates = [...new Set([
    ...[...now.entries.values()].filter(entry => entry.worktree !== '.').map(entry => entry.path),
    ...dirty.keys(),
  ])].sort()
  const files: (ShellFileChange | undefined)[] = []
  /** Positions of deleted and created files by content hash, for pairing renames. */
  const deleted = new Map<string, number>()
  const created = new Map<string, number>()
  let timedOut = false
  let hunkFiles = 0
  const diffDeadline = performance.now() + limits.diffMsTotal
  const readContent = candidates.length <= limits.maxCandidates
  for (const path of candidates) {
    if (signal.aborted) { timedOut = true; break }
    const entry = now.entries.get(path)
    const prior = dirty.get(path)
    const absolute = join(top, path)
    const stats = await lstat(absolute, { bigint: true }).catch(() => undefined)
    // An already-dirty path whose signature held was not touched by the command.
    if (prior !== undefined && signatureOf(stats) === prior.signature) continue
    const display = relative(input.displayRoot, absolute).split('\\').join('/')
    const change = (status: ShellFileChangeStatus, extra: Partial<ShellFileChange> = {}): ShellFileChange =>
      ({ path: display, status, added: 0, removed: 0, ...extra })
    if (stats?.isSymbolicLink() === true) { files.push(change('symlink')); continue }
    if (stats !== undefined && !stats.isFile()) continue
    const listedOnly = !readContent || hunkFiles >= limits.maxHunkFiles
    if (listedOnly) {
      files.push(change(stats === undefined ? 'deleted' : prior === undefined && entry?.kind === '?' ? 'created' : 'modified'))
      continue
    }
    const beforeSide = await previousContent(read, top, entry, prior, limits, signal)
    const afterSide: Side = stats === undefined ? { kind: 'absent' }
      : Number(stats.size) > limits.maxFileBytes ? { kind: 'too-large' }
        : sideOf(await readBounded(absolute, limits.maxFileBytes))
    if (afterSide.kind === 'absent' && beforeSide.kind === 'absent') continue
    if (afterSide.kind === 'absent') {
      const lines = beforeSide.kind === 'text' ? lineCount(beforeSide.text) : 0
      if (beforeSide.kind === 'text' && beforeSide.text !== '') deleted.set(contentHash(beforeSide.text), files.length)
      files.push(change('deleted', { removed: lines }))
      continue
    }
    if (afterSide.kind === 'too-large' || beforeSide.kind === 'too-large') { files.push(change('too-large')); continue }
    if (afterSide.kind === 'binary' || beforeSide.kind === 'binary') { files.push(change('binary')); continue }
    if (afterSide.kind !== 'text') continue
    if (beforeSide.kind === 'unknown') { files.push(change('unknown-before')); continue }
    const previous = beforeSide.kind === 'text' ? beforeSide.text : ''
    // A new file is reported even when empty.
    if (beforeSide.kind === 'text' && previous === afterSide.text) {
      if (modeChanged(entry, prior, stats)) files.push(change('mode'))
      continue
    }
    const remaining = Math.min(limits.diffMsPerFile, diffDeadline - performance.now())
    const hunks = remaining > 0 ? boundedHunks(previous, afterSide.text, remaining) : undefined
    const status: ShellFileChangeStatus = beforeSide.kind === 'absent' ? 'created' : 'modified'
    if (status === 'created' && afterSide.text !== '') created.set(contentHash(afterSide.text), files.length)
    if (hunks === undefined) { files.push(change(status)); continue }
    hunkFiles++
    const fits = JSON.stringify(hunks.hunks).length <= limits.maxHunkBytesPerFile
    files.push(change(status, { added: hunks.added, removed: hunks.removed, ...fits ? { hunks: hunks.hunks } : {} }))
    if (signal.aborted) { timedOut = true; break }
  }
  pairRenames(files, deleted, created)
  const report: ShellChanges = {
    version: 1,
    files: files.filter((file): file is ShellFileChange => file !== undefined),
    ...timedOut ? { timedOut: true as const } : {},
    ...state.overlapped ? { concurrent: true as const } : {},
    ...input.status.head !== undefined && now.head !== undefined && input.status.head !== now.head ? { headChanged: true as const } : {},
  }
  if (report.files.length === 0 && report.timedOut !== true) return undefined
  return bounded(report, limits)
}

/** Read the content a file had before the command. */
async function previousContent(
  read: GitRead, top: string, entry: StatusEntry | undefined, prior: Captured | undefined,
  limits: ChangeReportLimits, signal: AbortSignal,
): Promise<Side> {
  if (prior !== undefined) {
    if (prior.signature === 'missing') return { kind: 'absent' }
    return prior.bytes === undefined ? { kind: 'unknown' } : sideOf(prior.bytes)
  }
  if (entry === undefined || entry.kind === '?') return { kind: 'absent' }
  if (entry.kind === 'u' || entry.indexBlob === undefined) return { kind: 'unknown' }
  if (ZERO_OID.test(entry.indexBlob)) return { kind: 'absent' }
  // `cat-file blob` applies no filters, so nothing the repository configures runs.
  const blob = await read(['cat-file', 'blob', entry.indexBlob], { cwd: top, signal, maxBytes: limits.maxFileBytes })
  return blob === undefined ? { kind: 'unknown' } : sideOf(blob)
}

function sideOf(bytes: Buffer | undefined): Side {
  if (bytes === undefined) return { kind: 'unknown' }
  const text = textOf(bytes)
  return text === undefined ? { kind: 'binary' } : { kind: 'text', text }
}

/** Whether only the mode distinguishes the file from its state before the command. */
function modeChanged(entry: StatusEntry | undefined, prior: Captured | undefined, stats: BigIntStats | undefined): boolean {
  if (prior !== undefined) return stats !== undefined && prior.signature.split(':').at(-1) !== String(stats.mode)
  return entry?.indexMode !== undefined && entry.worktreeMode !== undefined && entry.indexMode !== entry.worktreeMode
}

function lineCount(text: string): number {
  if (text === '') return 0
  return text.split('\n').length - (text.endsWith('\n') ? 1 : 0)
}

function contentHash(text: string): string {
  return createHash('sha256').update(text).digest('hex')
}

/**
 * Report a deleted file and a created file with identical content as one
 * rename of the deleted path to the created one.
 * @param files - the changes so far; a paired deletion is cleared to `undefined`.
 * @param deleted - deleted files' positions by content hash.
 * @param created - created files' positions by content hash.
 */
function pairRenames(files: (ShellFileChange | undefined)[], deleted: Map<string, number>, created: Map<string, number>): void {
  for (const [hash, from] of deleted) {
    const to = created.get(hash)
    const gone = files[from]
    const made = to === undefined ? undefined : files[to]
    if (to === undefined || gone === undefined || made === undefined) continue
    files[to] = { path: made.path, status: 'renamed', from: gone.path, added: 0, removed: 0 }
    files[from] = undefined
    created.delete(hash)
  }
}

/**
 * Apply the list and size bounds to the complete report: list at most
 * `maxListedFiles`, then drop hunks from the largest files until the
 * serialized report fits, then drop files.
 */
function bounded(report: ShellChanges, limits: ChangeReportLimits): ShellChanges {
  let omitted = Math.max(0, report.files.length - limits.maxListedFiles)
  const files = report.files.slice(0, limits.maxListedFiles)
  const size = (): number => JSON.stringify({ ...report, files, omittedFiles: omitted }).length
  while (size() > limits.maxReportBytes) {
    let largest = -1
    let largestSize = 0
    for (const [position, file] of files.entries()) {
      const hunkSize = file.hunks === undefined ? 0 : JSON.stringify(file.hunks).length
      if (hunkSize > largestSize) { largest = position; largestSize = hunkSize }
    }
    const target = largest >= 0 ? files[largest] : undefined
    if (target !== undefined) {
      const { hunks: _dropped, ...rest } = target
      files[largest] = rest
    } else {
      files.pop()
      omitted++
    }
  }
  return { ...report, files, ...omitted > 0 ? { omittedFiles: omitted } : {} }
}
