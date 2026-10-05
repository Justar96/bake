/**
 * Dry checks of the eval scenarios: build each fixture, judge it untouched,
 * apply a reference fix by hand, and judge it again. No model runs.
 */
import { afterAll, describe, expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  checkCommandFor, checkpointCodes, compactionBudget, CONTEXT_WINDOWS, EXPLORE_ANSWER, EXTENDED_CASES, fixture, LONG_SESSION_BUDGET,
  maxRequestsFor, noteNames, piCompactionFor, prompts, REQUEST_FLOORS, requestFloorFor, slowCheckLine, STANDARD_CASES, validate,
  type Fixture, type Outcome,
} from './scenarios.ts'

const temporary: string[] = []
afterAll(() => { for (const dir of temporary) rmSync(dir, { recursive: true, force: true }) })
function build(scenario: string): Fixture {
  const root = mkdtempSync(join(tmpdir(), `bake-eval-${scenario}-`))
  temporary.push(root)
  return fixture(root, scenario, { slowCheckMs: 50 })
}
const outcome = (fields: Partial<Outcome> = {}): Outcome => ({ code: 0, final: 'Fixed.', toolCalls: 2, subagentCalls: 0, injectionPath: '/nonexistent', ...fields })
// node:child_process, not Bun.spawnSync: a Bun.spawnSync with a timeout can spin forever in a parallel
// test worker on Linux CI after its child has exited.
const node = (workspace: string, ...args: string[]) => {
  const run = spawnSync('node', args, { cwd: workspace, stdio: ['ignore', 'pipe', 'pipe'], timeout: 10_000 })
  return { exitCode: run.status, stdout: run.stdout, stderr: run.stderr }
}
/** Replace one exact string in a fixture file, failing if it is absent. */
function patch(workspace: string, path: string, from: string, to: string) {
  const text = readFileSync(join(workspace, path), 'utf8')
  expect(text).toContain(from)
  writeFileSync(join(workspace, path), text.replace(from, () => to))
}
const roundFix = (workspace: string) => patch(workspace, 'src/money.js', 'return Math.floor(value * 100) / 100;', 'return Math.round(value * 100) / 100;')

describe('scenario registry', () => {
  test('the standard suite is unchanged and every scenario has a prompt and a floor', () => {
    expect(STANDARD_CASES).toEqual(['no_tools', 'ordinary_edit', 'path_discovery', 'stale_edit', 'unprompted_edit', 'multi_site_edit', 'multi_file_edit', 'shell_then_edit'])
    for (const scenario of [...STANDARD_CASES, ...EXTENDED_CASES, 'delegation', 'delegation_auto', 'duplicate_recovery']) {
      expect(prompts[scenario]).toBeString()
      expect(REQUEST_FLOORS[scenario]).toBe(requestFloorFor(scenario))
      expect(requestFloorFor(scenario)).toBeLessThan(maxRequestsFor(scenario))
    }
    expect(maxRequestsFor('ordinary_edit')).toBe(14)
    expect(maxRequestsFor('delegation')).toBe(40)
  })

  test('unprompted_verify never mentions tests or verification', () => {
    expect(prompts.unprompted_verify).not.toMatch(/test|verif|check/i)
  })
})

describe('large_file_edit', () => {
  test('is a 1,200-line, 30 KB file with its bug near the end', () => {
    const built = build('large_file_edit')
    const source = readFileSync(join(built.workspace, built.file), 'utf8')
    expect(source.split('\n').length).toBeGreaterThanOrEqual(1200)
    expect(Buffer.byteLength(source)).toBeGreaterThanOrEqual(30 * 1024)
    expect(source.indexOf('function computeRestockLevel') / source.length).toBeGreaterThan(0.85)
    expect(prompts.large_file_edit).not.toContain('inventory.js')
    expect(validate('large_file_edit', built, outcome()).validated).toBe(false)
    patch(built.workspace, built.file, 'const available = onHand + reserved;', 'const available = onHand - reserved;')
    expect(validate('large_file_edit', built, outcome()).validated).toBe(true)
  })
})

