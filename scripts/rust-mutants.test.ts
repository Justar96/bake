import { afterEach, describe, expect, it } from 'bun:test'
import { chmodSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  changedRustFiles, CODEMODE_PACKAGE, createRepositoryMirror, defaultJobs, formatSummary, mergeSummaries,
  mutantsEnvironment, mutantsExitCode, type MutantsOptions, parseMutantsArgs, planMutantsRuns, readMutantsOutput,
  resolveMergeBase, SCRATCH_CARGO, SCRATCH_CARGO_NAME, usesDiff, workspaceDiff, workspacePath,
} from './rust-mutants.ts'
import { withoutRepositoryGitEnv } from './git-env.ts'

const REPO = '/repo'
const options = (overrides: Partial<MutantsOptions> = {}): MutantsOptions => ({
  base: undefined, files: [], packages: [], jobs: 2, inPlace: false, strict: false, list: false, ...overrides,
})

// A hook's GIT_DIR would otherwise point fixture commands at this repository.
const FIXTURE_GIT_ENV = {
  ...withoutRepositoryGitEnv(process.env),
  GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com',
}

const temporaryPaths: string[] = []
function temporaryDirectory(): string {
  const path = mkdtempSync(join(tmpdir(), 'rust-mutants-test-'))
  temporaryPaths.push(path)
  return path
}
afterEach(() => {
  for (const path of temporaryPaths.splice(0)) rmSync(path, { recursive: true, force: true })
})

function git(cwd: string, ...args: string[]): string {
  const result = Bun.spawnSync(['git', '-C', cwd, '-c', 'commit.gpgsign=false', ...args], { env: FIXTURE_GIT_ENV })
  if (result.exitCode !== 0) throw new Error(result.stderr.toString())
  return result.stdout.toString()
}

const gitRunner = (cwd: string) => (args: readonly string[]) => {
  const result = Bun.spawnSync(['git', '-C', cwd, ...args], { env: FIXTURE_GIT_ENV })
  return { code: result.exitCode, stdout: result.stdout.toString(), stderr: result.stderr.toString() }
}

describe('Rust mutation arguments', () => {
  it('maps repository, rust/, and absolute paths to rust/-relative form', () => {
    expect(workspacePath('rust/crates/bake-ai/src/types.rs', REPO)).toBe('crates/bake-ai/src/types.rs')
    expect(workspacePath('./rust/crates/bake-ai/src/types.rs', REPO)).toBe('crates/bake-ai/src/types.rs')
    expect(workspacePath('crates/bake-ai/src/types.rs', REPO)).toBe('crates/bake-ai/src/types.rs')
    expect(workspacePath('/repo/rust/crates/bake-ai/src/types.rs', REPO)).toBe('crates/bake-ai/src/types.rs')
  })

  it('keeps a few cold scratch copies by default and validates --jobs', () => {
    expect(defaultJobs(2)).toBe(1)
    expect(defaultJobs(16)).toBe(2)
    expect(defaultJobs(128)).toBe(4)
    expect(parseMutantsArgs([], 32, REPO).jobs).toBe(4)
    expect(parseMutantsArgs(['--jobs', '7'], 32, REPO).jobs).toBe(7)
    expect(() => parseMutantsArgs(['--jobs', '0'], 32, REPO)).toThrow('positive integer')
    expect(() => parseMutantsArgs(['--jobs', '2x'], 32, REPO)).toThrow('positive integer')
  })

  it('runs in place serially', () => {
    expect(parseMutantsArgs(['--in-place'], 32, REPO)).toMatchObject({ inPlace: true, jobs: 1 })
    expect(() => parseMutantsArgs(['--in-place', '--jobs', '2'], 32, REPO)).toThrow('--jobs 1')
  })

  it('collects files, packages, and flags, and rejects stray arguments', () => {
    const parsed = parseMutantsArgs(['--file', 'rust/crates/a/src/x.rs', '--file', 'crates/b/src/y.rs', '-p', 'bake-ai', '--strict', '--list', '--base', 'main'], 4, REPO)
    expect(parsed).toEqual({
      base: 'main', files: ['crates/a/src/x.rs', 'crates/b/src/y.rs'], packages: ['bake-ai'],
      jobs: 1, inPlace: false, strict: true, list: true,
    })
    expect(() => parseMutantsArgs(['crates/a.rs'], 4, REPO)).toThrow('unexpected argument')
  })

  it('follows a diff by default, and with files or packages only when given a base', () => {
    expect(usesDiff(options())).toBe(true)
    expect(usesDiff(options({ files: ['crates/a/src/x.rs'] }))).toBe(false)
    expect(usesDiff(options({ packages: ['bake-ai'] }))).toBe(false)
    expect(usesDiff(options({ packages: ['bake-ai'], base: 'develop' }))).toBe(true)
  })
})

