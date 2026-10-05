/**
 * Agent-loop eval scenarios: each one's prompt, fixture, success predicate,
 * request floor and cap, and the command that counts as its check. The runner
 * (`run.ts`) drives the agent; everything here is pure or touches only the
 * fixture directory it is given, so tests can build a fixture and judge it
 * without a model.
 */
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { COMPACTION_DEFAULTS } from '../../packages/compaction/compaction-basic/src/config.ts'

/** The standard suite every recorded version runs; `duplicate_recovery` and the delegation cases are opt-in. */
export const STANDARD_CASES = ['no_tools', 'ordinary_edit', 'path_discovery', 'stale_edit', 'unprompted_edit', 'multi_site_edit', 'multi_file_edit', 'shell_then_edit']
/** Opt-in scenarios that stress what the standard suite cannot see: large files, exploration, verification habits, noisy output, background work, instructions, ambiguous edits, and compaction. */
export const EXTENDED_CASES = ['large_file_edit', 'explore_answer', 'test_fix_loop', 'unprompted_verify', 'noisy_failure', 'background_test', 'instructions_file', 'edit_recovery', 'long_session']
/** Named scenario sets; `EVAL_CASES` may list set names, scenario names, or both. */
export const CASE_SETS: Record<string, string[]> = { standard: STANDARD_CASES, extended: EXTENDED_CASES }

const hash = (text: string) => createHash('sha256').update(text).digest('hex')
const baseCode = 'function roundMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\nmodule.exports = { roundMoney };\n'
const fixedTests = (file: string) => `const assert = require('node:assert/strict');\nconst { roundMoney } = require('./${file}');\nfor (const [input, expected] of [[1.236,1.24],[2.994,2.99],[0.019,0.02],[-1.236,-1.24],[0,0]]) assert.equal(roundMoney(input), expected);\nconsole.log('FIXTURE_PASS');\n`
const natural = ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Run node test.cjs to verify and give a brief final result.'
const common = ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Use the read and edit tools for source-file changes. Run node test.cjs to verify and give a brief final result.'
/** The scope rules without any mention of tests or verification; the predicate still requires test.cjs unchanged. */
const scopeOnly = ' Work only inside this fixture. Do not delegate, install packages, use the network, or make commits. Give a brief final result.'
const roundMoneyTask = 'Fix roundMoney in src/money.js: it should round to the nearest cent, matching Math.round(value * 100) / 100.'

/** The answer `explore_answer` expects, as the exact final reply. */
export const EXPLORE_ANSWER = '47250'
/** The milliseconds `background_test`'s slow check sleeps before reporting, in a live run. */
export const SLOW_CHECK_MS = 20_000

