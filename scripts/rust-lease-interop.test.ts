import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import { LEASE_SPEC, reportProblems, RESUME_SPEC, SPECS, type InteropSpec } from './rust-lease-interop.ts'

type Status = 'passed' | 'failed' | 'skipped' | 'pending'
type Statuses = readonly (readonly [string, Status])[]
/** Suite titles that replace the specs' own, as an opt-in skip names them. */
interface Suites { readonly lease?: string; readonly resume?: string }

/** One test file of a Vitest JSON report, shaped as Vitest 4 writes it, with each case's status. */
function file(plan: InteropSpec, statuses: Statuses, suite = plan.suite): Record<string, unknown> {
  return {
    startTime: 0, endTime: 0, status: statuses.some(([, status]) => status === 'failed') ? 'failed' : 'passed', message: '',
    name: `/work/bake/${plan.spec}`,
    assertionResults: statuses.map(([title, status]) => ({
      ancestorTitles: [suite], fullName: `${suite} ${title}`, status, title, failureMessages: [], meta: {}, tags: [],
    })),
  }
}

/** A Vitest JSON report of both specs, the lease spec's statuses first, with totals over both. */
function report(lease: Statuses, resume: Statuses = resumePassed, suites: Suites = {}): Record<string, unknown> {
  const all = [...lease, ...resume]
  const count = (status: Status | Status[]) => all.filter(([, value]) => ([] as Status[]).concat(status).includes(value)).length
  return {
    numTotalTestSuites: 4, numPassedTestSuites: 4, numFailedTestSuites: count('failed') > 0 ? 1 : 0, numPendingTestSuites: 0,
    numTotalTests: all.length, numPassedTests: count('passed'), numFailedTests: count('failed'),
    numPendingTests: count(['skipped', 'pending']), numTodoTests: 0,
    startTime: 0, success: count('failed') === 0,
    testResults: [file(LEASE_SPEC, lease, suites.lease), file(RESUME_SPEC, resume, suites.resume)],
  }
}

const posixOnly = new Set<string>(LEASE_SPEC.posixOnly)
const allPassed = LEASE_SPEC.cases.map(title => [title, 'passed'] as const)
const windowsRun = LEASE_SPEC.cases.map(title => [title, posixOnly.has(title) ? 'skipped' : 'passed'] as const)
const resumePassed = RESUME_SPEC.cases.map(title => [title, 'passed'] as const)
const resumePosixOnly = new Set<string>(RESUME_SPEC.posixOnly)
const resumeWindows = RESUME_SPEC.cases.map(title => [title, resumePosixOnly.has(title) ? 'skipped' : 'passed'] as const)
const total = LEASE_SPEC.cases.length + RESUME_SPEC.cases.length
const windowsSkipped = LEASE_SPEC.posixOnly.length + RESUME_SPEC.posixOnly.length

/** The titles of every `it` in a spec's source. */
function specTitles(plan: InteropSpec): string[] {
  const source = readFileSync(join(import.meta.dir, '..', plan.spec), 'utf8')
  return [...source.matchAll(/\bit(?:\.skipIf\([^)]*\))?\('((?:[^'\\]|\\.)*)'/gu)].map(match => match[1]!.replaceAll('\\\'', '\''))
}

