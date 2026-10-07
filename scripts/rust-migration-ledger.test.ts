import { createHash } from 'node:crypto'
import { lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'bun:test'
import { LedgerSetupError, main, manifestDigest, MAX_RECORD_BYTES, verifyLedger } from './rust-migration-ledger.ts'

const SCRIPT = fileURLToPath(new URL('./rust-migration-ledger.ts', import.meta.url))
const sha = (text: string) => createHash('sha256').update(text).digest('hex')

const roots: string[] = []
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

function ledger(): string {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'bake-rust-ledger-')))
  roots.push(root)
  writeFileSync(join(root, 'README.md'), '# Ledger\n')
  return root
}

// oxlint-disable-next-line typescript/no-explicit-any -- Cases mutate arbitrary nested record fields to build invalid input.
type Json = Record<string, any>

function put(root: string, record: Json, file = `scope-${record.scope}/${record.id}.json`): string {
  mkdirSync(join(root, file, '..'), { recursive: true })
  writeFileSync(join(root, file), `${JSON.stringify(record, null, 2)}\n`)
  return file
}

/** A valid partial attempt whose negative-control command intentionally expects exit 1. */
function partial(id = 'partial-attempt', scope = '01'): Json {
  const files = [
    { path: 'conformance/fixtures/a/fixture.json', role: 'shared-fixture-bytes', sha256: sha('a') },
    { path: 'evals/agent-loop/native-fixture.ts', role: 'fixture-definition-source', sha256: sha('b') },
  ]
  return {
    schema: 'bake/rust-migration/qualification-attempt',
    version: 1,
    id,
    scope,
    status: 'partial',
    recordedOn: '2026-10-07',
    summary: 'Test attempt.',
    supersedes: [],
    provenance: { base: '1'.repeat(40), candidate: '2'.repeat(40), oracle: { missing: 'No frozen oracle selected.' } },
    support: { missing: 'Support entries not reviewed.' },
    fixtures: { files, digest: manifestDigest(files) },
    artifacts: [{ id: 'binary', description: 'Native binary', sha256: { missing: 'CI uploads artifacts only on failure.' }, command: 'ci' }],
    commands: [
      {
        id: 'ci',
        argv: ['bun', 'run', 'preflight'],
        cwd: '.',
        host: { os: 'linux', arch: 'x64', runner: 'ubuntu-latest' },
        tools: { bun: { missing: 'Not printed.' } },
        testedCommit: '3'.repeat(40),
        expectedExit: 0,
        actualExit: 0,
        counts: { passed: 4, failed: 0, skipped: 1, warned: 0 },
        evidence: { reference: 'https://example.invalid/runs/1' },
      },
      {
        id: 'tamper',
        argv: ['bun', 'test', 'x.test.ts'],
        cwd: 'scripts',
        host: { os: 'linux', arch: 'x64', runner: 'local' },
        tools: { missing: 'Not recorded.' },
        testedCommit: { missing: 'Isolated source copy.' },
        expectedExit: 1,
        actualExit: 1,
        counts: { passed: 3, failed: 1, skipped: 0, warned: 0 },
        evidence: { reference: '.preflight/ledger/tamper.log', sha256: sha('log') },
      },
    ],
    results: [
      { id: 'native', behavior: 'Native checks', owner: 'scripts/preflight.ts', report: { missing: 'Summary only.' }, commands: ['ci'], outcome: 'pass' },
      { id: 'conpty', behavior: 'Windows ConPTY', owner: 'unassigned', report: { missing: 'Not run.' }, commands: [], outcome: 'missing', reason: 'Skipped on win32.' },
    ],
    negativeControls: [{ id: 'tamper', defect: 'Remove the digest check', assertion: 'tamper test', observed: 'rejected', commands: ['tamper'] }],
    missingEvidence: [{ what: 'Live native arm', reason: 'Not implemented.', owner: 'unassigned' }],
    evalRecords: { notApplicable: 'No model-visible change.' },
    performanceRecords: { notApplicable: 'No runtime path.' },
    rollback: { notApplicable: 'Reads immutable inputs only.' },
    review: { missing: 'No review requested.' },
  }
}

