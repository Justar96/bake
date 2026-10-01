/**
 * Change reports against real git repositories through the real subprocess
 * runtime: what a command changed, what it did not, and how the report
 * degrades and stays hardened.
 */
import { execFileSync } from 'node:child_process'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { beginChangeReport, boundedHunks, parseStatus, type ChangeReportOptions, type ShellChanges } from '../src/index.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A repository with a committed `a.js`, `b.js`, and `docs/readme.md`, isolated from user and system git config. */
function repository() {
  const root = mkdtempSync(join(tmpdir(), 'bake-change-report-'))
  roots.push(root)
  const repo = join(root, 'repo')
  mkdirSync(join(repo, 'docs'), { recursive: true })
  const env = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_'))),
    HOME: root, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'gitconfig'), GIT_CEILING_DIRECTORIES: root }
  const git = (...args: string[]): void => {
    execFileSync('git', ['-c', 'user.name=Bake', '-c', 'user.email=bake@example.test', ...args], { cwd: repo, env, stdio: 'ignore' })
  }
  git('init', '-q', '-b', 'main')
  writeFileSync(join(repo, 'a.js'), 'const retries = 3\nconst delay = 10\nmodule.exports = { retries, delay }\n')
  writeFileSync(join(repo, 'b.js'), 'module.exports = 1\n')
  writeFileSync(join(repo, 'docs/readme.md'), '# Notes\n')
  writeFileSync(join(repo, '.gitignore'), 'build/\n')
  git('add', '.')
  git('commit', '-q', '-m', 'first')
  return { root, repo, git }
}

function options(workdir: string, extra: Partial<ChangeReportOptions> = {}): ChangeReportOptions {
  return { workdir, displayRoot: workdir, signal: new AbortController().signal, ...extra }
}

/** Run `command` (a function standing in for the shell command) inside a report window. */
async function report(workdir: string, command: () => void, extra: Partial<ChangeReportOptions> = {}): Promise<ShellChanges | undefined> {
  const window = await beginChangeReport(options(workdir, extra))
  expect(window).toBeDefined()
  command()
  return window!.finish(new AbortController().signal)
}

