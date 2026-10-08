import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import { EXPECTED_CASES, POSIX_ONLY_CASES, reportProblems, SPEC, SUITE } from './rust-lease-interop.ts'

type Status = 'passed' | 'failed' | 'skipped' | 'pending'

/** A Vitest JSON report of the spec, shaped as Vitest 4 writes it, with each case's status. */
function report(statuses: readonly (readonly [string, Status])[], suite = SUITE): Record<string, unknown> {
  const count = (status: Status | Status[]) => statuses.filter(([, value]) => ([] as Status[]).concat(status).includes(value)).length
  return {
    numTotalTestSuites: 2, numPassedTestSuites: 2, numFailedTestSuites: count('failed') > 0 ? 1 : 0, numPendingTestSuites: 0,
    numTotalTests: statuses.length, numPassedTests: count('passed'), numFailedTests: count('failed'),
    numPendingTests: count(['skipped', 'pending']), numTodoTests: 0,
    startTime: 0, success: count('failed') === 0,
    testResults: [{
      startTime: 0, endTime: 0, status: count('failed') > 0 ? 'failed' : 'passed', message: '',
      name: `/work/bake/${SPEC}`,
      assertionResults: statuses.map(([title, status]) => ({
        ancestorTitles: [suite], fullName: `${suite} ${title}`, status, title, failureMessages: [], meta: {}, tags: [],
      })),
    }],
  }
}

const posixOnly = new Set<string>(POSIX_ONLY_CASES)
const allPassed = EXPECTED_CASES.map(title => [title, 'passed'] as const)
const windowsRun = EXPECTED_CASES.map(title => [title, posixOnly.has(title) ? 'skipped' : 'passed'] as const)

describe('rust lease interop report guard', () => {
  it('pins the spec\'s cases: nine, two of them POSIX-only, matching the spec source', () => {
    expect(EXPECTED_CASES).toHaveLength(9)
    expect(new Set(EXPECTED_CASES).size).toBe(9)
    for (const title of POSIX_ONLY_CASES) expect(EXPECTED_CASES).toContain(title)
    const source = readFileSync(join(import.meta.dir, '..', SPEC), 'utf8')
    const titles = [...source.matchAll(/\bit(?:\.skipIf\([^)]*\))?\('((?:[^'\\]|\\.)*)'/gu)].map(match => match[1]!.replaceAll('\\\'', '\''))
    expect(titles.sort()).toEqual([...EXPECTED_CASES].sort())
  })

  it('accepts all nine passing on Linux and macOS', () => {
    expect(reportProblems(report(allPassed), 'linux')).toEqual([])
    expect(reportProblems(report(allPassed), 'darwin')).toEqual([])
  })

  it('accepts seven passing and the two stopped-holder cases skipped or pending on Windows', () => {
    expect(reportProblems(report(windowsRun), 'win32')).toEqual([])
    const pending = EXPECTED_CASES.map(title => [title, posixOnly.has(title) ? 'pending' : 'passed'] as const)
    expect(reportProblems(report(pending), 'win32')).toEqual([])
  })

  it('rejects a run whose opt-in skipped every case, on every platform', () => {
    const skipped = report(EXPECTED_CASES.map(title => [title, 'skipped'] as const), 'cross-runtime write lease (opt-in skipped)')
    for (const platform of ['linux', 'darwin', 'win32'] as const) {
      const problems = reportProblems(skipped, platform)
      expect(problems.some(problem => problem.startsWith('case outside'))).toBe(true)
      expect(problems).toContain(`numPassedTests is 0, expected ${platform === 'win32' ? 7 : 9}`)
    }
  })

  it('rejects the POSIX-only cases skipped off Windows, and any other case skipped on Windows', () => {
    expect(reportProblems(report(windowsRun), 'linux')).toContain(`case skipped, expected passed: ${POSIX_ONLY_CASES[0]}`)
    const other = EXPECTED_CASES[0]
    const extraSkip = windowsRun.map(([title, status]) => [title, title === other ? 'skipped' : status] as const)
    expect(reportProblems(report(extraSkip), 'win32')).toContain(`case skipped, expected passed: ${other}`)
    expect(reportProblems(report(allPassed), 'win32')).toContain(`case passed, expected skipped: ${POSIX_ONLY_CASES[0]}`)
  })

  it('rejects a failed case', () => {
    const failed = allPassed.map(([title], index) => [title, index === 2 ? 'failed' : 'passed'] as const)
    const problems = reportProblems(report(failed), 'linux')
    expect(problems).toContain(`case failed, expected passed: ${EXPECTED_CASES[2]}`)
    expect(problems).toContain('numFailedTests is 1, expected 0')
  })

  it('rejects a dropped, duplicated, or unrecognised case', () => {
    expect(reportProblems(report(allPassed.slice(1)), 'linux')).toContain(`case missing from the report: ${EXPECTED_CASES[0]}`)
    const duplicated = [...allPassed.slice(1), allPassed[1]!]
    const problems = reportProblems(report(duplicated), 'linux')
    expect(problems).toContain(`case reported more than once: ${EXPECTED_CASES[1]}`)
    expect(problems).toContain(`case missing from the report: ${EXPECTED_CASES[0]}`)
    expect(reportProblems(report([...allPassed, ['a new case', 'passed']]), 'linux'))
      .toEqual(['unexpected case (passed): a new case', 'numTotalTests is 10, expected 9', 'numPassedTests is 10, expected 9'])
  })

  it('rejects a report that is not a single run of the spec', () => {
    expect(reportProblems(null, 'linux')).toEqual(['the report is not a JSON object'])
    expect(reportProblems([], 'linux')).toEqual(['the report is not a JSON object'])
    expect(reportProblems({ ...report(allPassed), testResults: [] }, 'linux')).toEqual(['the report must hold exactly one test file, got 0'])
    const other = report(allPassed)
    ;(other['testResults'] as Record<string, unknown>[])[0]!['name'] = '/work/bake/packages/session/session-persistence-jsonl/tests/lease.spec.ts'
    expect(reportProblems(other, 'linux')).toEqual([`the report's test file is "/work/bake/packages/session/session-persistence-jsonl/tests/lease.spec.ts", not ${SPEC}`])
    const windowsPath = report(windowsRun)
    ;(windowsPath['testResults'] as Record<string, unknown>[])[0]!['name'] = `D:\\a\\bake\\${SPEC.replaceAll('/', '\\')}`
    expect(reportProblems(windowsPath, 'win32')).toEqual([])
  })
})
