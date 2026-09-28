/**
 * The status line's git field reads the displayed workspace's branch and
 * changes in the background, follows a session into another workspace, and
 * stops with the application.
 */
import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { parseGitStatus, WorkspaceGit } from '../src/git.ts'

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