describe('beginChangeReport', () => {
  it('reports an in-place edit with its hunk, and nothing the command left alone', async () => {
    const { repo } = repository()
    // Dirty before the command and untouched by it: not part of the report.
    writeFileSync(join(repo, 'b.js'), 'module.exports = 2\n')
    const changes = await report(repo, () => {
      writeFileSync(join(repo, 'a.js'), 'const retries = 5\nconst delay = 10\nmodule.exports = { retries, delay }\n')
    })
    expect(changes).toEqual({
      version: 1,
      files: [{
        path: 'a.js', status: 'modified', added: 1, removed: 1,
        hunks: [{ oldText: 'const retries = 3\nconst delay = 10\nmodule.exports = { retries, delay }',
          newText: 'const retries = 5\nconst delay = 10\nmodule.exports = { retries, delay }', oldStart: 1, newStart: 1 }],
      }],
    })
  })

  it('reports paths relative to a session cwd reached through a symlink, as macOS temp dirs are', async () => {
    const { root, repo } = repository()
    const link = join(root, 'link')
    symlinkSync(repo, link, 'junction')
    const changes = await report(link, () => { writeFileSync(join(repo, 'b.js'), 'module.exports = 2\n') })
    expect(changes?.files.map(file => file.path)).toEqual(['b.js'])
  })

  it('compares a dirty file with its bytes before the command, not with the index', async () => {
    const { repo } = repository()
    writeFileSync(join(repo, 'b.js'), 'module.exports = 2\n')
    const changes = await report(repo, () => { writeFileSync(join(repo, 'b.js'), 'module.exports = 3\n') })
    expect(changes?.files).toEqual([{
      path: 'b.js', status: 'modified', added: 1, removed: 1,
      hunks: [{ oldText: 'module.exports = 2', newText: 'module.exports = 3', oldStart: 1, newStart: 1 }],
    }])
  })

  it('reports created, deleted, and renamed files, and skips ignored output and no-op rewrites', async () => {
    const { repo } = repository()
    const changes = await report(repo, () => {
      writeFileSync(join(repo, 'new.js'), 'export const fresh = true\n')
      unlinkSync(join(repo, 'b.js'))
      execFileSync('mv', [join(repo, 'docs/readme.md'), join(repo, 'docs/README.md')])
      mkdirSync(join(repo, 'build'))
      writeFileSync(join(repo, 'build/out.js'), 'ignored\n')
      // Rewritten with the same bytes: git compares content, so it is not a change.
      writeFileSync(join(repo, 'a.js'), 'const retries = 3\nconst delay = 10\nmodule.exports = { retries, delay }\n')
    })
    expect(changes?.files.map(({ path, status, from, added, removed }) => ({ path, status, from, added, removed }))).toEqual([
      { path: 'b.js', status: 'deleted', from: undefined, added: 0, removed: 1 },
      { path: 'docs/README.md', status: 'renamed', from: 'docs/readme.md', added: 0, removed: 0 },
      { path: 'new.js', status: 'created', from: undefined, added: 1, removed: 0 },
    ])
  })

  it('sees an edit the command also committed, and notes the moved HEAD', async () => {
    const { repo, git } = repository()
    const changes = await report(repo, () => {
      writeFileSync(join(repo, 'b.js'), 'module.exports = 9\n')
      git('commit', '-q', '-am', 'second')
    })
    expect(changes).toMatchObject({ headChanged: true, files: [{ path: 'b.js', status: 'modified', added: 1, removed: 1 }] })
  })

  it('reports paths relative to the session cwd from a subdirectory workdir, and binaries and modes by status', async () => {
    const { repo } = repository()
    const changes = await report(join(repo, 'docs'), () => {
      writeFileSync(join(repo, 'image.bin'), Buffer.from([0, 1, 2, 3]))
      chmodSync(join(repo, 'b.js'), 0o755)
    }, { displayRoot: join(repo, 'docs') })
    expect(changes?.files.map(({ path, status }) => ({ path, status }))).toEqual([
      { path: '../b.js', status: 'mode' },
      { path: '../image.bin', status: 'binary' },
    ])
  })

  it('returns nothing when the command changed nothing, and nothing outside a repository', async () => {
    const { root, repo } = repository()
    expect(await report(repo, () => {})).toBeUndefined()
    const plain = mkdtempSync(join(root, 'plain-'))
    expect(await beginChangeReport(options(plain))).toBeUndefined()
  })

  it('bounds the listed files and the hunk-bearing ones', async () => {
    const { repo } = repository()
    const changes = await report(repo, () => {
      for (let index = 0; index < 8; index++) writeFileSync(join(repo, `gen-${index}.txt`), `${index}\n`)
    }, { limits: { maxListedFiles: 5, maxHunkFiles: 2 } })
    expect(changes?.files).toHaveLength(5)
    expect(changes?.omittedFiles).toBe(3)
    expect(changes?.files.filter(file => file.hunks !== undefined)).toHaveLength(2)
  })

  it('drops hunks from the largest files to fit the report bound', async () => {
    const { repo } = repository()
    const changes = await report(repo, () => {
      writeFileSync(join(repo, 'large.txt'), 'x'.repeat(4_000) + '\n')
      writeFileSync(join(repo, 'small.txt'), 'y\n')
    }, { limits: { maxReportBytes: 2_000 } })
    const large = changes?.files.find(file => file.path === 'large.txt')
    const small = changes?.files.find(file => file.path === 'small.txt')
    expect(large).toMatchObject({ status: 'created', added: 1 })
    expect(large?.hunks).toBeUndefined()
    expect(small?.hunks).toHaveLength(1)
    expect(JSON.stringify(changes).length).toBeLessThanOrEqual(2_000)
  })

  it('never runs a planted fsmonitor hook, and writes nothing under .git', async () => {
    const { repo, git } = repository()
    git('config', 'core.fsmonitor', `touch ${join(repo, 'fsmonitor-ran')}; false`)
    const before = snapshotGitDir(join(repo, '.git'))
    const changes = await report(repo, () => { writeFileSync(join(repo, 'b.js'), 'module.exports = 4\n') })
    expect(changes?.files.map(file => file.path)).toEqual(['b.js'])
    expect(existsSync(join(repo, 'fsmonitor-ran'))).toBe(false)
    expect(snapshotGitDir(join(repo, '.git'))).toEqual(before)
  })

  it('routes every git read through the confinement wrapper, and gives up when it refuses', async () => {
    const { repo } = repository()
    const wrapped: string[][] = []
    const changes = await report(repo, () => { writeFileSync(join(repo, 'b.js'), 'module.exports = 5\n') }, {
      confine: async (argv) => { wrapped.push([...argv]); return argv },
    })
    expect(changes?.files.map(file => file.path)).toEqual(['b.js'])
    expect(wrapped.length).toBeGreaterThanOrEqual(3)
    for (const argv of wrapped) expect(argv.slice(0, 4)).toEqual(['git', '-c', 'core.fsmonitor=false', '--no-optional-locks'])
    await expect(beginChangeReport(options(repo, { confine: async () => { throw new Error('no sandbox') } }))).resolves.toBeUndefined()
  })

  it('marks overlapping windows in one repository as concurrent', async () => {
    const { repo } = repository()
    const first = await beginChangeReport(options(repo))
    const second = await beginChangeReport(options(repo))
    writeFileSync(join(repo, 'b.js'), 'module.exports = 6\n')
    const [one, two] = await Promise.all([first!.finish(new AbortController().signal), second!.finish(new AbortController().signal)])
    expect(one?.concurrent).toBe(true)
    expect(two?.concurrent).toBe(true)
    const alone = await report(repo, () => { writeFileSync(join(repo, 'b.js'), 'module.exports = 7\n') })
    expect(alone?.concurrent).toBeUndefined()
  })

  it('gives up before the command when the call is already cancelled, and releases a window without comparing', async () => {
    const { repo } = repository()
    const cancelled = new AbortController()
    cancelled.abort()
    expect(await beginChangeReport(options(repo, { signal: cancelled.signal }))).toBeUndefined()
    const temporary = (): Set<string> => new Set(readdirSync(tmpdir()).filter(name => name.startsWith('bake-shell-change-')))
    const earlier = temporary()
    const window = await beginChangeReport(options(repo))
    const opened = [...temporary()].filter(name => !earlier.has(name))
    expect(opened).toHaveLength(1)
    await window!.release()
    await window!.release()
    expect(await window!.finish(new AbortController().signal)).toBeUndefined()
    expect(temporary().has(opened[0]!)).toBe(false)
  })
})