describe('Rust mutation plan', () => {
  const out = '/out'

  it('reads the Rust files a diff leaves in the tree', () => {
    const diff = [
      'diff --git a/crates/a/src/x.rs b/crates/a/src/x.rs', '--- a/crates/a/src/x.rs', '+++ b/crates/a/src/x.rs',
      'diff --git a/README.md b/README.md', '--- a/README.md', '+++ b/README.md',
      'diff --git a/crates/a/src/gone.rs b/crates/a/src/gone.rs', '--- a/crates/a/src/gone.rs', '+++ /dev/null',
      '+++ b/crates/a/src/x.rs',
    ].join('\n')
    expect(changedRustFiles(diff)).toEqual(['crates/a/src/x.rs'])
  })

  it('tests codemode mutants with cargo test and every other package with nextest', () => {
    const runs = planMutantsRuns(options(), '/out/diff.patch', ['crates/bake-ai/src/x.rs', 'crates/bake-codemode/src/vm.rs'], out)
    expect(runs.map(run => run.name)).toEqual(['workspace', 'codemode'])
    const [workspace, codemode] = runs
    expect(workspace?.args).toEqual([
      'mutants', '--in-diff', '/out/diff.patch', '--jobs', '2', '--output', '/out/workspace',
      '--exclude', 'crates/bake-codemode/**', '--cargo-test-arg=--no-tests=pass',
    ])
    expect(codemode?.args).toEqual([
      'mutants', '--in-diff', '/out/diff.patch', '--jobs', '2', '--output', '/out/codemode',
      '--package', CODEMODE_PACKAGE, '--test-tool', 'cargo',
    ])
  })

  it('skips a run that has no changed files', () => {
    expect(planMutantsRuns(options(), '/d', ['crates/bake-ai/src/x.rs'], out).map(run => run.name)).toEqual(['workspace'])
    expect(planMutantsRuns(options(), '/d', ['crates/bake-codemode/src/vm.rs'], out).map(run => run.name)).toEqual(['codemode'])
  })

  it('routes files and packages to the run that tests them', () => {
    const files = planMutantsRuns(options({ files: ['crates/bake-tui-view/src/wheel.rs'] }), undefined, undefined, out)
    expect(files).toHaveLength(1)
    expect(files[0]?.args).toContain('crates/bake-tui-view/src/wheel.rs')
    expect(files[0]?.args).not.toContain('--in-diff')

    const codemode = planMutantsRuns(options({ files: ['crates/bake-codemode/src/vm.rs'] }), undefined, undefined, out)
    expect(codemode.map(run => run.name)).toEqual(['codemode'])
    expect(codemode[0]?.args).toContain('crates/bake-codemode/src/vm.rs')

    const glob = planMutantsRuns(options({ files: ['**/lib.rs'] }), undefined, undefined, out)
    expect(glob.map(run => run.name)).toEqual(['workspace', 'codemode'])

    const packages = planMutantsRuns(options({ packages: ['bake-ai', CODEMODE_PACKAGE] }), undefined, undefined, out)
    expect(packages[0]?.args).toEqual(expect.arrayContaining(['--package', 'bake-ai']))
    expect(packages[0]?.args).not.toContain(CODEMODE_PACKAGE)
    expect(packages[1]?.args).toEqual(expect.arrayContaining(['--package', CODEMODE_PACKAGE]))

    expect(planMutantsRuns(options({ packages: ['bake-ai'], files: ['crates/bake-codemode/src/vm.rs'] }), undefined, undefined, out)).toEqual([])
  })

  it('lists without output, jobs, or test arguments, and passes --in-place', () => {
    const [listed] = planMutantsRuns(options({ list: true, files: ['crates/a/src/x.rs'] }), undefined, undefined, out)
    expect(listed?.args).toEqual(['mutants', '--file', 'crates/a/src/x.rs', '--list', '--exclude', 'crates/bake-codemode/**'])
    const [inPlace] = planMutantsRuns(options({ inPlace: true, jobs: 1, files: ['crates/a/src/x.rs'] }), undefined, undefined, out)
    expect(inPlace?.args).toContain('--in-place')
  })
})