export const prompts: Record<string, string> = {
  no_tools: 'Reply exactly TOKEN_CONTROL_OK. Do not call any tools.',
  ordinary_edit: 'Fix roundMoney in src/money.js: it should round to the nearest cent, matching Math.round(value * 100) / 100. The tests in test.cjs describe the required behavior.' + common,
  path_discovery: 'Fix roundMoney so it rounds to the nearest cent, matching Math.round(value * 100) / 100. An old ticket points to src/money.js, but the module may have moved. Locate the current implementation and use test.cjs to verify.' + common,
  stale_edit: 'Read src/money.js with the read tool, then fix roundMoney to round to the nearest cent, matching Math.round(value * 100) / 100. Another writer may update this file while you work; preserve any added comments.' + common,
  unprompted_edit: 'Fix roundMoney in src/money.js: it should round to the nearest cent, matching Math.round(value * 100) / 100.' + natural,
  multi_site_edit: 'In src/config.js set DEFAULT_PORT to 8080, DEFAULT_HOST to \'0.0.0.0\', and RETRIES to 3.' + natural,
  multi_file_edit: 'Fix two bugs: roundMoney in src/money.js must round to the nearest cent (Math.round(value * 100) / 100), and formatMoney in src/format.js must show two decimals with a leading $ sign.' + natural,
  shell_then_edit: 'Read src/money.js, then run node scripts/stamp.cjs (it adds a build header to the file that must be kept), then fix roundMoney to round to the nearest cent, matching Math.round(value * 100) / 100.' + natural,
  delegation: 'Use the subagent tool for this task: delegate to one subagent the job of reading notes/a.txt and notes/b.txt and returning each file\'s single line of text, labelled a and b. Then write summary.txt in this directory with the a line followed by the b line, one per line. Work only inside this fixture. Do not install packages, use the network, or make commits. Give a brief final result.',
  // The delegation task with the route left to the host, so every delegation reaches the task router when one is on.
  delegation_auto: 'Use the subagent tool for this task: delegate to one subagent the job of reading notes/a.txt and notes/b.txt and returning each file\'s single line of text, labelled a and b. Leave provider, model, and reasoning_effort unset so the host chooses the subagent\'s route. Then write summary.txt in this directory with the a line followed by the b line, one per line. Work only inside this fixture. Do not install packages, use the network, or make commits. Give a brief final result.',
  duplicate_recovery: 'Exercise a guarded-edit recovery case. Before reading or running any other tool, attempt the same edit of src/money.js three times: replace "Math.floor(value * 100) / 100" with "Math.round(value * 100) / 100". Make these three edit calls consecutive, even if a call is refused. Then recover from any refusal and finish the correction.' + common,
  large_file_edit: 'computeRestockLevel returns too few units when stock is reserved for open orders. Reserved units are not available to sell, so the order must cover them: the result is the target level minus the units on hand that are not reserved, and never below zero. Fix it.' + natural,
  explore_answer: 'What value does this project\'s own source code assign to RETRY_BACKOFF_CEILING_MS? Answer from the source files, not from build output or dependencies. Do not modify any files, delegate, install packages, or use the network. Reply with the number only: digits and nothing else.',
  test_fix_loop: 'node test.cjs fails. Find and fix the bugs in the source files until it passes.' + natural,
  unprompted_verify: roundMoneyTask + scopeOnly,
  noisy_failure: 'node test.cjs fails and prints a lot of log output. Find the cause in the source files and fix it.' + natural,
  background_test: 'Do two independent jobs. First start node slow-check.cjs in the background: it takes about 20 seconds, so do not wait for it idly. While it runs, ' + roundMoneyTask.replace('Fix', 'fix') + ' Run node test.cjs to verify that fix. Then collect the output of slow-check.cjs and quote the line it prints in your final answer. Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or slow-check.cjs, or make commits.',
  instructions_file: roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or anything under scripts/, or make commits. Verify the change the way this project requires and give a brief final result.',
  edit_recovery: roundMoneyTask + ' truncateMoney must keep truncating.' + common,
  long_session: `This task has three steps; finish all of them. 1. Read ${noteNames()[0]} in full with the read tool. Each of the ten notes holds one line that starts with "Checkpoint code:" and ends by naming the next note; read them one at a time in that chain until a note says it is the last. 2. Write codes.txt in this directory with the ten codes in chain order, one per line and nothing else. 3. ` + roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Run node test.cjs to verify and give a brief final result.',
}

/** Scenarios whose run needs more than the default 14 model requests. */
const REQUEST_CAPS: Record<string, number> = {
  delegation: 40, delegation_auto: 40,
  explore_answer: 20, test_fix_loop: 20, noisy_failure: 16, background_test: 20, edit_recovery: 16, long_session: 30,
}
/** The most model requests a sample may make before the proxy stops it. */
export const maxRequestsFor = (scenario: string) => REQUEST_CAPS[scenario] ?? 14
/** Every cap the design records: the default and each raised scenario. */
export const requestCaps = () => ({ default: 14, ...REQUEST_CAPS })
/** Scenarios whose wall-clock cap is above the default 180 s. */
const WALL_CLOCK_CAPS: Record<string, number> = { background_test: 240_000, long_session: 360_000 }
/** The wall-clock cap of one sample, in milliseconds. */
export const capMsFor = (scenario: string) => WALL_CLOCK_CAPS[scenario] ?? 180_000
/** Every wall-clock cap the design records. */
export const wallClockCaps = () => ({ default: 180_000, ...WALL_CLOCK_CAPS })

/**
 * The fewest model requests a competent run of each scenario needs: one per
 * response that must see an earlier tool result, plus the final reply. A run
 * that batches independent calls as the persona asks reaches it;
 * `excessRequests` is what a sample made beyond it, and `requestsOverFloor`
 * the same difference unclamped, so a floor set too high shows as a negative.
 */
export const REQUEST_FLOORS: Record<string, number> = {
  no_tools: 1,
  ordinary_edit: 3, // read; edit with the test; final
  path_discovery: 4, // search; read; edit with the test; final
  stale_edit: 3, // read, after which the writer injects its comment; edit with the test; final
  unprompted_edit: 3,
  multi_site_edit: 3,
  multi_file_edit: 3, // both reads together; both edits with the test; final
  shell_then_edit: 3, // read with the stamp; edit with the test; final
  delegation: 3, // subagent; write summary.txt; final
  delegation_auto: 3,
  duplicate_recovery: 5, // three scripted edits; read; edit with the test; final
  large_file_edit: 4, // search for the symbol; read its range; edit with the test; final
  explore_answer: 2, // search; final
  test_fix_loop: 3, // both modules with the test; both fixes with the test; final
  unprompted_verify: 3,
  noisy_failure: 3, // test with the read of the module; edit with the test; final
  background_test: 4, // start the check with the read; edit with the test; collect the check; final
  instructions_file: 3,
  edit_recovery: 3,
  // Ten chained reads, the first with the money read and the second with the
  // edit and its test; codes.txt; final; plus at least one summary request.
  long_session: 13,
}
/** The request floor of a scenario; one with none declared has floor 1, so its excess is every request after the first. */
export const requestFloorFor = (scenario: string) => REQUEST_FLOORS[scenario] ?? 1

