/**
 * The status line's git field: the session workspace's branch and uncommitted
 * changes, read with `git status` in the background for the life of the process.
 * @module @dsh-tui/app/git
 */

import { execFile } from 'node:child_process'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-sandbox'
import type {} from '@deepseek-ai/dsh-sandbox-policy'
import type { Session } from '@deepseek-ai/dsh-session'
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

/**
 * How the displayed session's reads are confined. A sandboxed session can
 * write `.git/config` and `.gitattributes`, and `git status` runs commands
 * named there: a `core.fsmonitor` hook, and a clean filter on a changed file.
 * Run unconfined, the poll would execute them outside the sandbox.
 */
export interface GitConfinement {
  /** Changes with the policy, so a changed policy is read again at once. */
  readonly key: string
  /**
   * Wrap a read for read-only confinement.
   * @param argv - `git` and its arguments.
   * @param signal - the read's cancellation.
   * @returns the argv to run instead; a rejection leaves the field empty rather than reading unconfined.
   */
  readonly wrap: (argv: readonly string[], signal: AbortSignal) => Promise<readonly string[]>
}

/**
 * The reads' confinement for one session. A session under
 * `danger-full-access`, or a composition with no sandbox policy, runs its
 * commands unconfined, so its reads cannot reach anything the agent could not
 * already. Any other mode reads under `read-only` confinement through the
 * host's sandbox provider, and without one the field stays empty.
 * @param ctx - the host context that owns the sandbox services.
 * @param session - the displayed session.
 * @returns the confinement, or undefined for an unconfined session.
 */
export function sessionGitConfinement(ctx: Context, session: Session): GitConfinement | undefined {
  const policy = ctx.get('sandboxPolicy')?.resolve({ session })
  if (policy === undefined || policy.mode === 'danger-full-access') return undefined
  const { workspaceRoot, sessionId } = policy
  return {
    key: [policy.mode, workspaceRoot, sessionId ?? ''].join('\0'),
    wrap: async (argv, signal) => {
      const sandbox = ctx.get('sandbox')
      if (sandbox === undefined) throw new Error('no sandbox provider can confine the status line\'s git read')
      const confined = await sandbox.confine(argv, {
        mode: 'read-only', workspaceRoot, ...sessionId === undefined ? {} : { sessionId },
      }, signal)
      return confined.argv
    },
  }
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
  private confinement: GitConfinement | undefined
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
          const confinement = this.confinement
          if (cwd !== undefined) {
            const next = await this.read(cwd, confinement, signal)
            // The session may have moved to another workspace, or policy, while this ran.
            if (!signal.aborted && cwd === this.cwd && confinement?.key === this.confinement?.key && !same(next, this.state)) {
              this.state = next
              this.changed()
            }
          }
          if (signal.aborted) return
          // Followed elsewhere mid-read: read the new workspace now.
          if (cwd !== this.cwd || confinement?.key !== this.confinement?.key) continue
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
   * The state of `cwd`, read again at once when it is not the workspace, or
   * the confinement, followed until now.
   * @param cwd - the displayed session's working directory.
   * @param confinement - how the displayed session's reads are confined; omitted for an unconfined session.
   * @returns what the last read of `cwd` found, or undefined before one has.
   */
  follow(cwd: string, confinement?: GitConfinement): GitState | undefined {
    if (cwd !== this.cwd || confinement?.key !== this.confinement?.key) {
      this.cwd = cwd
      this.state = undefined
      this.wake?.()
    }
    // The latest wrapper, so a read never runs under a policy the session left.
    this.confinement = confinement
    return this.state
  }

  /** @returns once the reads have stopped, so teardown leaves no late callback. */
  async drain(): Promise<void> { await this.work }

  private async read(cwd: string, confinement: GitConfinement | undefined, signal: AbortSignal): Promise<GitState | undefined> {
    const env = { ...this.options.env ?? process.env, GIT_OPTIONAL_LOCKS: '0', LC_ALL: 'C' }
    // Confinement is the defense; disabling the hook also spares the sandbox a pointless process.
    let argv: readonly string[] = [this.options.git ?? 'git', '--no-optional-locks',
      ...confinement === undefined ? [] : ['-c', 'core.fsmonitor=false'],
      'status', '--porcelain=v2', '--branch', '--untracked-files=normal']
    if (confinement !== undefined) {
      try { argv = await confinement.wrap(argv, signal) } catch { return undefined }
    }
    const [program, ...args] = argv
    if (program === undefined) return undefined
    return new Promise(resolve => {
      execFile(program, args, {
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