describe('boundedHunks', () => {
  it('gives up on a large rewrite within its budget instead of blocking', () => {
    const before = Array.from({ length: 10_000 }, (_, index) => `line ${index} ${'a'.repeat(20)}`).join('\n')
    const after = Array.from({ length: 10_000 }, (_, index) => `other ${index * 7} ${'b'.repeat(20)}`).join('\n')
    const started = performance.now()
    expect(boundedHunks(before, after, 50)).toBeUndefined()
    expect(performance.now() - started).toBeLessThan(1_000)
  })

  it('counts added and removed lines and starts pure insertions with a null old side', () => {
    expect(boundedHunks('', 'one\ntwo\n', 100)).toEqual({ added: 2, removed: 0, hunks: [{ oldText: null, newText: 'one\ntwo', oldStart: 1, newStart: 1 }] })
  })
})

describe('parseStatus', () => {
  it('reads tracked, unmerged, and untracked records and skips untracked directories', () => {
    const status = parseStatus([
      '# branch.oid 0123abcd', '# branch.head main',
      '1 .M N... 100644 100644 100644 aaa bbb dir/with space.js',
      'u UU N... 100644 100644 100644 100644 h1 h2 h3 conflict.js',
      '? new.js', '? nested/', '',
    ].join('\0'))
    expect(status.head).toBe('0123abcd')
    expect([...status.entries.values()]).toEqual([
      { path: 'dir/with space.js', kind: '1', worktree: 'M', indexBlob: 'bbb', indexMode: '100644', worktreeMode: '100644' },
      { path: 'conflict.js', kind: 'u', worktree: 'U' },
      { path: 'new.js', kind: '?', worktree: '?' },
    ])
  })
})

/** Size and modification time of every file under a `.git` directory. */
function snapshotGitDir(dir: string): Record<string, string> {
  const out: Record<string, string> = {}
  const walk = (current: string): void => {
    for (const name of readdirSync(current)) {
      const path = join(current, name)
      const stats = statSync(path)
      if (stats.isDirectory()) walk(path)
      else out[path] = `${stats.size}:${stats.mtimeMs}`
    }
  }
  walk(dir)
  return out
}
