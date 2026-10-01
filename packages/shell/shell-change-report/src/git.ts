/**
 * Bounded, hardened `git` reads for a change report, and the parser for the
 * `status --porcelain=v2 -z` records they return.
 * @module @deepseek-ai/dsh-shell-change-report/git
 */

import { spawn } from 'node:child_process'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'

/**
 * Wrap a git argv for confinement. A sandboxed command can write `.git/config`
 * and `.gitattributes`, and `git status` runs a clean filter named there, so a
 * confined session's reads must run confined too.
 */
export type ConfineArgv = (argv: readonly string[], signal: AbortSignal) => Promise<readonly string[]>

/** One read's process options. */
export interface GitReadOptions {
  readonly cwd: string
  readonly signal: AbortSignal
  /** Stdout cap; a larger reply fails the read rather than returning a cut one. */
  readonly maxBytes: number
  /** The index file to read instead of the repository's own. */
  readonly indexFile?: string
}

/** Run one git read; resolves its stdout bytes, or `undefined` on any failure. */
export type GitRead = (args: readonly string[], options: GitReadOptions) => Promise<Buffer | undefined>

/**
 * Arguments that stop `git` from running a repository-local `core.fsmonitor`
 * hook and from taking optional locks. Clean filters cannot be switched off
 * this way, which is why confined sessions also confine the process.
 */
export const GIT_HARDENING = ['-c', 'core.fsmonitor=false', '--no-optional-locks'] as const

/** Variables that would point a read at another repository, index, or tree. */
const REDIRECTING_GIT_ENV: ReadonlySet<string> = new Set(['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY', 'GIT_COMMON_DIR'])

/**
 * Build the hardened git reader.
 *
 * Reads spawn `git` directly rather than through `ctx.subprocess`, whose Linux
 * containment starts each process in its own systemd scope: about 250 ms per
 * spawn, measured, against 2 to 15 ms for the read itself, and one report
 * makes several reads around every command. Each read still owns its process:
 * a POSIX child leads its own process group, which the deadline or the call's
 * cancellation kills, and the read settles only once the child has closed.
 * The environment is the shared credential scrub of the parent's.
 * @param confine - confinement for a sandboxed session, or `undefined` for an unconfined one.
 * @param git - the git executable.
 * @returns a reader that never throws: failures, cancellation, and cut output resolve `undefined`.
 */
export function createGitRead(confine: ConfineArgv | undefined, git = 'git'): GitRead {
  return async (args, options) => {
    if (options.signal.aborted) return undefined
    let argv: readonly string[] = [git, ...GIT_HARDENING, ...args]
    if (confine !== undefined) {
      try { argv = await confine(argv, options.signal) } catch { return undefined }
    }
    const env: NodeJS.ProcessEnv = {
      ...Object.fromEntries(Object.entries(scrubbedParentEnv()).filter(([name]) => !REDIRECTING_GIT_ENV.has(name))),
      GIT_OPTIONAL_LOCKS: '0', GIT_TERMINAL_PROMPT: '0', LC_ALL: 'C',
      ...options.indexFile === undefined ? {} : { GIT_INDEX_FILE: options.indexFile },
    }
    const finished = await runProcess(argv, { cwd: options.cwd, env, signal: options.signal, maxBytes: options.maxBytes })
    return finished?.code === 0 ? finished.stdout : undefined
  }
}

/**
 * Run one process to completion with bounded stdout.
 * @returns its exit code and stdout, or `undefined` when it failed to start,
 *   was cancelled, or wrote more than `maxBytes`.
 */