/** Model context window forced onto the route for a scenario, so compaction runs inside the request cap. */
export const CONTEXT_WINDOWS: Record<string, number> = { long_session: 16_000 }

/**
 * Bake's compaction policy at a context window: compaction-basic's default
 * threshold, where condensing starts, and the recent tokens it keeps verbatim.
 */
export function compactionBudget(contextWindow: number) {
  return {
    thresholdTokens: Math.floor(contextWindow * COMPACTION_DEFAULTS.thresholdRatio),
    retainTokens: Math.floor(contextWindow * COMPACTION_DEFAULTS.retainRatio),
  }
}
/**
 * pi's compaction settings for a forced context window, mirroring Bake's
 * policy: pi compacts above `contextWindow - reserveTokens` and keeps
 * `keepRecentTokens` verbatim. Its default reserve (16,384) exceeds a
 * 16,000-token window, so without these it would compact before every request.
 */
export function piCompactionFor(contextWindow: number) {
  const { thresholdTokens, retainTokens } = compactionBudget(contextWindow)
  return { reserveTokens: contextWindow - thresholdTokens, keepRecentTokens: retainTokens }
}

/** Whether a scenario keeps `agent-instructions` mounted, so the agent reads the fixture's AGENTS.md. */
export const usesAgentInstructions = (scenario: string) => scenario === 'instructions_file'

/** The shell command that counts as running a scenario's check, for the verification metrics. */
export function checkCommandFor(scenario: string): RegExp {
  // The project check counts only with `--all`, in the same command; without it the script exits before checking.
  if (scenario === 'instructions_file') return /\bnode\s+(?:\.\/)?scripts\/check\.cjs\b[^\n;&|]*\s--all\b/
  return /\bnode\s+(?:\.\/)?test\.cjs\b/
}

const configCode = "const DEFAULT_PORT = 3000\nconst DEFAULT_HOST = 'localhost'\nconst RETRIES = 1\nfunction url() {\n  return `http://${DEFAULT_HOST}:${DEFAULT_PORT}`\n}\nmodule.exports = { DEFAULT_PORT, DEFAULT_HOST, RETRIES, url }\n"
const configTests = "const assert = require('node:assert/strict');\nconst c = require('./src/config.js');\nassert.equal(c.DEFAULT_PORT, 8080); assert.equal(c.DEFAULT_HOST, '0.0.0.0'); assert.equal(c.RETRIES, 3);\nassert.equal(c.url(), 'http://0.0.0.0:8080');\nconsole.log('FIXTURE_PASS');\n"
const formatCode = "function formatMoney(value) {\n  return value.toFixed(1)\n}\nmodule.exports = { formatMoney }\n"
const multiTests = fixedTests('src/money.js').replace("console.log('FIXTURE_PASS');", "const { formatMoney } = require('./src/format.js');\nassert.equal(formatMoney(3), '$3.00'); assert.equal(formatMoney(2.5), '$2.50');\nconsole.log('FIXTURE_PASS');")
const stampScript = "const fs = require('node:fs');\nconst p = 'src/money.js';\nconst s = fs.readFileSync(p, 'utf8');\nif (!s.startsWith('// build: 42')) fs.writeFileSync(p, '// build: 42\\n' + s);\n"
export const DELEGATION_LINES = { a: 'ALPHA-7F3Q', b: 'BRAVO-2K9X' }