describe('rust lease interop report guard', () => {
  it('pins the lease spec\'s cases: nine, two of them POSIX-only, matching the spec source', () => {
    expect(LEASE_SPEC.cases).toHaveLength(9)
    expect(new Set(LEASE_SPEC.cases).size).toBe(9)
    expect(LEASE_SPEC.posixOnly).toHaveLength(2)
    for (const title of LEASE_SPEC.posixOnly) expect(LEASE_SPEC.cases).toContain(title)
    expect(specTitles(LEASE_SPEC).sort()).toEqual([...LEASE_SPEC.cases].sort())
  })

  it('pins the resume spec\'s cases: twenty-four, five of them POSIX-only, matching the spec source', () => {
    expect(RESUME_SPEC.cases).toHaveLength(24)
    expect(new Set(RESUME_SPEC.cases).size).toBe(24)
    expect(RESUME_SPEC.posixOnly).toHaveLength(5)
    for (const title of RESUME_SPEC.posixOnly) expect(RESUME_SPEC.cases).toContain(title)
    expect(specTitles(RESUME_SPEC).sort()).toEqual([...RESUME_SPEC.cases].sort())
    expect(SPECS).toEqual([LEASE_SPEC, RESUME_SPEC])
  })

  it('accepts all thirty-three passing on Linux and macOS', () => {
    expect(reportProblems(report(allPassed), 'linux')).toEqual([])
    expect(reportProblems(report(allPassed), 'darwin')).toEqual([])
  })

  it('accepts twenty-six passing and the seven POSIX-only cases skipped or pending on Windows', () => {
    expect(reportProblems(report(windowsRun, resumeWindows), 'win32')).toEqual([])
    const pending = LEASE_SPEC.cases.map(title => [title, posixOnly.has(title) ? 'pending' : 'passed'] as const)
    const resumePending = RESUME_SPEC.cases.map(title => [title, resumePosixOnly.has(title) ? 'pending' : 'passed'] as const)
    expect(reportProblems(report(pending, resumePending), 'win32')).toEqual([])
    expect(reportProblems(report(windowsRun), 'win32')).toContain(`case passed, expected skipped: ${RESUME_SPEC.posixOnly[0]}`)
  })

  it('rejects a run whose opt-in skipped every case, on every platform', () => {
    const skipped = report(
      LEASE_SPEC.cases.map(title => [title, 'skipped'] as const),
      RESUME_SPEC.cases.map(title => [title, 'skipped'] as const),
      { lease: 'cross-runtime write lease (opt-in skipped)', resume: 'cross-runtime resume (opt-in skipped)' },
    )
    for (const platform of ['linux', 'darwin', 'win32'] as const) {
      const problems = reportProblems(skipped, platform)
      expect(problems.some(problem => problem.startsWith('case outside the "cross-runtime write lease"'))).toBe(true)
      expect(problems.some(problem => problem.startsWith('case outside the "cross-runtime resume"'))).toBe(true)
      expect(problems).toContain(`numPassedTests is 0, expected ${platform === 'win32' ? total - windowsSkipped : total}`)
    }
  })

  it('rejects the POSIX-only cases skipped off Windows, and any other case skipped on Windows', () => {
    expect(reportProblems(report(windowsRun), 'linux')).toContain(`case skipped, expected passed: ${LEASE_SPEC.posixOnly[0]}`)
    const other = LEASE_SPEC.cases[0]
    const extraSkip = windowsRun.map(([title, status]) => [title, title === other ? 'skipped' : status] as const)
    expect(reportProblems(report(extraSkip, resumeWindows), 'win32')).toContain(`case skipped, expected passed: ${other}`)
    const resumeSkip = resumeWindows.map(([title, status], index) => [title, index === 0 ? 'skipped' : status] as const)
    expect(reportProblems(report(windowsRun, resumeSkip), 'win32')).toContain(`case skipped, expected passed: ${RESUME_SPEC.cases[0]}`)
    expect(reportProblems(report(allPassed, resumeWindows), 'linux')).toContain(`case skipped, expected passed: ${RESUME_SPEC.posixOnly[0]}`)
    expect(reportProblems(report(allPassed), 'win32')).toContain(`case passed, expected skipped: ${LEASE_SPEC.posixOnly[0]}`)
  })

  it('rejects a failed case in either spec', () => {
    const failed = allPassed.map(([title], index) => [title, index === 2 ? 'failed' : 'passed'] as const)
    const problems = reportProblems(report(failed), 'linux')
    expect(problems).toContain(`case failed, expected passed: ${LEASE_SPEC.cases[2]}`)
    expect(problems).toContain(`${LEASE_SPEC.spec}: the test file's status is "failed"`)
    expect(problems).toContain('numFailedTests is 1, expected 0')
    const resumeFailed = resumePassed.map(([title], index) => [title, index === 4 ? 'failed' : 'passed'] as const)
    expect(reportProblems(report(allPassed, resumeFailed), 'linux')).toContain(`case failed, expected passed: ${RESUME_SPEC.cases[4]}`)
  })

  it('rejects a dropped, duplicated, misplaced, or unrecognised case', () => {
    expect(reportProblems(report(allPassed.slice(1)), 'linux')).toContain(`case missing from the report: ${LEASE_SPEC.cases[0]}`)
    expect(reportProblems(report(allPassed, resumePassed.slice(1)), 'linux')).toContain(`case missing from the report: ${RESUME_SPEC.cases[0]}`)
    const duplicated = [...allPassed.slice(1), allPassed[1]!]
    const problems = reportProblems(report(duplicated), 'linux')
    expect(problems).toContain(`case reported more than once: ${LEASE_SPEC.cases[1]}`)
    expect(problems).toContain(`case missing from the report: ${LEASE_SPEC.cases[0]}`)
    // A resume case reported in the lease file is unexpected there and missing from its own.
    const moved = reportProblems(report([...allPassed, resumePassed[0]!], resumePassed.slice(1)), 'linux')
    expect(moved).toContain(`unexpected case (passed): ${RESUME_SPEC.cases[0]}`)
    expect(moved).toContain(`case missing from the report: ${RESUME_SPEC.cases[0]}`)
    expect(reportProblems(report([...allPassed, ['a new case', 'passed']]), 'linux'))
      .toEqual(['unexpected case (passed): a new case', `numTotalTests is ${total + 1}, expected ${total}`, `numPassedTests is ${total + 1}, expected ${total}`])
  })

  it('rejects a report that is not one run of both specs', () => {
    expect(reportProblems(null, 'linux')).toEqual(['the report is not a JSON object'])
    expect(reportProblems([], 'linux')).toEqual(['the report is not a JSON object'])
    expect(reportProblems({ ...report(allPassed), testResults: [] }, 'linux')).toEqual(['the report must hold exactly 2 test files, got 0'])
    const leaseOnly = report(allPassed)
    leaseOnly['testResults'] = (leaseOnly['testResults'] as unknown[]).slice(0, 1)
    expect(reportProblems(leaseOnly, 'linux')).toEqual(['the report must hold exactly 2 test files, got 1'])
    const other = report(allPassed)
    ;(other['testResults'] as Record<string, unknown>[])[0]!['name'] = '/work/bake/packages/session/session-persistence-jsonl/tests/lease.spec.ts'
    expect(reportProblems(other, 'linux')).toEqual([
      `the report has no test file ${LEASE_SPEC.spec}`,
      'the report\'s test file "/work/bake/packages/session/session-persistence-jsonl/tests/lease.spec.ts" is not an interop spec',
    ])
    const windowsPath = report(windowsRun, resumeWindows)
    for (const entry of windowsPath['testResults'] as Record<string, unknown>[]) {
      entry['name'] = `D:\\a\\bake\\${String(entry['name']).slice('/work/bake/'.length).replaceAll('/', '\\')}`
    }
    expect(reportProblems(windowsPath, 'win32')).toEqual([])
  })
})