function runProcess(
  argv: readonly string[],
  options: { cwd: string; env: NodeJS.ProcessEnv; signal: AbortSignal; maxBytes: number },
): Promise<{ code: number | null; stdout: Buffer } | undefined> {
  const [program, ...args] = argv
  if (program === undefined) return Promise.resolve(undefined)
  const posix = process.platform !== 'win32'
  return new Promise((resolve) => {
    let child: ReturnType<typeof spawn>
    try {
      child = spawn(program, args, { cwd: options.cwd, env: options.env, stdio: ['ignore', 'pipe', 'ignore'], detached: posix, windowsHide: true })
    } catch {
      resolve(undefined)
      return
    }
    const chunks: Buffer[] = []
    let size = 0
    let failed = false
    let settled = false
    const kill = (): void => {
      failed = true
      try {
        if (posix && child.pid !== undefined) process.kill(-child.pid, 'SIGKILL')
        else child.kill('SIGKILL')
      } catch { /* already gone */ }
    }
    const settle = (code: number | null): void => {
      if (settled) return
      settled = true
      options.signal.removeEventListener('abort', kill)
      resolve(failed ? undefined : { code, stdout: Buffer.concat(chunks) })
    }
    options.signal.addEventListener('abort', kill, { once: true })
    child.stdout?.on('data', (chunk: Buffer) => {
      size += chunk.length
      if (size > options.maxBytes) kill()
      else chunks.push(chunk)
    })
    child.once('error', () => { failed = true; settle(null) })
    child.once('close', (code) => { settle(code) })
  })
}

/** One `status --porcelain=v2` entry the report reads. */
export interface StatusEntry {
  /** Path relative to the repository's top level. */
  readonly path: string
  /** `1` tracked, `u` unmerged, `?` untracked. */
  readonly kind: '1' | 'u' | '?'
  /** The working tree's status letter against the index; `?` for an untracked path. */
  readonly worktree: string
  /** The index blob of a tracked path: the content before the command for a file that was clean. */
  readonly indexBlob?: string
  /** The index and working-tree modes of a tracked path. */
  readonly indexMode?: string
  readonly worktreeMode?: string
}

/** A parsed `status --porcelain=v2 --branch -z` reply. */
export interface StatusSnapshot {
  /** The HEAD commit, or `(initial)` before the first commit. */
  readonly head: string | undefined
  readonly entries: ReadonlyMap<string, StatusEntry>
}

/**
 * Parse `git status --porcelain=v2 --branch -z`. Rename records (`2`) do not
 * occur with `--no-renames`; one that does is read for its new path.
 * Untracked directories (a nested repository, under `--untracked-files=all`)
 * are left out, because they have no file content to compare.
 * @param text - the command's stdout.
 * @returns the HEAD commit and the entries by path.
 */
export function parseStatus(text: string): StatusSnapshot {
  const records = text.split('\0')
  const entries = new Map<string, StatusEntry>()
  let head: string | undefined
  for (let index = 0; index < records.length; index++) {
    const record = records[index] ?? ''
    if (record.startsWith('# branch.oid ')) {
      head = record.slice('# branch.oid '.length)
    } else if (record.startsWith('1 ') || record.startsWith('2 ')) {
      // `1 XY sub mH mI mW hH hI path`; a `2` record adds a score field and a NUL-separated origin.
      const fields = record.split(' ')
      const fixed = record.startsWith('1 ') ? 8 : 9
      const path = fields.slice(fixed).join(' ')
      if (record.startsWith('2 ')) index++
      entries.set(path, {
        path, kind: '1', worktree: fields[1]?.[1] ?? '.',
        ...fields[7] === undefined ? {} : { indexBlob: fields[7] },
        ...fields[4] === undefined ? {} : { indexMode: fields[4] },
        ...fields[5] === undefined ? {} : { worktreeMode: fields[5] },
      })
    } else if (record.startsWith('u ')) {
      const path = record.split(' ').slice(10).join(' ')
      entries.set(path, { path, kind: 'u', worktree: record[3] ?? '.' })
    } else if (record.startsWith('? ')) {
      const path = record.slice(2)
      if (!path.endsWith('/')) entries.set(path, { path, kind: '?', worktree: '?' })
    }
  }
  return { head, entries }
}