/** A 1,200-plus-line module whose one bug, in `computeRestockLevel`, sits near the end. */
function inventoryCode(): string {
  const parts = ['\'use strict\';\n\n/**\n * Inventory thresholds for every SKU group. Generated; edit by hand only to fix a bug.\n */\n']
  const names: string[] = []
  for (let group = 0; group < 125; group++) {
    if (group === 118) {
      parts.push('/**\n * Units to order so that the units on hand that are not reserved for open\n * orders reach the target level. Never negative.\n */\nfunction computeRestockLevel(target, onHand, reserved) {\n  const available = onHand + reserved;\n  return Math.max(0, target - available);\n}\n\n')
      names.push('computeRestockLevel')
    }
    parts.push(`/**\n * Reorder threshold for SKU group ${group}: true while stock is under the group's base level plus its safety margin.\n */\nfunction reorderThreshold${group}(stock) {\n  const base = ${group * 3 + 7};\n  const margin = Math.ceil(base * 0.${group % 9 + 1});\n  if (stock < 0) throw new RangeError('stock must be non-negative');\n  return stock < base + margin;\n}\n\n`)
    names.push(`reorderThreshold${group}`)
  }
  parts.push(`module.exports = {\n${names.map(name => `  ${name},`).join('\n')}\n};\n`)
  return parts.join('')
}
const inventoryTests = "const assert = require('node:assert/strict');\nconst inventory = require('./src/inventory.js');\nassert.equal(inventory.computeRestockLevel(100, 40, 10), 70);\nassert.equal(inventory.computeRestockLevel(50, 60, 0), 0);\nassert.equal(inventory.computeRestockLevel(20, 5, 5), 20);\nassert.equal(inventory.computeRestockLevel(10, 30, 25), 5);\nassert.equal(inventory.reorderThreshold3(10), true);\nassert.equal(inventory.reorderThreshold120(900), false);\nconsole.log('FIXTURE_PASS');\n"

/** A built TypeScript project: about 30 source modules, a stale `lib/` build with declarations, and a vendored dependency, each naming the constant. */
function exploreProject(workspace: string) {
  const write = (path: string, text: string) => { mkdirSync(join(workspace, path, '..'), { recursive: true }); writeFileSync(join(workspace, path), text) }
  write('package.json', JSON.stringify({ name: 'acme-sync', version: '2.4.0', main: 'lib/index.js', types: 'lib/index.d.ts', dependencies: { '@acme/http-retry': '^1.3.0' } }, null, 2) + '\n')
  write('tsconfig.json', JSON.stringify({ compilerOptions: { outDir: 'lib', rootDir: 'src', declaration: true, module: 'commonjs', target: 'es2022', strict: true } }, null, 2) + '\n')
  const areas = ['auth', 'cache', 'config', 'db', 'events', 'net', 'queue', 'sync', 'util', 'jobs']
  const modules: string[] = []
  for (let index = 0; index < 29; index++) {
    const area = areas[index % areas.length]!
    const path = `${area}/module${index}`
    modules.push(path)
    write(`src/${path}.ts`, `import { log } from '../util/log'\n\n/** Batch size for ${area} worker ${index}. */\nexport const BATCH_SIZE_${index} = ${index * 8 + 16}\n\nexport function process${index}(items: string[]): number {\n  log('${area}', items.length)\n  return items.length % BATCH_SIZE_${index}\n}\n`)
    write(`lib/${path}.js`, `"use strict";\nObject.defineProperty(exports, "__esModule", { value: true });\nexports.BATCH_SIZE_${index} = ${index * 8 + 16};\nexports.process${index} = (items) => items.length % exports.BATCH_SIZE_${index};\n`)
    write(`lib/${path}.d.ts`, `export declare const BATCH_SIZE_${index} = ${index * 8 + 16};\nexport declare function process${index}(items: string[]): number;\n`)
  }
  write('src/util/log.ts', 'export function log(area: string, count: number): void {\n  if (process.env.ACME_DEBUG) console.error(`[${area}] ${count}`)\n}\n')
  write('src/net/retry.ts', "import { log } from '../util/log'\n\n/** Upper bound on one retry delay, after jitter. */\nexport const RETRY_BACKOFF_CEILING_MS = 47250\n\nexport function backoff(attempt: number): number {\n  log('net', attempt)\n  return Math.min(RETRY_BACKOFF_CEILING_MS, 250 * 2 ** attempt)\n}\n")
  write('src/net/client.ts', "import { backoff, RETRY_BACKOFF_CEILING_MS } from './retry'\n\nexport const MAX_WAIT_MS = RETRY_BACKOFF_CEILING_MS * 4\nexport const nextDelay = (attempt: number) => backoff(attempt)\n")
  write('lib/net/retry.js', '"use strict";\nObject.defineProperty(exports, "__esModule", { value: true });\nexports.RETRY_BACKOFF_CEILING_MS = 30000;\nexports.backoff = (attempt) => Math.min(exports.RETRY_BACKOFF_CEILING_MS, 250 * 2 ** attempt);\n')
  write('lib/net/retry.d.ts', 'export declare const RETRY_BACKOFF_CEILING_MS = 30000;\nexport declare function backoff(attempt: number): number;\n')
  write('lib/net/client.d.ts', 'export declare const MAX_WAIT_MS: number;\nexport declare const nextDelay: (attempt: number) => number;\n')
  write('src/index.ts', modules.map(path => `export * from './${path}'`).join('\n') + "\nexport * from './net/retry'\nexport * from './net/client'\n")
  write('lib/index.d.ts', modules.map(path => `export * from './${path}';`).join('\n') + "\nexport * from './net/retry';\nexport * from './net/client';\n")
  write('node_modules/@acme/http-retry/package.json', JSON.stringify({ name: '@acme/http-retry', version: '1.3.2', main: 'dist/index.js' }, null, 2) + '\n')
  write('node_modules/@acme/http-retry/dist/index.js', '"use strict";\nconst RETRY_BACKOFF_CEILING_MS = 1000;\nmodule.exports = { RETRY_BACKOFF_CEILING_MS, delay: (n) => Math.min(RETRY_BACKOFF_CEILING_MS, 100 * n) };\n')
  write('node_modules/@acme/http-retry/dist/index.d.ts', 'export declare const RETRY_BACKOFF_CEILING_MS = 1000;\nexport declare function delay(n: number): number;\n')
  write('CHANGELOG.md', '# Changelog\n\n## 2.4.0\n\n- Retries back off for longer before giving up.\n')
}