function failed(id = 'failed-attempt', scope = '02'): Json {
  const record = partial(id, scope)
  record.status = 'failed'
  record.commands[0].actualExit = 1
  record.commands[0].counts = { passed: 3, failed: 1, skipped: 1, warned: 0 }
  record.results[0].outcome = 'fail'
  record.negativeControls[0].observed = 'accepted'
  record.review = { reviewer: 'reviewer', state: 'changes-requested', reference: 'https://example.invalid/pull/1#review' }
  return record
}

async function problems(root: string): Promise<string[]> {
  return [...(await verifyLedger(root)).problems]
}

function capture(): { lines: string[]; errors: string[]; output: { log: (line: string) => void; error: (line: string) => void } } {
  const lines: string[] = []
  const errors: string[] = []
  return { lines, errors, output: { log: line => lines.push(line), error: line => errors.push(line) } }
}

describe('verifyLedger', () => {
  it('accepts partial and failed attempts, including a superseding record and a control that expects exit 1', async () => {
    const root = ledger()
    put(root, partial())
    put(root, failed())
    put(root, { ...partial('partial-correction'), supersedes: ['partial-attempt'] })
    const report = await verifyLedger(root)
    expect(report.problems).toEqual([])
    expect(report.records.map(entry => `${entry.path} ${entry.record.status}`)).toEqual([
      'scope-01/partial-attempt.json partial',
      'scope-01/partial-correction.json partial',
      'scope-02/failed-attempt.json failed',
    ])
  })

  it('refuses a missing or symlinked ledger as setup and an empty ledger as invalid', async () => {
    const root = ledger()
    await expect(verifyLedger(join(root, 'absent'))).rejects.toBeInstanceOf(LedgerSetupError)
    expect(await problems(root)).toEqual(['(ledger) holds no records'])
    mkdirSync(join(root, 'scope-03'))
    expect(await problems(root)).toEqual(['scope-03: holds no records'])
    if (process.platform !== 'win32') {
      const link = join(ledger(), 'link')
      symlinkSync(root, link)
      await expect(verifyLedger(link)).rejects.toThrow('is a symlink')
    }
  })

  it('refuses a complete claim as unsupported', async () => {
    const root = ledger()
    put(root, { ...partial(), status: 'complete' })
    expect(await problems(root)).toEqual([expect.stringContaining('scope-01/partial-attempt.json: status complete is unsupported')])
  })

  it('reports malformed, dangling, and unexplained data', async () => {
    const cases: [string, (record: Json) => void, string][] = [
      ['uppercase commit', (record) => { record.provenance.candidate = 'A'.repeat(40) }, 'provenance.candidate must be a lowercase 40-hex commit'],
      ['short artifact digest', (record) => { record.artifacts[0].sha256 = 'abc' }, 'artifacts[0].sha256 must be a lowercase 64-hex'],
      ['unknown field', (record) => { record.commands[0].note = 'x' }, 'commands[0].note is not a recognized field'],
      ['absent field', (record) => { delete record.review }, 'review is required'],
      ['empty list', (record) => { record.negativeControls = [] }, 'negativeControls is empty; explain the omission'],
      ['blank reason', (record) => { record.support = { missing: ' ' } }, 'support.missing must be a non-blank string'],
      ['disallowed absence', (record) => { record.review = { notApplicable: 'x' } }, 'review cannot be notApplicable'],
      ['unexplained missing result', (record) => { delete record.results[1].reason }, 'results[1].reason must be a non-blank string'],
      ['dangling command', (record) => { record.results[0].commands = ['nope'] }, 'results[0].commands[0] names no command in this record: "nope"'],
      ['duplicate command id', (record) => { record.commands.push({ ...record.commands[0] }) }, 'commands[2].id duplicates ci'],
      ['glob path', (record) => { record.fixtures.files[0].path = 'conformance/fixtures/**' }, 'fixtures.files[0].path must be a literal repository-relative file path'],
      ['parent path', (record) => { record.commands[1].cwd = '../outside' }, 'commands[1].cwd must be . or a repository-relative directory'],
      ['id mismatch', (record) => { record.id = 'other' }, 'id must match the filename partial-attempt.json'],
      ['scope mismatch', (record) => { record.scope = '04' }, 'scope must match the directory scope-01'],
      ['bad date', (record) => { record.recordedOn = '2026-02-30' }, 'recordedOn must be a calendar date'],
    ]
    for (const [name, mutate, expected] of cases) {
      const root = ledger()
      const record = partial()
      mutate(record)
      put(root, record, 'scope-01/partial-attempt.json')
      expect({ name, problems: await problems(root) }).toEqual({ name, problems: [`scope-01/partial-attempt.json: ${expected}`].map(line => expect.stringContaining(line)) })
    }
  })

  it('refuses outcomes that contradict their own commands', async () => {
    const cases: [string, (record: Json) => void, string][] = [
      ['pass with an unexpected exit', (record) => { record.commands[0].actualExit = 1 }, 'results[0] claims pass, but command ci did not observe its expected exit 0'],
      ['pass with an unobserved exit', (record) => { record.commands[0].actualExit = { missing: 'log expired' } }, 'results[0] claims pass, but command ci did not observe'],
      ['pass with counted failures', (record) => { record.commands[0].counts.failed = 2 }, 'results[0] claims pass, but command ci counts 2 failed'],
      ['pass without commands', (record) => { record.results[0].commands = [] }, 'results[0].commands is empty; a pass result must cite'],
      ['rejected control whose command exited 0', (record) => { record.commands[1].actualExit = 0 }, 'negativeControls[0] claims rejected, but command tamper did not observe its expected exit 1'],
    ]
    for (const [name, mutate, expected] of cases) {
      const root = ledger()
      const record = partial()
      mutate(record)
      put(root, record)
      expect({ name, problems: await problems(root) }).toEqual({ name, problems: [expect.stringContaining(`scope-01/partial-attempt.json: ${expected}`)] })
    }
  })

  it('recomputes the manifest digest and requires sorted paths', async () => {
    const root = ledger()
    const tampered = partial()
    tampered.fixtures.files[1].sha256 = sha('changed')
    put(root, tampered)
    expect(await problems(root)).toEqual([expect.stringContaining('scope-01/partial-attempt.json: fixtures.digest does not match its files')])

    const unsorted = partial()
    unsorted.fixtures.files.reverse()
    unsorted.fixtures.digest = manifestDigest(unsorted.fixtures.files)
    put(root, unsorted)
    expect(await problems(root)).toEqual([expect.stringContaining('fixtures.files[1].path must sort strictly after evals/agent-loop/native-fixture.ts')])
  })

  it('requires supersedes links to exist in the same scope without cycles', async () => {
    const root = ledger()
    put(root, { ...partial('a'), supersedes: ['b'] })
    put(root, { ...partial('b'), supersedes: ['c'] })
    put(root, { ...partial('c'), supersedes: ['a'] })
    put(root, { ...partial('d'), supersedes: ['gone', 'x'] })
    put(root, partial('x', '05'))
    expect(await problems(root)).toEqual([
      'scope-01/a.json: supersedes cycle a -> b -> c -> a',
      'scope-01/b.json: supersedes cycle b -> c -> a -> b',
      'scope-01/c.json: supersedes cycle c -> a -> b -> c',
      'scope-01/d.json: supersedes names no record: gone',
      'scope-01/d.json: supersedes x, which is in another scope (scope-05/x.json)',
    ])
    expect((await verifyLedger(root)).records.map(entry => entry.path)).toEqual(['scope-05/x.json'])
  })

  it('reports a duplicate id across scopes', async () => {
    const root = ledger()
    put(root, partial('same', '01'))
    put(root, partial('same', '02'))
    expect(await problems(root)).toEqual(['scope-02/same.json: id duplicates scope-01/same.json'])
  })

  it('reads only regular files of bounded size in scope directories', async () => {
    const root = ledger()
    put(root, partial())
    writeFileSync(join(root, 'stray.json'), '{}')
    writeFileSync(join(root, 'scope-01', 'notes.txt'), 'x')
    mkdirSync(join(root, 'scope-18'))
    writeFileSync(join(root, 'scope-01', 'large.json'), ' '.repeat(MAX_RECORD_BYTES + 1))
    writeFileSync(join(root, 'scope-01', 'broken.json'), '{')
    const expected = [
      'scope-01/broken.json: is not valid JSON',
      `scope-01/large.json: exceeds ${MAX_RECORD_BYTES} bytes`,
      'scope-01/notes.txt: is not a <id>.json record file',
      'scope-18: is not a scope-00 to scope-17 directory or the ledger README.md',
      'stray.json: is not a scope-00 to scope-17 directory or the ledger README.md',
    ]
    if (process.platform !== 'win32') {
      // Symlinks point at valid content, so only the link itself can be the reason for rejection.
      symlinkSync(join(root, 'scope-01', 'partial-attempt.json'), join(root, 'scope-01', 'linked.json'))
      symlinkSync(join(root, 'scope-01'), join(root, 'scope-02'))
      expected.splice(2, 0, 'scope-01/linked.json: is a symlink; records must be regular files')
      expected.splice(4, 0, 'scope-02: is a symlink; records and scope directories must be real')
    }
    expect(await problems(root)).toEqual(expected)
  })
})