describe('Rust mutation results', () => {
  it('reads counts and named missed and timed-out mutants', () => {
    const dir = temporaryDirectory()
    writeFileSync(join(dir, 'outcomes.json'), JSON.stringify({ caught: 23, missed: 1, timeout: 1, unviable: 2, outcomes: [] }))
    writeFileSync(join(dir, 'missed.txt'), 'crates/a/src/x.rs:82:16: replace < with <= in f\n')
    writeFileSync(join(dir, 'timeout.txt'), 'crates/a/src/x.rs:9:5: replace g with ()\n')
    const summary = readMutantsOutput(dir)
    expect(summary).toEqual({
      caught: 23, missed: 1, timeout: 1, unviable: 2,
      missedMutants: ['rust/crates/a/src/x.rs:82:16: replace < with <= in f'],
      timeoutMutants: ['rust/crates/a/src/x.rs:9:5: replace g with ()'],
    })
    expect(formatSummary(summary)).toBe([
      'mutants:rust: 23 caught, 1 missed, 1 timeout, 2 unviable',
      'MISSED   rust/crates/a/src/x.rs:82:16: replace < with <= in f',
      'TIMEOUT  rust/crates/a/src/x.rs:9:5: replace g with ()',
    ].join('\n'))
  })

  it('treats a run without outcomes as empty and sums runs', () => {
    const empty = readMutantsOutput(join(temporaryDirectory(), 'missing'))
    expect(empty.caught + empty.missed + empty.timeout + empty.unviable).toBe(0)
    const one = { caught: 1, missed: 2, timeout: 0, unviable: 1, missedMutants: ['a', 'b'], timeoutMutants: [] }
    expect(mergeSummaries([one, one])).toEqual({ caught: 2, missed: 4, timeout: 0, unviable: 2, missedMutants: ['a', 'b', 'a', 'b'], timeoutMutants: [] })
  })

  it('fails on missed or timed-out mutants only when strict, and always on a failed run', () => {
    const missed = { caught: 1, missed: 1, timeout: 0, unviable: 0, missedMutants: ['a'], timeoutMutants: [] }
    const clean = { ...missed, missed: 0, missedMutants: [] }
    expect(mutantsExitCode([2], missed, false)).toBe(0)
    expect(mutantsExitCode([2], missed, true)).toBe(1)
    expect(mutantsExitCode([3], { ...clean, timeout: 1 }, true)).toBe(1)
    expect(mutantsExitCode([0, 0], clean, true)).toBe(0)
    expect(mutantsExitCode([0, 4], clean, false)).toBe(4)
  })
})