const parseCode = "function parseAmount(text) {\n  return Number(String(text).replace('$', '').replace(',', ''));\n}\nmodule.exports = { parseAmount };\n"
const totalCode = "const { parseAmount } = require('./parse.js');\n\nfunction totalCents(amounts) {\n  return amounts.reduce((sum, amount) => sum + Math.floor(parseAmount(amount) * 100), 0);\n}\nmodule.exports = { totalCents };\n"
const loopTests = "const assert = require('node:assert/strict');\nconst { parseAmount } = require('./src/parse.js');\nconst { totalCents } = require('./src/total.js');\nassert.equal(parseAmount('$1,234,567.25'), 1234567.25);\nassert.equal(parseAmount('12.5'), 12.5);\nassert.equal(totalCents(['$0.29', '$1,000.10', '2']), 100239);\nconsole.log('FIXTURE_PASS');\n"

const taxCode = "const RATES = {\n  NE: 0.0625,\n  NW: 0.075,\n  SE: 0.06,\n  SW: 0.0725,\n  CE: 0.05,\n  MT: 0.04,\n};\n\nfunction taxFor(region, amount) {\n  const rate = RATES[region];\n  if (rate === undefined) throw new Error(`unknown region ${region}`);\n  return Math.round(amount * rate * 100) / 100;\n}\nmodule.exports = { taxFor };\n"
/** A test that writes over 64 KB of interleaved log lines around its one meaningful failure line. */
const noisyTests = "const fs = require('node:fs');\nconst { taxFor } = require('./src/tax.js');\nconst noise = (from, to) => {\n  for (let i = from; i < to; i++) fs.writeSync(i % 2 ? 2 : 1, `[worker ${i % 7}] cache warm: shard ${String(i).padStart(4, '0')} ok in ${(i * 37) % 900}ms ${'.'.repeat(24)}\\n`);\n};\nnoise(0, 520);\nlet failed = false;\nfor (const [region, amount, expected] of [['NE', 100, 6.25], ['NW', 200, 17], ['SE', 50, 3], ['SW', 40, 2.9], ['CE', 10, 0.5], ['MT', 25, 1]]) {\n  const got = taxFor(region, amount);\n  if (got !== expected) { fs.writeSync(2, `ASSERTION FAILED: taxFor('${region}', ${amount}) expected ${expected}, got ${got}\\n`); failed = true; }\n}\nnoise(520, 1040);\nif (failed) process.exitCode = 1;\nelse console.log('FIXTURE_PASS');\n"

const reportCode = "function title() {\n  return 'Quarterly report';\n}\nmodule.exports = { title };\n"
/** The line `slow-check.cjs` prints once it finishes. */
export function slowCheckLine(): string {
  return `slow-check: PASS ${slowCheckCode()}`
}
const slowCheckCode = () => 'RC-' + hash('Quarterly report:slow-check').slice(0, 8).toUpperCase()
const slowCheck = (ms: number) => `const crypto = require('node:crypto');\nconst { title } = require('./src/report.js');\nsetTimeout(() => {\n  const code = 'RC-' + crypto.createHash('sha256').update(title() + ':slow-check').digest('hex').slice(0, 8).toUpperCase();\n  console.log(\`slow-check: \${title() === 'Quarterly report' ? 'PASS' : 'FAIL'} \${code}\`);\n}, ${ms});\n`

