/**
 * The status line's git field reads the displayed workspace's branch and
 * changes in the background, follows a session into another workspace, and
 * stops with the application.
 */
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import type { Session } from 'bake-session'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { parseGitStatus, sessionGitConfinement, WorkspaceGit, type GitConfinement } from '../src/git.ts'

/** Whether bwrap can run here; hosts without user namespaces skip the real-confinement case. */
const bwrapUsable = spawnSync('bwrap', ['--ro-bind', '/', '/', '--dev', '/dev', '--proc', '/proc', '--', 'true'], { timeout: 5_000, stdio: 'ignore' }).status === 0

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** The inherited environment without git's own variables, which a hook may set, and with no user config. */
function isolated(home: string): Record<string, string | undefined> {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')))
  return { ...env, HOME: home, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(home, 'gitconfig') }
}

/** A repository on `main` with one commit, and a directory outside any repository. */
function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'bake-git-field-'))
  roots.push(root)
  const env = isolated(root)
  const repo = join(root, 'repo')
  const git = (...args: string[]): void => {
    execFileSync('git', ['-c', 'user.name=Bake', '-c', 'user.email=bake@example.test', ...args], { cwd: repo, env, stdio: 'ignore' })
  }
  execFileSync('git', ['init', '-q', '-b', 'main', repo], { env, stdio: 'ignore' })
  writeFileSync(join(repo, 'tracked.txt'), 'one\n')
  git('add', '.')
  git('commit', '-q', '-m', 'first')
  const outside = mkdtempSync(join(root, 'plain-'))
  // Keep the outside directory out of the repository search.
  return { root, repo, outside, env: { ...env, GIT_CEILING_DIRECTORIES: root }, git }
}

describe('parseGitStatus', () => {
  it('counts staged, unstaged, untracked, and conflicted paths and the distance from the upstream', () => {
    expect(parseGitStatus([
      '# branch.oid 0123456789abcdef0123456789abcdef01234567',
      '# branch.head feature/status',
      '# branch.upstream origin/feature/status',
      '# branch.ab +2 -1',
      '1 M. N... 100644 100644 100644 aaa bbb staged.ts',
      '1 .M N... 100644 100644 100644 aaa aaa modified.ts',
      '1 MM N... 100644 100644 100644 aaa bbb both.ts',
      '2 R. N... 100644 100644 100644 aaa bbb R100 new.ts\told.ts',
      'u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.ts',
      '? notes.md',
      '? scratch/',
      '',
    ].join('\n'))).toEqual({
      branch: 'feature/status', detached: false, ahead: 2, behind: 1,
      staged: 3, modified: 2, untracked: 2, conflicted: 1,
    })
  })

  it('names a detached HEAD by its abbreviated commit, and an unborn branch by its name', () => {
    expect(parseGitStatus('# branch.oid 0123456789abcdef\n# branch.head (detached)\n')).toMatchObject({ branch: '0123456', detached: true })
    expect(parseGitStatus('# branch.oid (initial)\n# branch.head main\n? a\n')).toMatchObject({ branch: 'main', detached: false, untracked: 1 })
    expect(parseGitStatus('')).toBeUndefined()
  })
})