describe('Rust mutation scope and scratch copies', () => {
  it('diffs committed, unstaged, and untracked Rust changes since the merge-base', () => {
    const repo = temporaryDirectory()
    git(repo, 'init', '-q', '-b', 'develop')
    mkdirSync(join(repo, 'rust/crates/a/src'), { recursive: true })
    writeFileSync(join(repo, 'rust/crates/a/src/lib.rs'), 'fn a() {}\n')
    writeFileSync(join(repo, 'rust/crates/a/src/edited.rs'), 'fn e() {}\n')
    git(repo, 'add', '.')
    git(repo, 'commit', '-q', '-m', 'base')
    git(repo, 'checkout', '-q', '-b', 'topic')
    writeFileSync(join(repo, 'rust/crates/a/src/lib.rs'), 'fn a() { let _ = 1; }\n')
    git(repo, 'commit', '-q', '-am', 'topic')
    writeFileSync(join(repo, 'rust/crates/a/src/edited.rs'), 'fn e() { let _ = 2; }\n')
    writeFileSync(join(repo, 'rust/crates/a/src/new.rs'), 'fn n() {}\n')
    writeFileSync(join(repo, 'outside.rs'), 'fn o() {}\n')

    const rustGit = gitRunner(join(repo, 'rust'))
    const { base, mergeBase } = resolveMergeBase(rustGit, undefined)
    expect(base).toBe('develop')
    expect(mergeBase).toBe(git(repo, 'rev-parse', 'develop').trim())
    const diff = workspaceDiff(rustGit, mergeBase)
    expect(changedRustFiles(diff)).toEqual(['crates/a/src/edited.rs', 'crates/a/src/lib.rs', 'crates/a/src/new.rs'])
    expect(diff).toContain('+fn n() {}')
    expect(() => resolveMergeBase(rustGit, 'no-such-ref')).toThrow('pass --base')
  })

  it('mirrors the repository around rust/ without its Git directory', () => {
    const repo = temporaryDirectory()
    mkdirSync(join(repo, 'rust'))
    mkdirSync(join(repo, '.git'))
    mkdirSync(join(repo, 'distribution'))
    writeFileSync(join(repo, 'CHANGELOG.md'), 'changes\n')
    const mirror = createRepositoryMirror(repo, temporaryDirectory())
    expect(readdirSync(mirror).sort()).toEqual(['CHANGELOG.md', 'distribution'])
    expect(readFileSync(join(mirror, 'CHANGELOG.md'), 'utf8')).toBe('changes\n')
    expect(realpathSync(join(mirror, 'distribution'))).toBe(realpathSync(join(repo, 'distribution')))
  })

  it('gives builds and tests the caller\'s TMPDIR back in copy mode', () => {
    expect(mutantsEnvironment({ CARGO: '/x/cargo', TMPDIR: '/tmp', PATH: '/bin', UNSET: undefined }, undefined)).toEqual({ TMPDIR: '/tmp', PATH: '/bin' })
    expect(mutantsEnvironment({ CARGO: '/x/cargo', TMPDIR: '/tmp', PATH: '/bin' }, '/m')).toEqual({
      PATH: '/bin', TMPDIR: '/m', CARGO: `/m/${SCRATCH_CARGO_NAME}`, BAKE_MUTANTS_TMPDIR: '/tmp',
    })
    expect(mutantsEnvironment({ PATH: '/bin', BAKE_MUTANTS_TMPDIR: '/stale' }, '/m')).toEqual({ PATH: '/bin', TMPDIR: '/m', CARGO: `/m/${SCRATCH_CARGO_NAME}` })
  })

  it.skipIf(process.platform === 'win32')('runs the real cargo with the restored TMPDIR', () => {
    const dir = temporaryDirectory()
    const bin = join(dir, 'bin')
    mkdirSync(bin)
    writeFileSync(join(bin, 'cargo'), '#!/bin/sh\nprintf "%s|%s|%s" "${TMPDIR-unset}" "${BAKE_MUTANTS_TMPDIR-unset}" "$*"\n')
    chmodSync(join(bin, 'cargo'), 0o755)
    const wrapper = join(dir, SCRATCH_CARGO_NAME)
    writeFileSync(wrapper, SCRATCH_CARGO, { mode: 0o755 })
    const run = (env: Record<string, string>): string => Bun.spawnSync([wrapper, 'nextest', 'run'], { env: { PATH: `${bin}:/usr/bin:/bin`, ...env } }).stdout.toString()
    expect(run({ TMPDIR: '/mirror', BAKE_MUTANTS_TMPDIR: '/caller' })).toBe('/caller|unset|nextest run')
    expect(run({ TMPDIR: '/mirror' })).toBe('unset|unset|nextest run')
  })
})