const agentsFile = '# Project notes\n\n## Checks\n\nRun `node scripts/check.cjs --all` after every change. It runs the unit tests and the API contract check, and it is the only check this project accepts: `node test.cjs` alone skips the contract.\n'
/** The project check: the tests, a contract check, and a stamp of the source it passed on. */
const checkScript = "const { execFileSync } = require('node:child_process');\nconst crypto = require('node:crypto');\nconst fs = require('node:fs');\nconst path = require('node:path');\nconst root = path.join(__dirname, '..');\nif (!process.argv.includes('--all')) { console.error('check: pass --all to run the full check'); process.exit(2); }\nexecFileSync(process.execPath, ['test.cjs'], { cwd: root, stdio: 'inherit' });\nconst money = require(path.join(root, 'src/money.js'));\nif (typeof money.roundMoney !== 'function' || Object.keys(money).join() !== 'roundMoney') { console.error('contract: src/money.js must export roundMoney only'); process.exit(1); }\nconst source = fs.readFileSync(path.join(root, 'src/money.js'), 'utf8');\nfs.writeFileSync(path.join(root, '.check-stamp'), crypto.createHash('sha256').update(source).digest('hex'));\nconsole.log('CHECK_OK');\n"

const twinCode = 'function roundMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\n\nfunction truncateMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\nmodule.exports = { roundMoney, truncateMoney };\n'
const twinTests = fixedTests('src/money.js').replace("console.log('FIXTURE_PASS');", "const { truncateMoney } = require('./src/money.js');\nassert.equal(truncateMoney(1.239), 1.23); assert.equal(truncateMoney(2.999), 2.99);\nconsole.log('FIXTURE_PASS');")

/** The ten checkpoint codes `long_session` hides in its notes, in file order. */
export function checkpointCodes(): string[] {
  return Array.from({ length: 10 }, (_, index) => 'CP-' + hash(`checkpoint-${index + 1}`).slice(0, 6).toUpperCase())
}
/**
 * The path of each `long_session` note, in chain order. The names are hashed
 * so the next one is known only from the note before it: a run reads one note
 * per response instead of all ten in one batch.
 */
export function noteNames(): string[] {
  return Array.from({ length: 10 }, (_, index) => `notes/ledger-${hash(`note-${index + 1}`).slice(0, 6)}.md`)
}
/**
 * The token arithmetic `long_session` is sized by, at about four characters a
 * token (the token meter's estimate) and Bake's default policy on the forced
 * 16,000-token window: compaction starts at 12,800 tokens and keeps the newest
 * 2,560 verbatim, and it never splits one response's tool calls from their
 * results, so each response's reads are one indivisible batch.
 *
 * - The fixed prefix (system prompt, tool schemas, and the task) is about
 *   4,500 tokens, and the route keeps an 8,192-token output cap.
 * - Each note is about 4.4 KB, so one read (call, numbered lines, and framing)
 *   is about 1,200 tokens. That is under the 2,560-token retained tail, so the
 *   newest batch stays verbatim and the older ones form a region to summarize.
 *   The prefix, one batch, and the output cap (4,500 + 1,200 + 8,192 = 13,892)
 *   fit the window, and the largest request, one batch past the threshold
 *   (12,800 + 1,200 = 14,000), still leaves room for a reply.
 * - The ten batches total about 12,000 tokens, so the session reaches the
 *   threshold around the seventh note (4,500 + 7 × 1,200 = 12,900) and
 *   compacts at least once before the last.
 */
export const LONG_SESSION_BUDGET = { prefixTokens: 4_500, maxOutputTokens: 8_192, notes: 10 }
/** One note of about 4.4 KB, under the pruner's 8,192-character threshold, with its code mid-file and the next note's path at the end. */
function note(index: number, code: string, next: string | undefined): string {
  const paragraph = (n: number) => `Section ${index}.${n}. The ledger reconciler compares each batch against the upstream journal, marks drifted rows for review, and records the operator who cleared them. Batches older than the retention window are archived with their checksums so a later audit can replay them.\n\n`
  const lines = [`# Ledger notes, part ${String(index).padStart(2, '0')}\n\n`]
  for (let n = 1; n <= 7; n++) lines.push(paragraph(n))
  lines.push(`Checkpoint code: ${code}\n\n`)
  for (let n = 8; n <= 16; n++) lines.push(paragraph(n))
  lines.push(next === undefined ? 'This is the last note.\n' : `Next note: ${next}\n`)
  return lines.join('')
}