describe('CLI', () => {
  it('maps valid, invalid, and unusable ledgers to exit 0, 1, and 2', async () => {
    const root = ledger()
    put(root, partial())
    const ok = capture()
    expect(await main(['--check', '--ledger', root], ok.output)).toBe(0)
    expect(ok.lines).toEqual([
      'scope-01/partial-attempt.json: partial',
      'rust-migration-ledger: 1 record valid; record validity is not qualification success or scope acceptance',
    ])

    put(root, { ...partial(), status: 'complete' })
    const invalid = capture()
    expect(await main(['--check', '--ledger', root], invalid.output)).toBe(1)
    expect(invalid.errors.at(-1)).toBe('rust-migration-ledger: 1 problem; records invalid')

    for (const args of [['--check', '--ledger', join(root, 'absent')], ['--check', '--ledger'], ['--bogus'], []]) {
      expect({ args, status: await main(args, capture().output) }).toEqual({ args, status: 2 })
    }
    const help = capture()
    expect(await main(['--help'], help.output)).toBe(0)
    expect(help.lines[0]).toStartWith('Usage: bun scripts/rust-migration-ledger.ts --check')
  })

  it('validates one ledger from two concurrent processes without changing it', async () => {
    const root = ledger()
    put(root, partial())
    put(root, failed())
    const snapshot = () =>
      readdirSync(root, { recursive: true, encoding: 'utf8' })
        .sort()
        .map((path) => {
          const stat = lstatSync(join(root, path))
          return `${path} ${stat.mtimeMs} ${stat.isFile() ? sha(readFileSync(join(root, path), 'utf8')) : 'dir'}`
        })
    const before = snapshot()
    // Each process starts in its own temporary cwd so neither relies on, nor changes, the test's cwd.
    const run = async () => {
      const cwd = realpathSync(mkdtempSync(join(tmpdir(), 'bake-rust-ledger-cwd-')))
      roots.push(cwd)
      const child = Bun.spawn([process.execPath, SCRIPT, '--check', '--ledger', root], { cwd, stdout: 'pipe', stderr: 'pipe' })
      const timeout = setTimeout(() => child.kill(), 30_000)
      try {
        const [stdout, stderr, status] = await Promise.all([
          new Response(child.stdout).text(),
          new Response(child.stderr).text(),
          child.exited,
        ])
        return { status, signal: child.signalCode, stdout, stderr }
      } finally {
        clearTimeout(timeout)
        child.kill()
        await child.exited
      }
    }
    const completed = await Promise.allSettled([run(), run()])
    const [first, second] = completed.map((result) => {
      if (result.status === 'rejected') throw result.reason
      return result.value
    })
    expect(first).toEqual({
      status: 0,
      signal: null,
      stdout: [
        'scope-01/partial-attempt.json: partial',
        'scope-02/failed-attempt.json: failed',
        'rust-migration-ledger: 2 records valid; record validity is not qualification success or scope acceptance',
        '',
      ].join('\n'),
      stderr: '',
    })
    expect(second).toEqual(first)
    expect(snapshot()).toEqual(before)
  })
})
