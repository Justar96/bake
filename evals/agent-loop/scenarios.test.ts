/**
 * Dry checks of the eval scenarios: build each fixture, judge it untouched,
 * apply a reference fix by hand, and judge it again. No model runs.
 */
import { afterAll, describe, expect, test } from 'bun:test'
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  capMsFor, checkCommandFor, checkpointCodes, compactionBudget, CONTEXT_WINDOWS, EXPLORE_ANSWER, EXTENDED_CASES, FAN_OUT_LEDGERS, fanOutBalances, fixture, LONG_SESSION_BUDGET,
  INDEPENDENT_FACTS, INDEPENDENT_NOTES, MASKING_FACTS, maskingProbe, maxRequestsFor, MCP_CHECKLIST_ITEMS, MCP_DOCS_INSTRUCTIONS, MCP_DOCS_SERVER, MCP_DOCS_TOOL, MCP_DOCUMENTS, noteNames, PI_COMPACTION, PI_CONTEXT_WINDOWS, PI_RESERVE_TOKENS, piCompactionFor, piContextWindowFor, prompts, REQUEST_FLOORS, requestFloorFor, SKILL_SCENARIOS, slowCheckLine, STANDARD_CASES, SUMMARY_TOOL_RESULT_CHARS, usesSkills, validate,
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
    for (const scenario of [...STANDARD_CASES, ...EXTENDED_CASES, 'delegation', 'delegation_auto', 'duplicate_recovery', 'reminder']) {
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

describe('reminder', () => {
  test('needs a schedule_create call and a reminder.txt holding only a schedule id', () => {
    const built = build('reminder')
    expect(readdirSync(built.workspace)).toEqual([])
    expect(validate('reminder', built, outcome({ scheduleCalls: 1 })).validated).toBe(false)
    writeFileSync(join(built.workspace, 'reminder.txt'), 'schedule-1\n')
    expect(validate('reminder', built, outcome()).validated).toBe(false)
    expect(validate('reminder', built, outcome({ scheduleCalls: 1, code: 1 })).validated).toBe(false)
    expect(validate('reminder', built, outcome({ scheduleCalls: 1 })).validated).toBe(true)
    writeFileSync(join(built.workspace, 'reminder.txt'), 'The id is schedule-1')
    expect(validate('reminder', built, outcome({ scheduleCalls: 1 })).validated).toBe(false)
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

describe('skill scenarios', () => {
  const skillsOf = (built: Fixture) => readdirSync(join(built.workspace, '.agents/skills')).sort()

  test('ask for the task alone: neither prompt names a skill', () => {
    expect(prompts.skill_check).toBe(prompts.instructions_file!.replace('anything under scripts/', 'anything under .agents/'))
    expect(prompts.skill_distractors).toBe(prompts.unprompted_edit)
    for (const scenario of ['skill_check', 'skill_distractors']) {
      expect(prompts[scenario]).not.toMatch(/skill/i)
      expect(usesSkills(scenario)).toBe(true)
    }
    expect(usesSkills('instructions_file')).toBe(false)
    expect(SKILL_SCENARIOS).toEqual({ skill_check: 'money-check', skill_distractors: null })
  })

  test('skill_check passes only when the skill\'s own check ran on the final source', () => {
    const built = build('skill_check')
    expect(skillsOf(built)).toEqual(['docker-deploy', 'money-check', 'release-notes', 'translations'])
    const skill = readFileSync(join(built.workspace, '.agents/skills/money-check/SKILL.md'), 'utf8')
    expect(skill).toStartWith('---\nname: money-check\ndescription: ')
    expect(skill).toContain('node scripts/check.cjs --all')
    // The script is in the skill's directory, not the project's, so the agent must resolve the skill's relative path.
    expect(existsSync(join(built.workspace, 'scripts'))).toBe(false)
    expect(existsSync(join(built.workspace, 'AGENTS.md'))).toBe(false)
    const check = checkCommandFor('skill_check')
    for (const command of ['node .agents/skills/money-check/scripts/check.cjs --all', 'cd .agents/skills/money-check && node scripts/check.cjs --all', 'node /w/.agents/skills/money-check/scripts/check.cjs --verbose --all 2>&1']) {
      expect(check.test(command)).toBe(true)
    }
    for (const command of ['node test.cjs --all', 'node .agents/skills/money-check/scripts/check.cjs', 'node scripts/precheck.cjs --all']) expect(check.test(command)).toBe(false)
    roundFix(built.workspace)
    expect(node(built.workspace, 'test.cjs').exitCode).toBe(0)
    expect(validate('skill_check', built, outcome()).validated).toBe(false)
    expect(node(built.workspace, '.agents/skills/money-check/scripts/check.cjs').exitCode).toBe(2)
    const run = node(join(built.workspace, '.agents/skills/money-check'), 'scripts/check.cjs', '--all')
    expect(run.exitCode).toBe(0)
    expect(String(run.stdout)).toContain('CHECK_OK')
    expect(validate('skill_check', built, outcome()).validated).toBe(true)
    // An edit after the check leaves the stamp behind the source.
    patch(built.workspace, 'src/money.js', 'function roundMoney', '// rounds to the cent\nfunction roundMoney')
    expect(validate('skill_check', built, outcome()).validated).toBe(false)
  })

  test('skill_distractors passes on the fix alone, with only skills its task does not need', () => {
    const built = build('skill_distractors')
    expect(skillsOf(built)).toEqual(['docker-deploy', 'release-notes', 'translations'])
    expect(validate('skill_distractors', built, outcome()).validated).toBe(false)
    roundFix(built.workspace)
    expect(validate('skill_distractors', built, outcome()).validated).toBe(true)
  })

  test('fail when a skill file is changed', () => {
    const tampers: [string, string, string, string][] = [
      ['skill_check', '.agents/skills/money-check/SKILL.md', 'node scripts/check.cjs --all', 'node test.cjs'],
      ['skill_check', '.agents/skills/money-check/scripts/check.cjs', "if (!process.argv.includes('--all'))", 'if (false)'],
      ['skill_distractors', '.agents/skills/release-notes/SKILL.md', 'one line per change', 'two lines per change'],
    ]
    for (const [scenario, path, from, to] of tampers) {
      const built = build(scenario)
      roundFix(built.workspace)
      if (scenario === 'skill_check') expect(node(built.workspace, '.agents/skills/money-check/scripts/check.cjs', '--all').exitCode).toBe(0)
      expect(validate(scenario, built, outcome()).validated).toBe(true)
      patch(built.workspace, path, from, to)
      expect(validate(scenario, built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
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
      // pi's summarizer sees only a tool result's first characters; the code must be among them.
      expect(text.indexOf(`Checkpoint code: ${checkpointCodes()[index]}\n`) + `Checkpoint code: ${checkpointCodes()[index]}`.length).toBeLessThan(SUMMARY_TOOL_RESULT_CHARS - 300)
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
      // About 1,650 tokens each, as the budget's comment states.
      expect(batch).toBeGreaterThan(1_550)
      expect(batch).toBeLessThan(1_750)
      // The newest batch fits the retained tail, so older batches form a region compaction can summarize.
      expect(batch).toBeLessThan(retainTokens)
      // The prefix, one batch, and the output cap fit the window.
      expect(prefixTokens + batch + maxOutputTokens).toBeLessThanOrEqual(window)
      // The largest request, one batch past the threshold, still fits it.
      expect(thresholdTokens + batch).toBeLessThan(window)
    }
    const total = batches.reduce((sum, batch) => sum + batch, 0)
    expect(total).toBeGreaterThan(15_500)
    expect(total).toBeLessThan(17_500)
    // The session crosses the threshold with notes to spare, so compaction runs before the last read.
    expect(prefixTokens + total - batches.at(-1)! - batches.at(-2)!).toBeGreaterThan(thresholdTokens)
  })

  test('gives pi its default reserve on a window that keeps Bake\'s threshold and tail', () => {
    const window = CONTEXT_WINDOWS.long_session!
    const { thresholdTokens, retainTokens } = compactionBudget(window)
    const piWindow = piContextWindowFor(window)
    const { reserveTokens, keepRecentTokens } = piCompactionFor(window)
    expect({ piWindow, reserveTokens, keepRecentTokens }).toEqual({ piWindow: 29_184, reserveTokens: PI_RESERVE_TOKENS, keepRecentTokens: 2_560 })
    // pi compacts where Bake does and keeps the same tail.
    expect(piWindow - reserveTokens).toBe(thresholdTokens)
    expect(keepRecentTokens).toBe(retainTokens)
    // pi caps a request's output at the window less its context and 4,096 tokens: up to the
    // threshold, which a reply can pass by one read batch, the route's full cap remains.
    const { maxOutputTokens } = LONG_SESSION_BUDGET
    expect(piWindow - (thresholdTokens + 1_300) - 4_096).toBeGreaterThanOrEqual(maxOutputTokens)
    // A split turn's prefix summary gets half the reserve, and the history summary 0.8 of it, capped by the route.
    expect(Math.min(Math.floor(0.5 * reserveTokens), maxOutputTokens)).toBe(maxOutputTokens)
  })
})

describe('fan_out', () => {
  /** A ledger's balance read back from its text, independently of the generator. */
  const balanceOf = (text: string) => text.split('\n').reduce((sum, line) => {
    const opening = /^opening (\d+)$/.exec(line)
    const entry = /^t=\d{3} (credit|debit) (\d+) ref /.exec(line)
    return opening ? sum + Number(opening[1]) : entry ? sum + (entry[1] === 'credit' ? 1 : -1) * Number(entry[2]) : sum
  }, 0)

  test('sixteen ledgers, about 39 KB, whose balances and total are what balances.txt must hold', () => {
    const built = build('fan_out')
    const names = readdirSync(join(built.workspace, 'ledgers')).sort()
    expect(names).toEqual(FAN_OUT_LEDGERS.map(name => `${name}.log`))
    const texts = names.map(name => readFileSync(join(built.workspace, 'ledgers', name), 'utf8'))
    const bytes = texts.reduce((sum, text) => sum + Buffer.byteLength(text), 0)
    expect(bytes).toBeGreaterThan(36_000)
    expect(bytes).toBeLessThan(42_000)
    for (const text of texts) {
      expect(text.split('\n').filter(line => / (credit|debit) /.test(line))).toHaveLength(60)
      // Memo figures look like amounts, so a sum over every number on a line is wrong.
      expect(text.split('\n').filter(line => /^memo: .*\d/.test(line))).toHaveLength(10)
    }
    const balances = texts.map(balanceOf)
    expect(fanOutBalances()).toEqual([...FAN_OUT_LEDGERS.map((name, index) => `${name}=${balances[index]}`), `total=${balances.reduce((sum, value) => sum + value, 0)}`])
  })

  test('succeeds only with every balance in order, the total, and the ledgers unchanged', () => {
    const built = build('fan_out')
    expect(validate('fan_out', built, outcome()).validated).toBe(false)
    const expected = fanOutBalances()
    writeFileSync(join(built.workspace, 'balances.txt'), [...expected.slice(0, -1).reverse(), expected.at(-1)].join('\n'))
    expect(validate('fan_out', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, 'balances.txt'), expected.join('\r\n') + '\r\n')
    expect(validate('fan_out', built, outcome())).toMatchObject({ validated: true, fixturesUnchanged: true, testExit: null })
    expect(validate('fan_out', built, outcome({ code: 1 })).validated).toBe(false)
    writeFileSync(join(built.workspace, 'ledgers/amber.log'), 'changed')
    expect(validate('fan_out', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('names no tool or method, and leaves room for one read per response', () => {
    expect(prompts.fan_out).not.toMatch(/\b(read|bash|grep|script|codemode|parallel)\b/i)
    expect(requestFloorFor('fan_out')).toBe(2)
    expect(maxRequestsFor('fan_out')).toBeGreaterThanOrEqual(FAN_OUT_LEDGERS.length + 3)
  })
})

describe('mcp_docs', () => {
  /** One JSON-RPC exchange at a time with the built fixture server, over its standard input and output. */
  function driver(script: string) {
    const child = spawn('node', [script], { stdio: ['pipe', 'pipe', 'pipe'] })
    let pending = ''
    let stderr = ''
    const waiting = new Map<number, (message: any) => void>()
    child.stdout.setEncoding('utf8').on('data', chunk => {
      pending += chunk
      const lines = pending.split('\n')
      pending = lines.pop()!
      for (const line of lines.filter(Boolean)) {
        const message = JSON.parse(line)
        waiting.get(message.id)?.(message)
        waiting.delete(message.id)
      }
    })
    child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk })
    let id = 0
    return {
      get stderr() { return stderr },
      /** Send one request and resolve with the whole response message. */
      request(method: string, params?: unknown): Promise<any> {
        id++
        const sent = id
        const answered = new Promise<any>((resolveMessage, reject) => {
          const timer = setTimeout(() => { waiting.delete(sent); reject(new Error(`${method} went unanswered`)) }, 10_000)
          waiting.set(sent, message => { clearTimeout(timer); resolveMessage(message) })
        })
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: sent, method, ...params === undefined ? {} : { params } }) + '\n')
        return answered
      },
      notify(method: string) { child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method }) + '\n') },
      /** Close standard input, as the stdio shutdown does, and wait for the server to exit. */
      async close(): Promise<number | null> {
        child.stdin.end()
        const exit = await new Promise<number | null>(resolveExit => child.once('close', resolveExit))
        child.kill('SIGKILL')
        return exit
      },
    }
  }

  test('is native-only, names no tool, and keeps the default caps', () => {
    expect(STANDARD_CASES).not.toContain('mcp_docs')
    expect(EXTENDED_CASES).not.toContain('mcp_docs')
    expect(prompts.mcp_docs).not.toMatch(/\b(read|bash|grep|resource|get_document)\b/i)
    expect(prompts.mcp_docs).toContain('docs MCP server')
    expect(requestFloorFor('mcp_docs')).toBe(3)
    expect(maxRequestsFor('mcp_docs')).toBe(14)
    expect(capMsFor('mcp_docs')).toBe(180_000)
  })

  test('builds a server beside an empty workspace, with the checklist in two places', () => {
    const built = build('mcp_docs')
    expect(readdirSync(built.workspace)).toEqual([])
    expect(built.file).toBe('checklist.txt')
    expect(built.mcpServer).toEqual({ name: MCP_DOCS_SERVER, command: 'node', args: [expect.stringContaining('docs-server.cjs')], tools: [MCP_DOCS_TOOL] })
    // The server is outside the workspace, so the checklist is reachable only through it.
    for (const path of Object.keys(built.protected!)) expect(path.startsWith(built.workspace)).toBe(false)
    const checklist = MCP_DOCUMENTS[0]!
    expect(checklist.uri).toBe('docs://release/checklist')
    expect(checklist.text.split('\n').slice(2, -1)).toEqual(MCP_CHECKLIST_ITEMS.map((item, index) => `${index + 1}. ${item}`))
    for (const document of MCP_DOCUMENTS.slice(1)) {
      expect(document.text).not.toContain(MCP_CHECKLIST_ITEMS[0]!)
    }
  })

  test('succeeds only on the checklist items in order with the server unchanged', () => {
    const built = build('mcp_docs')
    const write = (text: string) => writeFileSync(join(built.workspace, 'checklist.txt'), text)
    expect(validate('mcp_docs', built, outcome())).toMatchObject({ validated: false, testExit: null })
    write(MCP_DOCUMENTS[1]!.text)
    expect(validate('mcp_docs', built, outcome()).validated).toBe(false)
    write([...MCP_CHECKLIST_ITEMS].reverse().join('\n') + '\n')
    expect(validate('mcp_docs', built, outcome()).validated).toBe(false)
    write(MCP_CHECKLIST_ITEMS.map((item, index) => `${index + 1}. ${item}`).join('\n') + '\n')
    expect(validate('mcp_docs', built, outcome()).validated).toBe(false)
    write(MCP_CHECKLIST_ITEMS.join('\r\n') + '\r\n')
    expect(validate('mcp_docs', built, outcome())).toMatchObject({ validated: true, fixturesUnchanged: true })
    expect(validate('mcp_docs', built, outcome({ code: 1 })).validated).toBe(false)
    // A changed fixture server fails the sample even with the right answer.
    const script = built.mcpServer!.args[0]!
    writeFileSync(script, readFileSync(script, 'utf8') + '// touched\n')
    expect(validate('mcp_docs', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('answers initialize, tools/list, resources/list, resources/read, and tools/call over stdio', async () => {
    const built = build('mcp_docs')
    const server = driver(built.mcpServer!.args[0]!)
    try {
      const initialize = await server.request('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'eval-test', version: '1' } })
      expect(initialize.result).toMatchObject({
        protocolVersion: '2025-11-25',
        capabilities: { tools: { listChanged: false }, resources: { listChanged: false } },
        serverInfo: { name: MCP_DOCS_SERVER },
        instructions: MCP_DOCS_INSTRUCTIONS,
      })
      server.notify('notifications/initialized')
      // A revision the client does not offer gets the server's own.
      const older = driver(built.mcpServer!.args[0]!)
      expect((await older.request('initialize', { protocolVersion: '1999-01-01' })).result.protocolVersion).toBe('2025-06-18')
      expect(await older.close()).toBe(0)

      const tools = (await server.request('tools/list')).result.tools
      expect(tools.map((tool: any) => tool.name)).toEqual([MCP_DOCS_TOOL])
      expect(tools[0].inputSchema).toMatchObject({ type: 'object', required: ['name'] })
      expect(tools[0].description).toContain('release-checklist')

      const resources = (await server.request('resources/list')).result.resources
      expect(resources.map((resource: any) => resource.uri)).toEqual(MCP_DOCUMENTS.map(document => document.uri))
      expect(resources).toHaveLength(3)
      expect((await server.request('resources/templates/list')).result).toEqual({ resourceTemplates: [] })

      const read = await server.request('resources/read', { uri: 'docs://release/checklist' })
      expect(read.result.contents).toEqual([{ uri: 'docs://release/checklist', mimeType: 'text/plain', text: MCP_DOCUMENTS[0]!.text }])
      for (const item of MCP_CHECKLIST_ITEMS) expect(read.result.contents[0].text).toContain(item)
      expect((await server.request('resources/read', { uri: 'docs://nothing' })).error.code).toBe(-32602)

      const call = await server.request('tools/call', { name: MCP_DOCS_TOOL, arguments: { name: 'release-checklist' } })
      expect(call.result).toEqual({ content: [{ type: 'text', text: MCP_DOCUMENTS[0]!.text }] })
      const missing = await server.request('tools/call', { name: MCP_DOCS_TOOL, arguments: { name: 'nothing' } })
      expect(missing.result.isError).toBe(true)
      expect(missing.result.content[0].text).toContain('release-checklist')
      expect((await server.request('tools/call', { name: 'other' })).error.code).toBe(-32602)
      expect((await server.request('does/not/exist')).error.code).toBe(-32601)
      expect((await server.request('ping')).result).toEqual({})

      // The task's own path: the checklist through the tool, then through the resource, agree.
      expect(call.result.content[0].text).toBe(read.result.contents[0].text)
      expect(server.stderr).toBe('')
      // The server writes no file: the fixture is still intact.
      expect(validate('mcp_docs', built, outcome()).fixturesUnchanged).toBe(true)
    } finally {
      expect(await server.close()).toBe(0)
    }
  })
})

describe('protected fixtures', () => {
  test('independent reads require every protected fact in source order', () => {
    const built = build('independent_reads')
    roundFix(built.workspace)
    expect(validate('independent_reads', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, 'facts.txt'), INDEPENDENT_FACTS.join('\n'))
    expect(validate('independent_reads', built, outcome()).validated).toBe(true)
    expect(prompts.independent_reads).toContain('same assistant response')
    expect(requestFloorFor('independent_reads')).toBe(3)
    writeFileSync(join(built.workspace, INDEPENDENT_NOTES[0]!), 'changed')
    expect(validate('independent_reads', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('masking fixture protects older unique facts across twenty noisy chained searches', () => {
    const built = build('context_masking')
    const diagnostics = readFileSync(join(built.workspace, 'diagnostics.log'), 'utf8')
    for (let index = 0; index < 20; index++) {
      const matches = diagnostics.split('\n').filter(line => line.includes(maskingProbe(index)))
      expect(matches.at(-1)).toBe(`${maskingProbe(index)} NEXT: ${index === 19 ? 'DONE' : maskingProbe(index + 1)}`)
      expect(matches.join('\n').length).toBeGreaterThan(6000)
    }
    expect(PI_COMPACTION.context_masking).toEqual(PI_COMPACTION.background_compaction)
    expect(PI_CONTEXT_WINDOWS.context_masking).toBe(40_000)
    roundFix(built.workspace)
    writeFileSync(join(built.workspace, 'facts.txt'), MASKING_FACTS.slice(1).join('\n'))
    expect(validate('context_masking', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, 'facts.txt'), MASKING_FACTS.join('\n'))
    expect(validate('context_masking', built, outcome()).validated).toBe(true)
    writeFileSync(join(built.workspace, 'diagnostics.log'), diagnostics + 'changed\n')
    expect(validate('context_masking', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

  test('background compaction has a positive early band and protected twenty-note output', () => {
    const built = build('background_compaction')
    const window = PI_CONTEXT_WINDOWS.background_compaction!
    const { reserveTokens, keepRecentTokens } = PI_COMPACTION.background_compaction!
    expect({ early: window - 2 * reserveTokens, foreground: window - reserveTokens, keepRecentTokens })
      .toEqual({ early: 7232, foreground: 23_616, keepRecentTokens: 2560 })
    const names = noteNames(20)
    expect(readdirSync(join(built.workspace, 'notes')).length).toBe(20)
    expect(prompts.background_compaction).toContain(names[0]!)
    expect(maxRequestsFor('background_compaction')).toBe(46)
    expect(requestFloorFor('background_compaction')).toBe(22)
    roundFix(built.workspace)
    writeFileSync(join(built.workspace, 'codes.txt'), checkpointCodes(10).join('\n'))
    expect(validate('background_compaction', built, outcome()).validated).toBe(false)
    writeFileSync(join(built.workspace, 'codes.txt'), checkpointCodes(20).join('\n'))
    expect(validate('background_compaction', built, outcome()).validated).toBe(true)
    patch(built.workspace, names[19]!, 'Checkpoint code:', 'Checkpoint code (changed):')
    expect(validate('background_compaction', built, outcome())).toMatchObject({ validated: false, fixturesUnchanged: false })
  })

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