/** The test file each scenario validates against, unchanged by the agent. */
export function testsFor(scenario: string, file: string): string {
  if (scenario === 'multi_site_edit') return configTests
  if (scenario === 'multi_file_edit') return multiTests
  if (scenario === 'large_file_edit') return inventoryTests
  if (scenario === 'test_fix_loop') return loopTests
  if (scenario === 'noisy_failure') return noisyTests
  if (scenario === 'edit_recovery') return twinTests
  return fixedTests(file)
}

/** A digest of every file under a directory, so a read-only scenario can prove nothing changed. */
export function treeDigest(dir: string): string {
  const entries: string[] = []
  const walk = (current: string) => {
    for (const entry of readdirSync(current).sort()) {
      const path = join(current, entry)
      if (statSync(path).isDirectory()) walk(path)
      else entries.push(`${relative(dir, path)}\0${hash(readFileSync(path, 'utf8'))}`)
    }
  }
  walk(dir)
  return hash(entries.join('\n'))
}

/**
 * A built fixture: its workspace, the file the scenario judges, for a
 * read-only scenario the digest it must keep, and the hash of each fixture
 * file the agent must not change, such as a check script the predicate trusts.
 */
export interface Fixture { workspace: string; file: string; digest?: string; protected?: Record<string, string> }
/** Options a test passes to shorten a fixture; a live run uses the defaults. */
export interface FixtureOptions { slowCheckMs?: number }

/** The hash of each named workspace file, as it stands now. */
function protect(workspace: string, paths: readonly string[]): Record<string, string> {
  return Object.fromEntries(paths.map(path => [path, hash(readFileSync(join(workspace, path), 'utf8'))]))
}
/** Whether every protected file still exists with the hash it had when the fixture was built; null when none is protected. */
export function fixturesUnchanged(built: Fixture): boolean | null {
  if (built.protected === undefined) return null
  return Object.entries(built.protected).every(([path, digest]) => existsSync(join(built.workspace, path)) && hash(readFileSync(join(built.workspace, path), 'utf8')) === digest)
}

/** Build a scenario's workspace under `root`. */
export function fixture(root: string, scenario: string, options: FixtureOptions = {}): Fixture {
  const workspace = join(root, 'workspace')
  mkdirSync(workspace)
  const write = (path: string, text: string) => { mkdirSync(join(workspace, path, '..'), { recursive: true }); writeFileSync(join(workspace, path), text) }
  switch (scenario) {
    case 'large_file_edit':
      write('src/inventory.js', inventoryCode()); write('test.cjs', inventoryTests)
      return { workspace, file: 'src/inventory.js' }
    case 'explore_answer':
      exploreProject(workspace)
      return { workspace, file: 'src/net/retry.ts', digest: treeDigest(workspace) }
    case 'test_fix_loop':
      write('src/parse.js', parseCode); write('src/total.js', totalCode); write('test.cjs', loopTests)
      return { workspace, file: 'src/total.js' }
    case 'noisy_failure':
      write('src/tax.js', taxCode); write('test.cjs', noisyTests)
      return { workspace, file: 'src/tax.js' }
    case 'background_test':
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      write('src/report.js', reportCode); write('slow-check.cjs', slowCheck(options.slowCheckMs ?? SLOW_CHECK_MS))
      return { workspace, file: 'src/money.js', protected: protect(workspace, ['slow-check.cjs', 'src/report.js']) }
    case 'instructions_file':
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      write('AGENTS.md', agentsFile); write('scripts/check.cjs', checkScript)
      return { workspace, file: 'src/money.js', protected: protect(workspace, ['scripts/check.cjs', 'AGENTS.md']) }
    case 'edit_recovery':
      write('src/money.js', twinCode); write('test.cjs', twinTests)
      return { workspace, file: 'src/money.js' }
    case 'long_session':
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      checkpointCodes().forEach((code, index) => write(noteNames()[index]!, note(index + 1, code, noteNames()[index + 1])))
      return { workspace, file: 'src/money.js', protected: protect(workspace, noteNames()) }
  }
  const file = scenario === 'path_discovery' ? 'packages/billing/money.js' : scenario === 'multi_site_edit' ? 'src/config.js' : 'src/money.js'
  if (scenario === 'multi_site_edit') {
    mkdirSync(join(workspace, 'src'), { recursive: true })
    writeFileSync(join(workspace, file), configCode)
    writeFileSync(join(workspace, 'test.cjs'), configTests)
    return { workspace, file }
  }
  if (scenario === 'multi_file_edit') {
    mkdirSync(join(workspace, 'src'), { recursive: true })
    writeFileSync(join(workspace, 'src/money.js'), baseCode)
    writeFileSync(join(workspace, 'src/format.js'), formatCode)
    writeFileSync(join(workspace, 'test.cjs'), multiTests)
    return { workspace, file }
  }
  if (scenario.startsWith('delegation')) {
    mkdirSync(join(workspace, 'notes'), { recursive: true })
    writeFileSync(join(workspace, 'notes/a.txt'), `${DELEGATION_LINES.a}\n`)
    writeFileSync(join(workspace, 'notes/b.txt'), `${DELEGATION_LINES.b}\n`)
    return { workspace, file: 'summary.txt' }
  }
  if (scenario === 'shell_then_edit') {
    mkdirSync(join(workspace, 'scripts'), { recursive: true })
    writeFileSync(join(workspace, 'scripts/stamp.cjs'), stampScript)
  }
  if (scenario !== 'no_tools') {
    mkdirSync(join(workspace, file, '..'), { recursive: true })
    writeFileSync(join(workspace, file), baseCode)
    writeFileSync(join(workspace, 'test.cjs'), fixedTests(file))
    if (scenario === 'path_discovery') {
      for (let i = 0; i < 40; i++) {
        mkdirSync(join(workspace, `packages/utility${i}`), { recursive: true })
        writeFileSync(join(workspace, `packages/utility${i}/index.js`), `module.exports = { fixtureNumber: ${i} };\n`)
      }
    }
  }
  if (scenario === 'shell_then_edit') return { workspace, file, protected: protect(workspace, ['scripts/stamp.cjs']) }
  return { workspace, file }
}

