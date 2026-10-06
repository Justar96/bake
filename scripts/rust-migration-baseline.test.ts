import { execFileSync, spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'bun:test'
import { withoutRepositoryGitEnv } from './git-env.ts'
import {
  AUTHORITATIVE_INPUTS,
  BaselineError,
  captureProvenance,
  DEFAULT_RECORD,
  KNOWN_ABSENCES,
  main,
  matchesPattern,
  provenanceShapeProblems,
  ROADMAP_BASELINE,
  runCheck,
  serialize,
  sourceDigest,
  verifyCheckResult,
  verifyProvenance,
  type KnownAbsence,
  type ProvenanceRecord,
  type ToolProbe,
} from './rust-migration-baseline.ts'

const SCRIPT = fileURLToPath(new URL('./rust-migration-baseline.ts', import.meta.url))
const REPO = dirname(dirname(SCRIPT))
const probe: ToolProbe = async tool => `${tool} 1.0.0`

const roots: string[] = []
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

function git(root: string, ...args: string[]): string {
  return execFileSync('git', ['-C', root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', ...args], {
    env: withoutRepositoryGitEnv(process.env),
    encoding: 'utf8',
  })
}

function write(root: string, path: string, content: string): void {
  mkdirSync(dirname(join(root, path)), { recursive: true })
  writeFileSync(join(root, path), content)
}

/** A committed repository with one file for every authoritative input pattern except `omit`. */
function fixture(omit: readonly string[] = []): string {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'bake-rust-baseline-')))
  roots.push(root)
  execFileSync('git', ['init', '--quiet', root], { env: withoutRepositoryGitEnv(process.env) })
  for (const spec of AUTHORITATIVE_INPUTS) {
    for (const pattern of spec.patterns.filter(pattern => !omit.includes(pattern))) write(root, pattern.replaceAll('**', 'fixture').replaceAll('*', 'fixture'), `${spec.id}: ${pattern}\n`)
  }
  write(root, 'package.json', '{ "name": "bake", "version": "0.3.8" }\n')
  write(root, 'apps/cli/package.json', '{ "name": "bake-cli", "version": "0.3.8" }\n')
  write(root, 'packages/core/session/src/types.ts', 'export const SESSION_FORMAT_VERSION = 3\n')
  write(root, 'bun.lock', '{ "lockfileVersion": 1 }\n')
  write(root, '.gitignore', '.preflight/\nlib/\n')
  write(root, 'README.md', 'fixture\n')
  git(root, 'add', '--all')
  git(root, 'commit', '--quiet', '-m', 'fixture')
  return root
}

function head(root: string): string {
  return git(root, 'rev-parse', 'HEAD').trim()
}