describe('WorkspaceGit', () => {
  it('reads the followed workspace, sees its changes, and follows a session elsewhere', async () => {
    const { repo, outside, env, git } = fixture()
    const workspace = new WorkspaceGit({ env, pollMs: 20 })
    const abort = new AbortController()
    const onChange = vi.fn()
    workspace.start(abort.signal, onChange)
    try {
      expect(workspace.follow(repo)).toBeUndefined()
      await vi.waitFor(() => expect(workspace.follow(repo)).toEqual({
        branch: 'main', detached: false, ahead: 0, behind: 0, staged: 0, modified: 0, untracked: 0, conflicted: 0,
      }))
      expect(onChange).toHaveBeenCalled()

      writeFileSync(join(repo, 'tracked.txt'), 'two\n')
      writeFileSync(join(repo, 'new.txt'), 'new\n')
      await vi.waitFor(() => expect(workspace.follow(repo)).toMatchObject({ modified: 1, untracked: 1, staged: 0 }))
      git('add', 'new.txt')
      git('switch', '-q', '-c', 'topic')
      await vi.waitFor(() => expect(workspace.follow(repo)).toMatchObject({ branch: 'topic', staged: 1, modified: 1, untracked: 0 }))

      // A new workspace starts empty rather than borrowing the last one's answer.
      const reads = onChange.mock.calls.length
      expect(workspace.follow(outside)).toBeUndefined()
      await new Promise(resolve => setTimeout(resolve, 100))
      expect(workspace.follow(outside)).toBeUndefined()
      // Outside a repository there is nothing to name, so nothing repaints.
      expect(onChange).toHaveBeenCalledTimes(reads)
    } finally {
      abort.abort()
      await workspace.drain()
    }
    // Stopped: no late read reports anything.
    const calls = onChange.mock.calls.length
    writeFileSync(join(repo, 'later.txt'), 'later\n')
    await new Promise(resolve => setTimeout(resolve, 60))
    expect(onChange).toHaveBeenCalledTimes(calls)
  })

  it('reads a confined session through its wrapper, with the fsmonitor hook off', async () => {
    const { repo, env, git } = fixture()
    // A hook a sandboxed command could plant; the pass-through wrapper leaves only the flag to stop it.
    git('config', 'core.fsmonitor', `touch ${join(repo, 'fsmonitor-ran')}; false`)
    const wrapped: (readonly string[])[] = []
    const confinement: GitConfinement = { key: 'workspace-write', wrap: async argv => { wrapped.push(argv); return argv } }
    const workspace = new WorkspaceGit({ env, pollMs: 20 })
    const abort = new AbortController()
    workspace.start(abort.signal, () => {})
    try {
      workspace.follow(repo, confinement)
      await vi.waitFor(() => expect(workspace.follow(repo, confinement)?.branch).toBe('main'))
      expect(wrapped[0]).toEqual(['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
        'status', '--porcelain=v2', '--branch', '--untracked-files=normal'])
      expect(existsSync(join(repo, 'fsmonitor-ran'))).toBe(false)
    } finally {
      abort.abort()
      await workspace.drain()
    }
  })

  it.skipIf(process.platform === 'win32')('stops what git ran when a read is aborted', async () => {
    const { repo, env, git } = fixture()
    // A hook still running when the read stops must not write afterwards.
    git('config', 'core.fsmonitor', `touch ${join(repo, 'hook-started')}; sleep 0.4; touch ${join(repo, 'hook-finished')}; false`)
    const workspace = new WorkspaceGit({ env, pollMs: 10 })
    const abort = new AbortController()
    workspace.start(abort.signal, () => {})
    workspace.follow(repo)
    await vi.waitFor(() => expect(existsSync(join(repo, 'hook-started'))).toBe(true))
    abort.abort()
    await workspace.drain()
    await new Promise(resolve => setTimeout(resolve, 700))
    expect(existsSync(join(repo, 'hook-finished'))).toBe(false)
  })

  it('leaves the field empty rather than reading unconfined when confinement fails', async () => {
    const { repo, env, git } = fixture()
    git('config', 'core.fsmonitor', `touch ${join(repo, 'fsmonitor-ran')}; false`)
    let attempts = 0
    const unavailable: GitConfinement = { key: 'workspace-write', wrap: async () => { attempts++; throw new Error('no sandbox') } }
    const workspace = new WorkspaceGit({ env, pollMs: 10 })
    const abort = new AbortController()
    const onChange = vi.fn()
    workspace.start(abort.signal, onChange)
    try {
      workspace.follow(repo, unavailable)
      await vi.waitFor(() => expect(attempts).toBeGreaterThan(2))
      expect(workspace.follow(repo, unavailable)).toBeUndefined()
      expect(onChange).not.toHaveBeenCalled()
      expect(existsSync(join(repo, 'fsmonitor-ran'))).toBe(false)
      // The session left the sandbox: its reads run as before.
      await vi.waitFor(() => expect(workspace.follow(repo)?.branch).toBe('main'))
    } finally {
      abort.abort()
      await workspace.drain()
    }
  })

  it.skipIf(!bwrapUsable)('keeps a planted clean filter inside read-only confinement', async () => {
    const { repo, env, git } = fixture()
    // `git status` runs the clean filter of a changed file; core.fsmonitor=false does not stop it.
    git('config', 'filter.plant.clean', `sh -c 'touch ${join(repo, 'filter-ran')}; cat'`)
    writeFileSync(join(repo, '.gitattributes'), 'tracked.txt filter=plant\n')
    writeFileSync(join(repo, 'tracked.txt'), 'changed size\n')
    const readOnly: GitConfinement = {
      key: 'read-only',
      wrap: async argv => ['bwrap', '--ro-bind', '/', '/', '--dev', '/dev', '--proc', '/proc', '--die-with-parent', '--', ...argv],
    }
    const workspace = new WorkspaceGit({ env, pollMs: 20 })
    const abort = new AbortController()
    workspace.start(abort.signal, () => {})
    try {
      workspace.follow(repo, readOnly)
      await vi.waitFor(() => expect(workspace.follow(repo, readOnly)).toMatchObject({ branch: 'main', modified: 1 }))
      expect(existsSync(join(repo, 'filter-ran'))).toBe(false)
    } finally {
      abort.abort()
      await workspace.drain()
    }
  })

  it('stops at once when the application does, without waiting out the pause', async () => {
    const { repo, env } = fixture()
    const workspace = new WorkspaceGit({ env, pollMs: 60_000 })
    const abort = new AbortController()
    workspace.start(abort.signal, () => {})
    workspace.follow(repo)
    await vi.waitFor(() => expect(workspace.follow(repo)?.branch).toBe('main'))
    const started = performance.now()
    abort.abort()
    await workspace.drain()
    expect(performance.now() - started).toBeLessThan(1_000)
  })
})

