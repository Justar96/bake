/**
 * The status line's git field: the session workspace's branch and uncommitted
 * changes, read with `git status` in the background for the life of the process.
 * @module @dsh-tui/app/git
 */

import { execFile } from 'node:child_process'
import type { GitState } from '@dsh-tui/ui/git.ts'

/** What the reads use besides the working tree; injected so tests own it. */
export interface WorkspaceGitOptions {
  /** Pause between reads. The agent's edits and commits show within one. */
  readonly pollMs?: number
  /** The environment `git` runs in. */
  readonly env?: Record<string, string | undefined>
  /** The executable to run. */
  readonly git?: string
}

/** Pause between reads. `git status` on a large tree still takes milliseconds. */
export const GIT_POLL_MS = 2_000
/** A read that takes longer is abandoned; the field keeps its last answer. */
const GIT_TIMEOUT_MS = 5_000

/**
 * Read `git status --porcelain=v2 --branch` into branch and change counts.
 * @param output - the command's standard output.
 * @returns the working tree's state, or undefined when the output names no branch.
 */
export function parseGitStatus(output: string): GitState | undefined {
  let head: string | undefined
  let oid = ''
  let ahead = 0
  let behind = 0
  let staged = 0
  let modified = 0
  let untracked = 0
  let conflicted = 0
  for (const line of output.split('\n')) {
    if (line.startsWith('# branch.oid ')) oid = line.slice('# branch.oid '.length)
    else if (line.startsWith('# branch.head ')) head = line.slice('# branch.head '.length)
    else if (line.startsWith('# branch.ab ')) {
      const [a, b] = line.slice('# branch.ab '.length).split(' ')
      ahead = Math.abs(Number.parseInt(a ?? '0', 10)) || 0
      behind = Math.abs(Number.parseInt(b ?? '0', 10)) || 0
    } else if (line.startsWith('1 ') || line.startsWith('2 ')) {
      // `XY`: the index's status, then the working tree's; `.` is unchanged.
      if (line[2] !== '.') staged++
      if (line[3] !== '.') modified++
    } else if (line.startsWith('u ')) conflicted++
    else if (line.startsWith('? ')) untracked++
  }
  if (head === undefined) return undefined
  const detached = head === '(detached)'
  return {
    branch: detached ? oid.slice(0, 7) : head, detached,
    ahead, behind, staged, modified, untracked, conflicted,
  }
}

/**
 * Same branch and counts. A read that finds nothing new does not repaint.
 * @param a - one state.
 * @param b - the other.
 * @returns whether they draw the same field.
 */
function same(a: GitState | undefined, b: GitState | undefined): boolean {
  if (a === undefined || b === undefined) return a === b
  return a.branch === b.branch && a.detached === b.detached && a.ahead === b.ahead && a.behind === b.behind
    && a.staged === b.staged && a.modified === b.modified && a.untracked === b.untracked && a.conflicted === b.conflicted
}

/**
 * The branch and changes of the displayed session's workspace.
 *
 * One read at a time, a pause after each, so a slow tree never stacks
 * processes. `--no-optional-locks` keeps the read from taking the index lock,
 * which would make a `git commit` the agent runs at the same moment fail.
 * Outside a repository, or without git, there is no field, and the reads go
 * on, so a `git init` shows up.
 */
export class WorkspaceGit {
  private cwd: string | undefined
  private state: GitState | undefined
  private work: Promise<void> | undefined
  private wake: (() => void) | undefined
  private changed: () => void = () => {}

  constructor(private readonly options: WorkspaceGitOptions = {}) {}

  /**
   * Keep reading the followed workspace until `signal` aborts.
   * @param signal - the application's lifetime; aborting it stops the reads.
   * @param onChange - called when the branch or a count changes.
   */
  start(signal: AbortSignal, onChange: () => void): void {
    this.changed = onChange
    const stop = (): void => { this.wake?.() }
    signal.addEventListener('abort', stop, { once: true })
    this.work = (async () => {
      try {
        while (!signal.aborted) {
          const cwd = this.cwd
          if (cwd !== undefined) {
            const next = await this.read(cwd, signal)
            // The session may have moved to another workspace while this ran.
            if (!signal.aborted && cwd === this.cwd && !same(next, this.state)) {
              this.state = next
              this.changed()
            }
          }
          if (signal.aborted) return
          // Followed elsewhere mid-read: read the new workspace now.
          if (cwd !== this.cwd) continue
          await new Promise<void>(resolve => {
            const timer = setTimeout(resolve, this.options.pollMs ?? GIT_POLL_MS)
            timer.unref()
            this.wake = () => { clearTimeout(timer); resolve() }
          })
          this.wake = undefined
        }
      } finally {
        signal.removeEventListener('abort', stop)
      }
    })()
  }

  /**
   * The state of `cwd`, read again at once when it is not the workspace
   * followed until now.
   * @param cwd - the displayed session's working directory.
   * @returns what the last read of `cwd` found, or undefined before one has.
   */
  follow(cwd: string): GitState | undefined {
    if (cwd !== this.cwd) {
      this.cwd = cwd
      this.state = undefined
      this.wake?.()
    }
    return this.state
  }

  /** @returns once the reads have stopped, so teardown leaves no late callback. */
  async drain(): Promise<void> { await this.work }

  private read(cwd: string, signal: AbortSignal): Promise<GitState | undefined> {
    const env = { ...this.options.env ?? process.env, GIT_OPTIONAL_LOCKS: '0', LC_ALL: 'C' }
    return new Promise(resolve => {
      execFile(this.options.git ?? 'git', ['--no-optional-locks', 'status', '--porcelain=v2', '--branch', '--untracked-files=normal'], {
        cwd, env, signal, timeout: GIT_TIMEOUT_MS, maxBuffer: 16 * 1024 * 1024, windowsHide: true, encoding: 'utf8',
      }, (error, stdout) => {
        // A read that ran out of time says nothing about the tree, so the
        // field keeps its last answer. Not a repository or no git: no field.
        if (error !== null && error.killed === true && !signal.aborted) resolve(this.state)
        else resolve(error === null ? parseGitStatus(stdout) : undefined)
      })
    })
  }
}