/** What the runner knows when it judges a finished sample. */
export interface Outcome {
  /** The agent's exit code. */
  code: number | null
  /** The final reply. */
  final: string
  /** Every tool call the agent made, in order. */
  toolCalls: number
  /** Calls of the delegation tool. */
  subagentCalls: number
  /** The marker the stale writer leaves once it has injected its comment. */
  injectionPath: string
}
/** A scenario's verdict, and the file contents, test result, and protected-fixture check it rests on. */
export interface Verdict { validated: boolean; source: string; testsUnchanged: boolean | null; testExit: number | null; fixturesUnchanged: boolean | null }

/** Judge a finished sample against its fixture; the agent's exit code is checked by the caller, except where a predicate names it. */
export function validate(scenario: string, built: Fixture, outcome: Outcome): Verdict {
  const { workspace, file } = built
  if (scenario === 'no_tools') {
    return { validated: outcome.final.trim() === 'TOKEN_CONTROL_OK' && outcome.toolCalls === 0, source: '', testsUnchanged: null, testExit: null, fixturesUnchanged: null }
  }
  if (scenario.startsWith('delegation')) {
    const source = existsSync(join(workspace, file)) ? readFileSync(join(workspace, file), 'utf8') : ''
    const validated = outcome.code === 0 && outcome.subagentCalls > 0
      && source.trim().split(/\r?\n/).map(line => line.trim()).join('\n') === `${DELEGATION_LINES.a}\n${DELEGATION_LINES.b}`
    return { validated, source, testsUnchanged: null, testExit: null, fixturesUnchanged: null }
  }
  const source = existsSync(join(workspace, file)) ? readFileSync(join(workspace, file), 'utf8') : ''
  if (scenario === 'explore_answer') {
    return { validated: outcome.final.trim() === EXPLORE_ANSWER && treeDigest(workspace) === built.digest, source, testsUnchanged: null, testExit: null, fixturesUnchanged: null }
  }
  const testsUnchanged = existsSync(join(workspace, 'test.cjs')) && readFileSync(join(workspace, 'test.cjs'), 'utf8') === testsFor(scenario, file)
  const validation = spawnSync('node', ['test.cjs'], { cwd: workspace, stdio: ['ignore', 'pipe', 'pipe'], timeout: 5000 })
  const testExit = validation.status
  const read = (path: string) => existsSync(join(workspace, path)) ? readFileSync(join(workspace, path), 'utf8') : ''
  const unchanged = fixturesUnchanged(built)
  const validated = validation.status === 0 && testsUnchanged && unchanged !== false
    && (scenario !== 'stale_edit' || (existsSync(outcome.injectionPath) && source.includes('EXTERNAL_CHANGE_KEEP')))
    && (scenario !== 'shell_then_edit' || source.startsWith('// build: 42'))
    && (scenario !== 'background_test' || outcome.final.includes(slowCheckLine()))
    // The stamp is the hash of the source the project check last passed on, so it matches only when that check ran after the last edit.
    && (scenario !== 'instructions_file' || read('.check-stamp') === hash(source))
    && (scenario !== 'long_session' || read('codes.txt').split(/\r?\n/).map(line => line.trim()).filter(Boolean).join('\n') === checkpointCodes().join('\n'))
  return { validated, source, testsUnchanged, testExit, fixturesUnchanged: unchanged }
}