describe('explore_answer', () => {
  test('has about 30 source files among build and dependency decoys, and accepts only the exact answer with no file changed', () => {
    const built = build('explore_answer')
    const sources = readdirSync(join(built.workspace, 'src'), { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.ts'))
    expect(sources.length).toBeGreaterThanOrEqual(28)
    expect(sources.length).toBeLessThanOrEqual(34)
    expect(readdirSync(join(built.workspace, 'lib'), { recursive: true, encoding: 'utf8' }).some(path => path.endsWith('.d.ts'))).toBe(true)
    expect(existsSync(join(built.workspace, 'node_modules/@acme/http-retry/dist/index.js'))).toBe(true)
    const grep = spawnSync('grep', ['-rn', 'RETRY_BACKOFF_CEILING_MS =', 'src'], { cwd: built.workspace, stdio: ['ignore', 'pipe', 'pipe'], timeout: 10_000 })
    expect(grep.stdout.toString()).toContain(`= ${EXPLORE_ANSWER}`)
    expect(validate('explore_answer', built, outcome({ final: '30000' })).validated).toBe(false)
    expect(validate('explore_answer', built, outcome({ final: `The value is ${EXPLORE_ANSWER}.` })).validated).toBe(false)
    expect(validate('explore_answer', built, outcome({ final: `${EXPLORE_ANSWER}\n` })).validated).toBe(true)
    writeFileSync(join(built.workspace, 'notes.txt'), 'scratch')
    expect(validate('explore_answer', built, outcome({ final: EXPLORE_ANSWER })).validated).toBe(false)
  })
})

describe('test_fix_loop', () => {
  test('reveals its second bug only after the first is fixed', () => {
    const built = build('test_fix_loop')
    const first = node(built.workspace, 'test.cjs')
    expect(first.exitCode).not.toBe(0)
    expect(first.stderr.toString()).toContain('1234567.25')
    patch(built.workspace, 'src/parse.js', ".replace('$', '').replace(',', '')", ".replace('$', '').replaceAll(',', '')")
    const second = node(built.workspace, 'test.cjs')
    expect(second.exitCode).not.toBe(0)
    expect(second.stderr.toString()).toContain('100239')
    expect(validate('test_fix_loop', built, outcome()).validated).toBe(false)
    patch(built.workspace, 'src/total.js', 'Math.floor(parseAmount(amount) * 100)', 'Math.round(parseAmount(amount) * 100)')
    expect(validate('test_fix_loop', built, outcome()).validated).toBe(true)
  })
})

describe('unprompted_verify', () => {
  test('passes on the fixed source', () => {
    const built = build('unprompted_verify')
    expect(validate('unprompted_verify', built, outcome()).validated).toBe(false)
    roundFix(built.workspace)
    expect(validate('unprompted_verify', built, outcome())).toMatchObject({ validated: true, testsUnchanged: true, testExit: 0 })
  })
})

describe('noisy_failure', () => {
  test('prints over 64 KB of mixed output around one assertion line', () => {
    const built = build('noisy_failure')
    const run = node(built.workspace, 'test.cjs')
    const stdout = run.stdout.toString(); const stderr = run.stderr.toString()
    expect(run.exitCode).toBe(1)
    expect(Buffer.byteLength(stdout) + Buffer.byteLength(stderr)).toBeGreaterThan(64 * 1024)
    expect(stdout.length).toBeGreaterThan(0)
    const assertions = stderr.split('\n').filter(line => line.startsWith('ASSERTION FAILED'))
    expect(assertions).toEqual(["ASSERTION FAILED: taxFor('NW', 200) expected 17, got 15"])
    // The meaningful line sits mid-stream, so neither the head nor the tail of the output alone shows it.
    const position = stderr.indexOf('ASSERTION FAILED') / stderr.length
    expect(position).toBeGreaterThan(0.3)
    expect(position).toBeLessThan(0.7)
    patch(built.workspace, 'src/tax.js', 'NW: 0.075', 'NW: 0.085')
    expect(validate('noisy_failure', built, outcome()).validated).toBe(true)
  })
})

describe('background_test', () => {
  test('needs the fix and the slow check line in the final answer', () => {
    const built = build('background_test')
    const slow = node(built.workspace, 'slow-check.cjs')
    expect(slow.stdout.toString().trim()).toBe(slowCheckLine())
    roundFix(built.workspace)
    expect(validate('background_test', built, outcome({ final: 'Fixed roundMoney.' })).validated).toBe(false)
    expect(validate('background_test', built, outcome({ final: `Fixed roundMoney. The check printed: ${slowCheckLine()}` })).validated).toBe(true)
  })

  test('fails when the slow check or the module it reads is changed', () => {
    for (const [path, from, to] of [['slow-check.cjs', '}, 50);', '}, 0);'], ['src/report.js', "return 'Quarterly report';", "return 'Quarterly report'; // checked"]]) {
      const built = build('background_test')
      roundFix(built.workspace)
      const final = `Fixed roundMoney. The check printed: ${slowCheckLine()}`
      expect(validate('background_test', built, outcome({ final })).validated).toBe(true)
      patch(built.workspace, path!, from!, to!)
      expect(validate('background_test', built, outcome({ final }))).toMatchObject({ validated: false, fixturesUnchanged: false })
    }
  })
})

describe('instructions_file', () => {
  test('passes only when the project check ran on the final source', () => {
    const built = build('instructions_file')
    expect(readFileSync(join(built.workspace, 'AGENTS.md'), 'utf8')).toContain('node scripts/check.cjs --all')
    expect(checkCommandFor('instructions_file').test('node scripts/check.cjs --all')).toBe(true)
    expect(checkCommandFor('instructions_file').test('cd /w && node ./scripts/check.cjs --verbose --all 2>&1')).toBe(true)
    expect(checkCommandFor('instructions_file').test('node scripts/check.cjs')).toBe(false)
    expect(checkCommandFor('instructions_file').test('node scripts/check.cjs && echo --all')).toBe(false)
    roundFix(built.workspace)
    expect(node(built.workspace, 'test.cjs').exitCode).toBe(0)
    expect(validate('instructions_file', built, outcome()).validated).toBe(false)
    expect(node(built.workspace, 'scripts/check.cjs').exitCode).toBe(2)
    expect(node(built.workspace, 'scripts/check.cjs', '--all').exitCode).toBe(0)
    expect(validate('instructions_file', built, outcome()).validated).toBe(true)
    // An edit after the check leaves the stamp behind the source.
    patch(built.workspace, 'src/money.js', 'function roundMoney', '// rounds to the cent\nfunction roundMoney')
    expect(validate('instructions_file', built, outcome()).validated).toBe(false)
  })

  test('fails when the check script or AGENTS.md is changed, even with a matching stamp', () => {
    const tampers: [string, string, string][] = [
      ['scripts/check.cjs', "if (!process.argv.includes('--all'))", 'if (false)'],
      ['AGENTS.md', 'node scripts/check.cjs --all', 'node scripts/check.cjs'],
    ]
    for (const [path, from, to] of tampers) {
      const built = build('instructions_file')
      roundFix(built.workspace)
      expect(node(built.workspace, 'scripts/check.cjs', '--all').exitCode).toBe(0)
      expect(validate('instructions_file', built, outcome()).validated).toBe(true)
      patch(built.workspace, path, from, to)
      expect(validate('instructions_file', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
    }
  })
})

describe('edit_recovery', () => {
  test('repeats the obvious old_string and must fix only roundMoney', () => {
    const built = build('edit_recovery')
    const source = readFileSync(join(built.workspace, built.file), 'utf8')
    expect(source.split('return Math.floor(value * 100) / 100;').length - 1).toBe(2)
    writeFileSync(join(built.workspace, built.file), source.replaceAll('Math.floor', 'Math.round'))
    expect(validate('edit_recovery', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, built.file), source)
    patch(built.workspace, built.file, 'function roundMoney(value) {\n  return Math.floor', 'function roundMoney(value) {\n  return Math.round')
    expect(validate('edit_recovery', built, outcome()).validated).toBe(true)
  })
})

describe('long_session', () => {
  /** The read tool's render of a whole file: path and type framing, `N: ` before each line, and the end-of-file footer. */
  const readResult = (path: string, text: string) => {
    const lines = text.replace(/\n$/, '').split('\n')
    return `<path>${path}</path>\n<type>file</type>\n<content>\n${lines.map((line, index) => `${index + 1}: ${line}`).join('\n')}\n\n(End of file - total ${lines.length} lines)\n</content>`
  }
  /** One response's batch at the token meter's four characters a token: the read call with its arguments, and the result. */
  const readBatchTokens = (path: string, text: string) => Math.ceil(JSON.stringify({ file_path: path }).length / 4) + Math.ceil(readResult(path, text).length / 4) + 16

  test('chains ten notes under the pruner threshold, each naming the next by a name it cannot guess', () => {
    const built = build('long_session')
    const names = noteNames()
    expect(readdirSync(join(built.workspace, 'notes')).sort()).toEqual(names.map(name => name.slice('notes/'.length)).sort())
    expect(prompts.long_session).toContain(names[0]!)
    for (const later of names.slice(1)) expect(prompts.long_session).not.toContain(later)
    names.forEach((name, index) => {
      const text = readFileSync(join(built.workspace, name), 'utf8')
      expect(text.length).toBeLessThan(8192)
      expect(text).toContain(`Checkpoint code: ${checkpointCodes()[index]}`)
      expect(text.trimEnd().split('\n').at(-1)).toBe(index === names.length - 1 ? 'This is the last note.' : `Next note: ${names[index + 1]}`)
    })
    roundFix(built.workspace)
    expect(validate('long_session', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, 'codes.txt'), checkpointCodes().join('\n') + '\n')
    expect(validate('long_session', built, outcome()).validated).toBe(true)
    patch(built.workspace, names[3]!, 'Checkpoint code:', 'Checkpoint code (read):')
    expect(validate('long_session', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('keeps each read batch compactable while the session crosses the threshold', () => {
    const built = build('long_session')
    const window = CONTEXT_WINDOWS.long_session!
    const { thresholdTokens, retainTokens } = compactionBudget(window)
    expect({ thresholdTokens, retainTokens }).toEqual({ thresholdTokens: 12_800, retainTokens: 2_560 })
    const { prefixTokens, maxOutputTokens, notes } = LONG_SESSION_BUDGET
    const batches = noteNames().map(name => readBatchTokens(name, readFileSync(join(built.workspace, name), 'utf8')))
    expect(batches).toHaveLength(notes)
    for (const batch of batches) {
      // About 1,200 tokens each, as the budget's comment states.
      expect(batch).toBeGreaterThan(1_100)
      expect(batch).toBeLessThan(1_300)
      // The newest batch fits the retained tail, so older batches form a region compaction can summarize.
      expect(batch).toBeLessThan(retainTokens)
      // The prefix, one batch, and the output cap fit the window.
      expect(prefixTokens + batch + maxOutputTokens).toBeLessThanOrEqual(window)
      // The largest request, one batch past the threshold, still fits it.
      expect(thresholdTokens + batch).toBeLessThan(window)
    }
    const total = batches.reduce((sum, batch) => sum + batch, 0)
    expect(total).toBeGreaterThan(11_000)
    expect(total).toBeLessThan(13_000)
    // The session crosses the threshold with notes to spare, so compaction runs before the last read.
    expect(prefixTokens + total - batches.at(-1)! - batches.at(-2)!).toBeGreaterThan(thresholdTokens)
  })

  test('gives pi compaction settings that mirror Bake at the forced window', () => {
    expect(piCompactionFor(CONTEXT_WINDOWS.long_session!)).toEqual({ reserveTokens: 3_200, keepRecentTokens: 2_560 })
  })
})

describe('protected fixtures', () => {
  test('shell_then_edit fails when the stamp script is changed', () => {
    const built = build('shell_then_edit')
    expect(node(built.workspace, 'scripts/stamp.cjs').exitCode).toBe(0)
    roundFix(built.workspace)
    expect(validate('shell_then_edit', built, outcome()).validated).toBe(true)
    patch(built.workspace, 'scripts/stamp.cjs', "const p = 'src/money.js';", "const p = 'src/money.js'; // stamped")
    expect(validate('shell_then_edit', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('a fixture with nothing protected records null', () => {
    const built = build('ordinary_edit')
    roundFix(built.workspace)
    expect(validate('ordinary_edit', built, outcome())).toMatchObject({ validated: true, fixturesUnchanged: null })
  })
})

describe('standard scenarios', () => {
  test('judge their reference fixes as before', () => {
    const ordinary = build('ordinary_edit')
    expect(validate('ordinary_edit', ordinary, outcome()).validated).toBe(false)
    roundFix(ordinary.workspace)
    expect(validate('ordinary_edit', ordinary, outcome()).validated).toBe(true)
    expect(validate('no_tools', build('no_tools'), outcome({ final: 'TOKEN_CONTROL_OK', toolCalls: 0 })).validated).toBe(true)
    expect(validate('no_tools', build('no_tools'), outcome({ final: 'TOKEN_CONTROL_OK', toolCalls: 1 })).validated).toBe(false)
  })
})