describe('sessionGitConfinement', () => {
  const session = { id: 'session-1' } as unknown as Session
  function host(mode: string | undefined, sandbox?: { confine: (...args: unknown[]) => Promise<{ argv: string[] }> }): Context {
    return {
      get: (name: string) => name === 'sandboxPolicy'
        ? mode === undefined ? undefined : { resolve: () => ({ mode, workspaceRoot: '/work', sessionId: 'session-1' }) }
        : name === 'sandbox' ? sandbox : undefined,
    } as unknown as Context
  }

  it('leaves an unconfined session unwrapped', () => {
    expect(sessionGitConfinement(host(undefined), session)).toBeUndefined()
    expect(sessionGitConfinement(host('danger-full-access'), session)).toBeUndefined()
  })

  it('wraps a confined session read-only in its workspace, and refuses without a provider', async () => {
    const confine = vi.fn(async (argv: unknown) => ({ argv: ['runner', ...(argv as string[])] }))
    const signal = new AbortController().signal
    const confined = sessionGitConfinement(host('workspace-write', { confine }), session)
    await expect(confined?.wrap(['git', 'status'], signal)).resolves.toEqual(['runner', 'git', 'status'])
    expect(confine).toHaveBeenCalledWith(['git', 'status'], { mode: 'read-only', workspaceRoot: '/work', sessionId: 'session-1' }, signal)
    expect(sessionGitConfinement(host('read-only', { confine }), session)?.key).not.toBe(confined?.key)
    await expect(sessionGitConfinement(host('workspace-write'), session)?.wrap(['git'], signal)).rejects.toThrow('no sandbox provider')
  })
})