async function refusal(promise: Promise<unknown>): Promise<readonly string[]> {
  try {
    await promise
  } catch (error) {
    if (error instanceof BaselineError) return error.problems
    throw error
  }
  throw new Error('expected a BaselineError')
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function treePaths(revision: string): string[] {
  return execFileSync('git', ['-C', REPO, 'ls-tree', '-r', '-z', '--name-only', '--full-tree', revision], {
    env: { ...withoutRepositoryGitEnv(process.env), GIT_OPTIONAL_LOCKS: '0' },
    encoding: 'utf8',
    maxBuffer: 256 * 1024 * 1024,
  }).split('\0').filter(Boolean)
}

const MODEL_SURFACE = AUTHORITATIVE_INPUTS.find(spec => spec.id === 'model-surface')!.patterns
const RELEASE = ROADMAP_BASELINE.release.commit
// A shallow clone may not hold the release commit; the pinned-tree case then has nothing to read.
const releaseAvailable = spawnSync('git', ['-C', REPO, 'cat-file', '-e', `${RELEASE}^{commit}`], { env: withoutRepositoryGitEnv(process.env) }).status === 0

/** Authoritative input patterns that match no path of a tree. */
function unmatchedPatterns(tree: readonly string[]): string[] {
  return AUTHORITATIVE_INPUTS.flatMap(spec => spec.patterns.filter(pattern => !tree.some(path => matchesPattern(pattern, path))))
}

/** Absences of the model-surface patterns at a fixture's HEAD, in place of the pinned release commit. */
function absencesAt(commit: string, patterns: readonly string[] = MODEL_SURFACE): KnownAbsence[] {
  return patterns.map(pattern => ({ commit, input: 'model-surface', pattern, reason: 'fixture predates the snapshots' }))
}

describe('matchesPattern', () => {
  it('keeps * inside one segment and lets ** span segments', () => {
    expect(matchesPattern('packages/bundle/*/cordis.patch.yml', 'packages/bundle/base/cordis.patch.yml')).toBe(true)
    expect(matchesPattern('packages/bundle/*/cordis.patch.yml', 'packages/bundle/a/b/cordis.patch.yml')).toBe(false)
    expect(matchesPattern('docs/persistence-changes/**', 'docs/persistence-changes/a/b.md')).toBe(true)
    expect(matchesPattern('docs/persistence-changes/**', 'docs/persistence-changes')).toBe(false)
    expect(matchesPattern('a/*.md', 'a/x.mdx')).toBe(false)
    expect(matchesPattern('bun.lock', 'bun.lock')).toBe(true)
  })

  it('matches every authoritative input in this repository commit', () => {
    expect(unmatchedPatterns(treePaths('HEAD'))).toEqual([])
  })

  it.skipIf(!releaseAvailable)('matches every authoritative input at the pinned release except its known absences', () => {
    const exempted = KNOWN_ABSENCES.filter(absence => absence.commit === RELEASE).map(absence => absence.pattern)
    expect(unmatchedPatterns(treePaths(RELEASE))).toEqual(exempted)
  })
})

describe('KNOWN_ABSENCES', () => {
  it('exempts only the model-surface snapshots, only at the pinned release commit', () => {
    expect(KNOWN_ABSENCES.map(({ commit, input, pattern }) => ({ commit, input, pattern }))).toEqual(MODEL_SURFACE.map(pattern => ({ commit: RELEASE, input: 'model-surface', pattern })))
    for (const absence of KNOWN_ABSENCES) expect(absence.reason).toContain('predates the model-surface snapshots')
  })
})

describe('captureProvenance', () => {
  it('records the exact source deterministically and marks nothing qualified', async () => {
    const root = fixture()
    const first = await captureProvenance(root, { probe })
    const second = await captureProvenance(root, { probe })
    expect(serialize(second)).toBe(serialize(first))
    expect(first.source.commit).toBe(head(root))
    expect(first.source.product).toEqual({ version: '0.3.8', manifests: { 'package.json': '0.3.8', 'apps/cli/package.json': '0.3.8' } })
    expect(first.source.sessionFormatVersion).toBe(3)
    expect(first.source.lockfile.sha256).toBe(new Bun.CryptoHasher('sha256').update(readFileSync(join(root, 'bun.lock'))).digest('hex'))
    expect(first.sourceDigest).toBe(sourceDigest(first.source))
    expect(first.source.inputs.map(group => group.id)).toEqual(AUTHORITATIVE_INPUTS.map(spec => spec.id))
    expect(first.qualification).toBe('not-evaluated')
    expect(first.evidence).toEqual({ build: 'not-recorded', tests: 'not-recorded', performance: 'not-recorded' })
    expect(first.environment.tools).toEqual({ bun: 'bun 1.0.0', node: 'node 1.0.0', git: 'git 1.0.0' })
    const text = serialize(first)
    expect(text).not.toContain(root)
    expect(text).not.toContain(homedir())
  })

  it('ignores ignored build and log output', async () => {
    const root = fixture()
    const before = await captureProvenance(root, { probe })
    write(root, 'lib/index.js', 'built\n')
    write(root, '.preflight/build.log', 'log\n')
    expect((await captureProvenance(root, { probe })).sourceDigest).toBe(before.sourceDigest)
  })

  it('rejects a modified tracked file', async () => {
    const root = fixture()
    write(root, 'bun.lock', '{ "lockfileVersion": 2 }\n')
    expect(await refusal(captureProvenance(root, { probe }))).toContainEqual(expect.stringContaining('worktree is dirty: M bun.lock'))
  })

  it('rejects an untracked file', async () => {
    const root = fixture()
    write(root, 'notes.txt', 'stray\n')
    expect(await refusal(captureProvenance(root, { probe }))).toContainEqual(expect.stringContaining('worktree is dirty: ?? notes.txt'))
  })

  it('rejects a change hidden by assume-unchanged', async () => {
    const root = fixture()
    git(root, 'update-index', '--assume-unchanged', 'docs/tool-catalog.md')
    write(root, 'docs/tool-catalog.md', 'tampered\n')
    const problems = await refusal(captureProvenance(root, { probe }))
    expect(problems).toContainEqual(expect.stringContaining('assume-unchanged: docs/tool-catalog.md'))
    expect(problems).toContainEqual(expect.stringContaining('worktree content of docs/tool-catalog.md differs'))
  })

  it('digests committed bytes even when checkout rewrites line endings', async () => {
    const root = fixture()
    const committed = readFileSync(join(root, 'docs/tool-catalog.md'))
    const before = await captureProvenance(root, { probe })
    git(root, 'config', 'core.autocrlf', 'true')
    unlinkSync(join(root, 'docs/tool-catalog.md'))
    git(root, 'checkout', '--', 'docs/tool-catalog.md')
    expect(readFileSync(join(root, 'docs/tool-catalog.md'))).not.toEqual(committed)
    const after = await captureProvenance(root, { probe })
    expect(after.sourceDigest).toBe(before.sourceDigest)
    expect(await verifyProvenance(root, before)).toEqual([])
  })

  it.skipIf(process.platform === 'win32')('rejects a retargeted symlink hidden by assume-unchanged', async () => {
    const root = fixture()
    const link = 'packages/preset/agent-presets/presets/link'
    symlinkSync('fixture', join(root, link))
    git(root, 'add', link)
    git(root, 'commit', '--quiet', '-m', 'link')
    git(root, 'update-index', '--assume-unchanged', link)
    unlinkSync(join(root, link))
    symlinkSync('elsewhere', join(root, link))
    expect(await refusal(captureProvenance(root, { probe }))).toContainEqual(`worktree content of ${link} differs from ${head(root)}`)
  })

  it('rejects a commit missing an authoritative input', async () => {
    const root = fixture()
    git(root, 'rm', '--quiet', 'docs/config-catalog.md')
    git(root, 'commit', '--quiet', '-m', 'drop catalog')
    expect(await refusal(captureProvenance(root, { probe }))).toContainEqual(expect.stringContaining('missing authoritative input config-catalog'))
  })

  it('rejects disagreeing product versions and an unexpected commit', async () => {
    const root = fixture()
    expect(await refusal(captureProvenance(root, { probe, expectCommit: '0'.repeat(40) }))).toContainEqual(expect.stringContaining('not the expected'))
    write(root, 'apps/cli/package.json', '{ "version": "0.3.9" }\n')
    git(root, 'commit', '--quiet', '-am', 'skew')
    expect(await refusal(captureProvenance(root, { probe }))).toContainEqual(expect.stringContaining('product manifests disagree'))
  })

  it('rejects a root below the worktree top level', async () => {
    const root = fixture()
    expect(await refusal(captureProvenance(join(root, 'docs'), { probe }))).toContainEqual(expect.stringContaining('top level'))
  })
})

describe('known absences', () => {
  it('records an exempted absence and its reason as missing evidence, and verifies it', async () => {
    const root = fixture(MODEL_SURFACE)
    const absences = absencesAt(head(root))
    const record = await captureProvenance(root, { probe, absences })
    expect(record.source.absent).toEqual(MODEL_SURFACE.map(pattern => ({ input: 'model-surface', pattern, reason: 'fixture predates the snapshots' })))
    expect(record.source.inputs.find(group => group.id === 'model-surface')!.files).toEqual([])
    for (const pattern of MODEL_SURFACE) expect(record.missingEvidence).toContain(`model-surface: nothing matches ${pattern} at ${head(root)}; fixture predates the snapshots`)
    expect(provenanceShapeProblems(clone(record))).toEqual([])
    expect(await verifyProvenance(root, record, AUTHORITATIVE_INPUTS, absences)).toEqual([])
  })

  it('records no absence for a commit that has every input', async () => {
    const root = fixture()
    const record = await captureProvenance(root, { probe })
    expect(record.source.absent).toEqual([])
    expect(record.missingEvidence.some(item => item.startsWith('model-surface:'))).toBe(false)
  })

  it('refuses the same absence without an exemption for its exact commit', async () => {
    const root = fixture(MODEL_SURFACE)
    for (const absences of [KNOWN_ABSENCES, absencesAt('0'.repeat(40))]) {
      const problems = await refusal(captureProvenance(root, { probe, absences }))
      for (const pattern of MODEL_SURFACE) expect(problems).toContain(`missing authoritative input model-surface: nothing in ${head(root)} matches ${pattern}`)
    }
  })

  it('exempts only the named pattern, not the rest of its input or other inputs', async () => {
    const root = fixture([...MODEL_SURFACE, 'docs/config-catalog.md'])
    const problems = await refusal(captureProvenance(root, { probe, absences: absencesAt(head(root), MODEL_SURFACE.slice(0, 1)) }))
    expect(problems).toContain(`missing authoritative input model-surface: nothing in ${head(root)} matches ${MODEL_SURFACE[1]}`)
    expect(problems).toContainEqual(expect.stringContaining('missing authoritative input config-catalog'))
    expect(problems).not.toContainEqual(expect.stringContaining(MODEL_SURFACE[0]!))
  })

  it('refuses a stale absence for a commit that has the input, and one naming no input pattern', async () => {
    const root = fixture()
    expect(await refusal(captureProvenance(root, { probe, absences: absencesAt(head(root)) }))).toContain(`known absence is stale: ${head(root)} has 1 file(s) matching ${MODEL_SURFACE[0]} of model-surface`)
    const unknown = [{ commit: head(root), input: 'model-surface', pattern: 'elsewhere/*.md', reason: 'none' }]
    expect(await refusal(captureProvenance(root, { probe, absences: unknown }))).toContain('known absence names no authoritative input pattern: model-surface elsewhere/*.md')
  })

  it('rejects a record that claims an absence the worktree does not have', async () => {
    const root = fixture()
    type Group = { id: string; files: unknown[]; digest: string }
    type Editable = { source: Record<string, unknown> & { inputs: Group[] }; sourceDigest: string }
    const record = clone(await captureProvenance(root, { probe })) as unknown as Editable
    const group = record.source.inputs.find(item => item.id === 'model-surface')!
    group.files = []
    group.digest = new Bun.CryptoHasher('sha256').update('').digest('hex')
    record.source.absent = absencesAt(head(root)).map(({ input, pattern, reason }) => ({ input, pattern, reason }))
    record.sourceDigest = sourceDigest(record.source as never)
    const problems = await verifyProvenance(root, record)
    expect(problems).toContainEqual(expect.stringMatching(/^known absences: record /u))
    expect(problems).toContainEqual(expect.stringContaining('is present but not recorded'))
  })

  it('rejects an empty input group the record does not name absent', () => {
    const record = { source: { absent: [], inputs: [{ id: 'model-surface', digest: 'x', patterns: MODEL_SURFACE, files: [] }] } }
    expect(provenanceShapeProblems(record)).toContain('record.source.inputs[model-surface] has no files and no recorded absence')
    expect(provenanceShapeProblems({ source: { inputs: [] } })).toContain('record.source.absent is missing')
  })
})

describe('verifyProvenance', () => {
  it('accepts the worktree it was captured from', async () => {
    const root = fixture()
    expect(await verifyProvenance(root, await captureProvenance(root, { probe }))).toEqual([])
  })

  it('rejects a tampered file hash and a tampered source digest', async () => {
    const root = fixture()
    const record = await captureProvenance(root, { probe })
    type Mutable = { -readonly [K in keyof ProvenanceRecord]: ProvenanceRecord[K] }
      & { source: { inputs: { files: { sha256: string }[] }[] } }
    const tampered = clone(record) as Mutable
    tampered.source.inputs[3]!.files[0]!.sha256 = '0'.repeat(64)
    const problems = await verifyProvenance(root, tampered)
    expect(problems).toContain('record.sourceDigest does not match the recorded source')
    expect(problems).toContainEqual(expect.stringContaining(`input ${record.source.inputs[3]!.id}: recorded digest does not match`))
    expect(problems).toContainEqual(expect.stringContaining('differs (record sha256 0000'))

    const digest = clone(record) as unknown as Record<string, unknown>
    digest.sourceDigest = 'f'.repeat(64)
    expect(await verifyProvenance(root, digest)).toEqual(['record.sourceDigest does not match the recorded source'])
  })

  it('rejects a record of another commit', async () => {
    const root = fixture()
    const record = await captureProvenance(root, { probe })
    write(root, 'docs/tool-catalog.md', 'changed\n')
    git(root, 'commit', '--quiet', '-am', 'change tools')
    const problems = await verifyProvenance(root, record)
    expect(problems).toContainEqual(expect.stringMatching(/^commit: record /u))
    expect(problems).toContainEqual(expect.stringContaining('input tool-catalog: docs/tool-catalog.md differs'))
  })

  it('rejects a dirty worktree even when the record matches its commit', async () => {
    const root = fixture()
    const record = await captureProvenance(root, { probe })
    write(root, 'README.md', 'edited\n')
    expect(await verifyProvenance(root, record)).toContainEqual(expect.stringContaining('worktree is dirty: M README.md'))
  })

  it('rejects incomplete records and records that claim qualification', async () => {
    const root = fixture()
    const record = clone(await captureProvenance(root, { probe })) as unknown as Record<string, unknown>
    expect(await verifyProvenance(root, { ...record, qualification: 'passed' })).toContainEqual(expect.stringContaining('record.qualification must be "not-evaluated"'))
    expect(await verifyProvenance(root, { ...record, evidence: { build: 'passed', tests: 'not-recorded', performance: 'not-recorded' } })).toContainEqual(expect.stringContaining('record.evidence'))
    const { environment: _environment, ...withoutEnvironment } = record
    expect(await verifyProvenance(root, withoutEnvironment)).toContain('record.environment is missing')
    const source = record.source as Record<string, unknown>
    expect(await verifyProvenance(root, { ...record, source: { ...source, inputs: [] } })).toContain('record.source.inputs is missing')
    const lessInputs = { ...source, inputs: (source.inputs as unknown[]).slice(1) }
    expect(await verifyProvenance(root, { ...record, source: lessInputs, sourceDigest: sourceDigest(lessInputs as never) })).toContain('input lockfile: missing from the record')
  })
})

describe('runCheck', () => {
  async function verified(): Promise<{ root: string; record: ProvenanceRecord }> {
    const root = fixture()
    return { root, record: await captureProvenance(root, { probe }) }
  }

  it('records a passing command, and its result verifies against the record and log', async () => {
    const { root, record } = await verified()
    const out = join(root, '.preflight/rust-migration/echo.json')
    const { result, path } = await runCheck(root, record, { id: 'echo', argv: [process.execPath, '-e', `console.log(${JSON.stringify(root)})`], out, probe })
    expect(result.outcome).toBe('passed')
    expect(result.exitCode).toBe(0)
    expect(result.argv[0]).not.toContain(root)
    expect(result.argv[2]).toBe('console.log("<root>")')
    expect(readFileSync(join(root, '.preflight/rust-migration/echo.log'), 'utf8')).toContain(root)
    expect(await verifyCheckResult(path, JSON.parse(readFileSync(path, 'utf8')), record)).toEqual([])
  })

  it('records a failing exit as failed and rejects a result that claims otherwise', async () => {
    const { root, record } = await verified()
    const { result, path } = await runCheck(root, record, { id: 'fail', argv: [process.execPath, '-e', 'process.exit(3)'], out: join(root, '.preflight/fail.json'), probe })
    expect(result).toMatchObject({ outcome: 'failed', exitCode: 3, signal: null, timedOut: false, sourceUnchanged: true })
    const forged = { ...JSON.parse(readFileSync(path, 'utf8')), outcome: 'passed' }
    expect(await verifyCheckResult(path, forged, record)).toContainEqual(expect.stringContaining('contradicts its observations, which imply failed'))
  })

  it('marks a command that changes tracked source invalid', async () => {
    const { root, record } = await verified()
    const script = 'require(\'node:fs\').writeFileSync(\'README.md\', \'rewritten\\n\')'
    const { result } = await runCheck(root, record, { id: 'dirty', argv: [process.execPath, '-e', script], out: join(root, '.preflight/dirty.json'), probe })
    expect(result).toMatchObject({ exitCode: 0, sourceUnchanged: false, outcome: 'invalid' })
  })

  it('rejects a tampered or missing log', async () => {
    const { root, record } = await verified()
    const { path } = await runCheck(root, record, { id: 'log', argv: [process.execPath, '-e', 'console.log(1)'], out: join(root, '.preflight/log.json'), probe })
    const result = JSON.parse(readFileSync(path, 'utf8')) as unknown
    writeFileSync(join(root, '.preflight/log.log'), 'edited\n')
    expect(await verifyCheckResult(path, result, record)).toContainEqual(expect.stringContaining('does not match its recorded sha256'))
    unlinkSync(join(root, '.preflight/log.log'))
    expect(await verifyCheckResult(path, result, record)).toContainEqual(expect.stringContaining('log log.log is missing'))
  })

  it('rejects a result for another source', async () => {
    const { root, record } = await verified()
    const { path } = await runCheck(root, record, { id: 'other', argv: [process.execPath, '-e', ''], out: join(root, '.preflight/other.json'), probe })
    const other = { ...record, sourceDigest: 'a'.repeat(64) }
    expect(await verifyCheckResult(path, JSON.parse(readFileSync(path, 'utf8')), other)).toContainEqual(expect.stringContaining('different source'))
  })

  it('refuses to run against a record that does not verify, or into a tracked path', async () => {
    const { root, record } = await verified()
    const tampered = { ...record, sourceDigest: 'b'.repeat(64) }
    expect(await refusal(runCheck(root, tampered, { id: 'x', argv: [process.execPath, '-e', ''], out: join(root, '.preflight/x.json'), probe }))).toContain('record.sourceDigest does not match the recorded source')
    expect(await refusal(runCheck(root, record, { id: 'x', argv: [process.execPath, '-e', ''], out: join(root, 'docs/x.json'), probe }))).toContainEqual(expect.stringContaining('not ignored by git'))
    expect(await refusal(runCheck(root, record, { id: 'x', argv: [process.execPath, '-e', ''], out: join(root, '..x.json'), probe }))).toContainEqual(expect.stringContaining('output ..x.json is inside the worktree but not ignored'))
  })

  it('kills a command at its deadline and awaits it', async () => {
    const { root, record } = await verified()
    const { result } = await runCheck(root, record, {
      id: 'slow', argv: [process.execPath, '-e', 'setInterval(() => {}, 1000)'], out: join(root, '.preflight/slow.json'), timeoutMs: 300, killGraceMs: 1_000, probe,
    })
    expect(result).toMatchObject({ timedOut: true, outcome: 'failed' })
    expect(result.signal ?? result.exitCode).not.toBe(0)
  })

  it.skipIf(process.platform === 'win32')('kills descendants left in the command group and fails the result', async () => {
    const { root, record } = await verified()
    const script = 'const c = require(\'node:child_process\').spawn(process.execPath, [\'-e\', \'setTimeout(() => {}, 60000)\'], { stdio: \'ignore\' }); console.log(\'child \' + c.pid); c.unref()'
    const { result } = await runCheck(root, record, { id: 'orphan', argv: [process.execPath, '-e', script], out: join(root, '.preflight/orphan.json'), probe })
    expect(result).toMatchObject({ exitCode: 0, descendantsRemained: true, outcome: 'failed' })
    const pid = Number(/child (\d+)/u.exec(readFileSync(join(root, '.preflight/orphan.log'), 'utf8'))![1])
    const deadline = Date.now() + 10_000
    const alive = () => {
      try {
        process.kill(pid, 0)
        return true
      } catch {
        return false
      }
    }
    while (alive() && Date.now() < deadline) await Bun.sleep(20)
    expect(alive()).toBe(false)
  })
})

describe('main', () => {
  function output(): { lines: string[]; errors: string[]; log: (line: string) => void; error: (line: string) => void } {
    const lines: string[] = []
    const errors: string[] = []
    return { lines, errors, log: line => lines.push(line), error: line => errors.push(line) }
  }

  it('prints help from a real process', () => {
    const child = spawnSync(process.execPath, [SCRIPT, 'help'], { encoding: 'utf8', env: withoutRepositoryGitEnv(process.env), timeout: 60_000 })
    expect(child.signal).toBeNull()
    expect(child.status).toBe(0)
    expect(child.stdout).toContain('Usage: bun scripts/rust-migration-baseline.ts')
  })

  it('captures and checks through a real process with --root and --out', () => {
    const root = fixture()
    const out = join(root, '.preflight/baseline.json')
    const run = (...args: string[]) => spawnSync(process.execPath, [SCRIPT, ...args], { encoding: 'utf8', env: withoutRepositoryGitEnv(process.env), timeout: 60_000 })
    const captured = run('capture', '--root', root, '--out', out, '--expect-commit', head(root))
    expect(captured.signal).toBeNull()
    expect(captured.stderr).toBe('')
    expect(captured.status).toBe(0)
    expect(captured.stdout).toContain('qualification not evaluated')
    expect(captured.stdout).not.toContain(root)
    expect(run('capture', '--root', root, '--out', out).status).toBe(2)
    const checked = run('check', '--root', root, '--record', out)
    expect(checked.status).toBe(0)
    expect(checked.stdout).toContain(`source provenance verified: ${head(root)}`)
    expect(git(root, 'status', '--porcelain')).toBe('')
  })

  it('uses the default record path, rejects unignored output, and reports check failures', async () => {
    const root = fixture()
    const io = output()
    expect(await main(['capture', '--root', root], { ...io })).toBe(0)
    expect(JSON.parse(readFileSync(join(root, DEFAULT_RECORD), 'utf8'))).toMatchObject({ kind: 'source-provenance' })
    expect(await main(['check', '--root', root, '--expect-commit', '0'.repeat(40)], io)).toBe(2)
    expect(io.errors.at(-1)).toContain('not the expected')
    expect(await main(['capture', '--root', root, '--out', join(root, 'record.json')], io)).toBe(2)
    expect(io.errors.at(-1)).toContain('not ignored by git')
    expect(await main(['run', '--root', root, '--id', 'f', '--out', join(root, '.preflight/f.json'), '--', process.execPath, '-e', 'process.exit(1)'], io)).toBe(1)
    expect(await main(['check', '--root', root, '--result', join(root, '.preflight/f.json')], io)).toBe(1)
    expect(io.lines.at(-1)).toBe('check result f: failed')
    expect(await main(['run', '--root', root, '--id', 'p', '--out', join(root, '.preflight/p.json'), '--', process.execPath, '-e', ''], io)).toBe(0)
    expect(await main(['check', '--root', root, '--result', join(root, '.preflight/p.json')], io)).toBe(0)
    expect(await main(['--help'], io)).toBe(0)
    expect(io.lines.at(-1)).toContain('Usage:')
    expect(await main(['capture', '--root', root, '--bogus'], io)).toBe(2)
  })
})
