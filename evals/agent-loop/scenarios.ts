/**
 * Agent-loop eval scenarios: each one's prompt, fixture, success predicate,
 * request floor and cap, and the command that counts as its check. The runner
 * (`run.ts`) drives the agent; everything here is pure or touches only the
 * fixture directory it is given, so tests can build a fixture and judge it
 * without a model.
 */
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join, relative, resolve } from 'node:path'
import { COMPACTION_DEFAULTS } from '../../packages/compaction/compaction-basic/src/config.ts'

/** The standard suite every recorded version runs; `duplicate_recovery`, `reminder`, and the delegation cases are opt-in. */
export const STANDARD_CASES = ['no_tools', 'ordinary_edit', 'path_discovery', 'stale_edit', 'unprompted_edit', 'multi_site_edit', 'multi_file_edit', 'shell_then_edit']
/** Opt-in scenarios that stress what the standard suite cannot see: large files, exploration, verification habits, noisy output, background work, instructions, skills, ambiguous edits, compaction, and fan-out over many files. */
export const EXTENDED_CASES = ['large_file_edit', 'explore_answer', 'test_fix_loop', 'unprompted_verify', 'noisy_failure', 'background_test', 'instructions_file', 'skill_check', 'skill_distractors', 'edit_recovery', 'long_session', 'fan_out']
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
const chainedTask = (notes: number) => `This task has three steps; finish all of them. 1. Read ${noteNames(notes)[0]} with the read tool. Each of the ${notes} notes holds one line that starts with "Checkpoint code:" and ends by naming the next note; read them one at a time in that chain until a note says it is the last. 2. Write codes.txt in this directory with the ${notes} codes in chain order, one per line and nothing else. 3. ` + roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or the notes, or make commits. Run node test.cjs to verify and give a brief final result.'

/** The answer `explore_answer` expects, as the exact final reply. */
export const EXPLORE_ANSWER = '47250'
/** The milliseconds `background_test`'s slow check sleeps before reporting, in a live run. */
export const SLOW_CHECK_MS = 20_000

/** Independent read inputs; every fact is needed by the artifact predicate. */
export const INDEPENDENT_NOTES = ['notes/alpha.txt', 'notes/beta.txt', 'notes/gamma.txt', 'notes/delta.txt']
export const INDEPENDENT_FACTS = ['alpha=3917', 'beta=6284', 'gamma=1059', 'delta=7432']
/** An older unique observation that masking must retain throughout noisy search batches. */
export const MASKING_FACTS = ['warehouse=HARBOR-73', 'currency=VIOLET-28', 'rounding=NEAREST-CENT']
export const maskingProbe = (index: number) => `diagnostic-${String(index + 1).padStart(2, '0')}`
/** The `fan_out` ledgers, one `ledgers/<name>.log` each, in the alphabetical order `balances.txt` lists them. */
export const FAN_OUT_LEDGERS = ['amber', 'birch', 'cedar', 'dune', 'ember', 'fjord', 'grove', 'heath', 'iris', 'juniper', 'kestrel', 'lumen', 'maple', 'nimbus', 'orchid', 'prairie']

/** The `mcp_docs` server's name in `mcp.json`, so its tools reach the model as `mcp__docs__<tool>`. */
export const MCP_DOCS_SERVER = 'docs'
/** Its one tool, which returns a document by name. */
export const MCP_DOCS_TOOL = 'get_document'
/** The `instructions` it returns from `initialize`; the model reads the first line in the prompt's `mcp_servers` section. */
export const MCP_DOCS_INSTRUCTIONS = 'Project documentation server: release checklists and runbooks.'
/** The release checklist's items, in order, as `checklist.txt` must hold them. */
export const MCP_CHECKLIST_ITEMS = [
  'Freeze the release branch',
  'Update CHANGELOG.md with the release section',
  'Run the full test suite on the release branch',
  'Tag the release commit',
  'Publish the release notes',
]
/** A document the `mcp_docs` server serves, reachable both as the resource `uri` and by `name` through its tool. */
export interface McpDocument { name: string; uri: string; title: string; description: string; text: string }
/** A document's numbered steps, so its items are told apart from its title. */
const numbered = (title: string, items: readonly string[]) => `# ${title}\n\n${items.map((item, index) => `${index + 1}. ${item}`).join('\n')}\n`
/** The checklist and two distractors; a run that reads the wrong document writes the wrong lines. */
export const MCP_DOCUMENTS: McpDocument[] = [
  {
    name: 'release-checklist', uri: 'docs://release/checklist', title: 'Release checklist',
    description: 'The steps of a release, in the order they are done.',
    text: numbered('Release checklist', MCP_CHECKLIST_ITEMS),
  },
  {
    name: 'deploy-runbook', uri: 'docs://runbook/deploy', title: 'Deployment runbook',
    description: 'How a built image reaches staging and production.',
    text: numbered('Deployment runbook', ['Build the service image', 'Push it to the internal registry', 'Deploy it to staging', 'Watch the error rate for ten minutes', 'Promote it to production']),
  },
  {
    name: 'oncall-rotation', uri: 'docs://oncall/rotation', title: 'On-call rotation',
    description: 'Who carries the pager, and how a handover is done.',
    text: numbered('On-call rotation', ['Hand over at 10:00 on Monday', 'Read the open incidents with the outgoing engineer', 'Check that the pager reaches your phone', 'Record the handover in the rotation log']),
  },
]

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
  reminder: 'Use the schedule tools for this task: create one reminder that fires in two hours with the prompt "rotate the staging API keys". Then write reminder.txt in this directory containing only the id that schedule_create returned. Work only inside this fixture. Do not delegate, install packages, use the network, or make commits. Give a brief final result.',
  duplicate_recovery: 'Exercise a guarded-edit recovery case. Before reading or running any other tool, attempt the same edit of src/money.js three times: replace "Math.floor(value * 100) / 100" with "Math.round(value * 100) / 100". Make these three edit calls consecutive, even if a call is refused. Then recover from any refusal and finish the correction.' + common,
  large_file_edit: 'computeRestockLevel returns too few units when stock is reserved for open orders. Reserved units are not available to sell, so the order must cover them: the result is the target level minus the units on hand that are not reserved, and never below zero. Fix it.' + natural,
  explore_answer: 'What value does this project\'s own source code assign to RETRY_BACKOFF_CEILING_MS? Answer from the source files, not from build output or dependencies. Do not modify any files, delegate, install packages, or use the network. Reply with the number only: digits and nothing else.',
  test_fix_loop: 'node test.cjs fails. Find and fix the bugs in the source files until it passes.' + natural,
  unprompted_verify: roundMoneyTask + scopeOnly,
  noisy_failure: 'node test.cjs fails and prints a lot of log output. Find the cause in the source files and fix it.' + natural,
  background_test: 'Do two independent jobs. First start node slow-check.cjs in the background: it takes about 20 seconds, so do not wait for it idly. While it runs, ' + roundMoneyTask.replace('Fix', 'fix') + ' Run node test.cjs to verify that fix. Then collect the output of slow-check.cjs and quote the line it prints in your final answer. Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or slow-check.cjs, or make commits.',
  instructions_file: roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or anything under scripts/, or make commits. Verify the change the way this project requires and give a brief final result.',
  // instructions_file's task, with the project's check in a skill instead of AGENTS.md.
  skill_check: roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs or anything under .agents/, or make commits. Verify the change the way this project requires and give a brief final result.',
  // unprompted_edit's task, with skills that do not apply to it.
  skill_distractors: roundMoneyTask + natural,
  edit_recovery: roundMoneyTask + ' truncateMoney must keep truncating.' + common,
  long_session: `This task has three steps; finish all of them. 1. Read ${noteNames()[0]} with the read tool. Each of the ten notes holds one line that starts with "Checkpoint code:" and ends by naming the next note; read them one at a time in that chain until a note says it is the last. 2. Write codes.txt in this directory with the ten codes in chain order, one per line and nothing else. 3. ` + roundMoneyTask + ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Run node test.cjs to verify and give a brief final result.',
  background_compaction: chainedTask(20),
  independent_reads: `Read these four independent files using four read calls in the same assistant response: ${INDEPENDENT_NOTES.join(', ')}. Each holds one fact. Write facts.txt with those four fact lines in that order, then ${roundMoneyTask.replace('Fix', 'fix')} Do not change the notes.` + natural,
  fan_out: 'Each file under ledgers/ is an independent daily ledger. A ledger\'s balance is its opening amount plus every credit entry minus every debit entry; memo lines are not entries. Write balances.txt with one line per ledger, <name>=<balance>, where name is the file name without .log, in alphabetical order, then a final line total=<the sum of all the balances>. Do not change the ledgers.' + scopeOnly,
  // The MCP server is named, no tool is: the checklist is reachable through
  // the server's own tool and as one of its resources.
  mcp_docs: 'The project\'s docs MCP server has the release checklist. Write checklist.txt in this directory with its items in order, one per line, each line the item\'s own text without its number.' + scopeOnly,
  context_masking: `First read notes/requirements.txt and retain its three fact lines for the final artifact. Then inspect twenty diagnostic batches: start by calling grep with pattern "${maskingProbe(0)}" and path "diagnostics.log". Each batch's final matching line names the next pattern; call grep once per response and follow that chain until the final batch says DONE. Do not combine batches or read diagnostics.log with another tool. Finally write facts.txt with the three original fact lines in their original order and ${roundMoneyTask.replace('Fix', 'fix')} Do not change the requirements or diagnostics.` + natural,
}

/** Scenarios whose run needs more than the default 14 model requests. */
const REQUEST_CAPS: Record<string, number> = {
  delegation: 40, delegation_auto: 40,
  explore_answer: 20, test_fix_loop: 20, noisy_failure: 16, background_test: 20, edit_recovery: 16, long_session: 30, background_compaction: 46,
  context_masking: 46,
  // Room for a run that lists the ledgers, reads them one per response, and writes the answer.
  fan_out: 24,
}
/** The most model requests a sample may make before the proxy stops it. */
export const maxRequestsFor = (scenario: string) => REQUEST_CAPS[scenario] ?? 14
/** Every cap the design records: the default and each raised scenario. */
export const requestCaps = () => ({ default: 14, ...REQUEST_CAPS })
/** Scenarios whose wall-clock cap is above the default 180 s. */
const WALL_CLOCK_CAPS: Record<string, number> = { background_test: 240_000, long_session: 360_000, background_compaction: 540_000, context_masking: 540_000, fan_out: 300_000 }
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
  reminder: 3, // schedule_create; write reminder.txt; final
  duplicate_recovery: 5, // three scripted edits; read; edit with the test; final
  large_file_edit: 4, // search for the symbol; read its range; edit with the test; final
  explore_answer: 2, // search; final
  test_fix_loop: 3, // both modules with the test; both fixes with the test; final
  unprompted_verify: 3,
  noisy_failure: 3, // test with the read of the module; edit with the test; final
  background_test: 4, // start the check with the read; edit with the test; collect the check; final
  instructions_file: 3,
  skill_check: 3, // the module with the skill's file; edit with the check; final
  skill_distractors: 3,
  edit_recovery: 3,
  // Ten chained reads, the first with the money read and the second with the
  // edit and its test; codes.txt; final; plus at least one summary request.
  long_session: 13,
  // Twenty chained reads, one codes write, and a final reply. Summaries add
  // requests; the lower bound does not assume either arm compacts.
  background_compaction: 22,
  independent_reads: 3,
  // The checklist in one call, through the server's tool or as its resource;
  // the write, which needs that result; the final reply. A run that reaches
  // the resource by its uri lists the resources first and pays one more.
  mcp_docs: 3,
  context_masking: 22,
  // One script or shell command that lists, reads, sums, and writes; final.
  fan_out: 2,
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
 * pi's default `compaction.reserveTokens`. Besides placing the threshold at
 * `contextWindow - reserveTokens`, pi sizes its summary requests from it (0.8
 * of it for the history, 0.5 for a split turn's prefix), and it caps each
 * request's output at the window less the estimated context and 4,096 more
 * tokens. A reserve near or below that margin leaves requests just under the
 * threshold one output token, and a reasoning model's summaries no room.
 */
export const PI_RESERVE_TOKENS = 16_384
/**
 * The context window a pi-family arm (pi or bake-rs) gets in place of a
 * forced window: Bake's threshold at that window plus pi's default reserve,
 * so pi compacts where Bake does with the budgets it ships with.
 */
export function piContextWindowFor(contextWindow: number) {
  return compactionBudget(contextWindow).thresholdTokens + PI_RESERVE_TOKENS
}
/**
 * pi's compaction settings for a forced context window, used with
 * {@link piContextWindowFor}: its default reserve, and Bake's retained tail
 * as `keepRecentTokens`, which pi's default (20,000) would exceed.
 */
export function piCompactionFor(contextWindow: number) {
  return { reserveTokens: PI_RESERVE_TOKENS, keepRecentTokens: compactionBudget(contextWindow).retainTokens }
}

/** Forced windows and policies for Pi-family arms; the background case is native-only and explicitly selected. */
export const PI_CONTEXT_WINDOWS: Record<string, number> = {
  ...Object.fromEntries(Object.entries(CONTEXT_WINDOWS).map(([scenario, window]) => [scenario, piContextWindowFor(window)])),
  background_compaction: 40_000,
  context_masking: 40_000,
}
export const PI_COMPACTION: Record<string, { reserveTokens: number; keepRecentTokens: number }> = {
  ...Object.fromEntries(Object.entries(CONTEXT_WINDOWS).map(([scenario, window]) => [scenario, piCompactionFor(window)])),
  background_compaction: { reserveTokens: PI_RESERVE_TOKENS, keepRecentTokens: 2560 },
  context_masking: { reserveTokens: PI_RESERVE_TOKENS, keepRecentTokens: 2560 },
}

/** Whether a scenario keeps `agent-instructions` mounted, so the agent reads the fixture's AGENTS.md. */
export const usesAgentInstructions = (scenario: string) => scenario === 'instructions_file'

/**
 * Scenarios whose fixture holds an `.agents/skills` directory, each with the
 * skill its task needs, or null when none of them applies.
 */
export const SKILL_SCENARIOS: Record<string, string | null> = { skill_check: 'money-check', skill_distractors: null }
/** Whether a scenario's fixture provides skills, so the agent should see its `.agents/skills`. */
export const usesSkills = (scenario: string) => scenario in SKILL_SCENARIOS

/** The shell command that counts as running a scenario's check, for the verification metrics. */
export function checkCommandFor(scenario: string): RegExp {
  // The project check counts only with `--all`, in the same command; without it the script exits before checking.
  if (scenario === 'instructions_file') return /\bnode\s+(?:\.\/)?scripts\/check\.cjs\b[^\n;&|]*\s--all\b/
  // The skill's check lives in its own directory, so any path to a check.cjs counts.
  if (scenario === 'skill_check') return /\bnode\s+(?:\S*\/)?check\.cjs\b[^\n;&|]*\s--all\b/
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

/** The skill `skill_check` needs: the project check of `instructions_file`, bundled in the skill's own directory. */
const moneyCheckSkill = '---\nname: money-check\ndescription: The verification this project requires after any change to src/money.js. Use it before reporting a money change as done.\n---\n\n# Money check\n\n`node test.cjs` alone is not accepted here: it skips the API contract that callers of `src/money.js` rely on.\n\nAfter your last change to `src/money.js`, run the full check:\n\n    node scripts/check.cjs --all\n\n`scripts/check.cjs` is in this skill\'s directory. It runs the unit tests and the contract check, and prints `CHECK_OK` when both pass.\n'
/** `checkScript` from inside a skill: the project root is four levels above `scripts/`. */
const skillCheckScript = checkScript.replace("const root = path.join(__dirname, '..');", "const root = path.join(__dirname, '..', '..', '..', '..');")
/** Skills that apply to neither skill task, each a short real procedure. */
const DISTRACTOR_SKILLS: Record<string, string> = {
  'docker-deploy': '---\nname: docker-deploy\ndescription: Build the service image and deploy it to staging. Use when asked to deploy, ship, or roll back the service.\n---\n\n# Deploy\n\n1. Build with `docker build -t billing:staging .`.\n2. Push with `docker push registry.internal/billing:staging`.\n3. Roll back by redeploying the previous tag.\n',
  'release-notes': '---\nname: release-notes\ndescription: Draft release notes and CHANGELOG.md entries from merged changes. Use when preparing a release.\n---\n\n# Release notes\n\nGroup entries under Added, Changed, and Fixed, newest release first, one line per change.\n',
  'translations': '---\nname: translations\ndescription: Add or update user-facing strings in locales/*.json. Use when changing text that users see.\n---\n\n# Translations\n\nAdd each new key to every file in `locales/`, keep keys sorted, and leave untranslated values in English.\n',
}
/** The paths of a skill fixture's files, relative to the workspace. */
const skillPath = (name: string, file = 'SKILL.md') => `.agents/skills/${name}/${file}`

const twinCode = 'function roundMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\n\nfunction truncateMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\nmodule.exports = { roundMoney, truncateMoney };\n'
const twinTests = fixedTests('src/money.js').replace("console.log('FIXTURE_PASS');", "const { truncateMoney } = require('./src/money.js');\nassert.equal(truncateMoney(1.239), 1.23); assert.equal(truncateMoney(2.999), 2.99);\nconsole.log('FIXTURE_PASS');")

/** Deterministic checkpoint codes in chain order; long_session uses ten. */
/** A deterministic amount in cents, 100 to 9,099, drawn from its seed. */
const amountOf = (seed: string) => Number.parseInt(hash(seed).slice(0, 6), 16) % 9_000 + 100
/** One `fan_out` ledger: its opening balance and sixty entries, every third a debit. */
function ledgerBook(name: string) {
  const opening = amountOf(`${name}-opening`) * 10
  const entries = Array.from({ length: 60 }, (_, index) => ({ kind: index % 3 === 2 ? 'debit' : 'credit', cents: amountOf(`${name}-${index}`) }))
  return { opening, entries }
}
/** A ledger's text: a header, its opening line, its entries, and a memo after every sixth entry whose figure is not an entry. */
export function ledgerText(name: string): string {
  const { opening, entries } = ledgerBook(name)
  const lines = [`# ${name} ledger, day close 2026-03-14`, `opening ${opening}`]
  entries.forEach((entry, index) => {
    lines.push(`t=${String(index + 1).padStart(3, '0')} ${entry.kind} ${entry.cents} ref ${name.slice(0, 3).toUpperCase()}-${hash(`${name}-ref-${index}`).slice(0, 4)}`)
    if (index % 6 === 5) lines.push(`memo: courier quoted ${amountOf(`${name}-memo-${index}`)} for a replacement, not booked`)
  })
  return lines.join('\n') + '\n'
}
/** The `balances.txt` lines `fan_out` expects: each ledger's balance in order, then their total. */
export function fanOutBalances(): string[] {
  const balances = FAN_OUT_LEDGERS.map(name => {
    const { opening, entries } = ledgerBook(name)
    return entries.reduce((sum, entry) => sum + (entry.kind === 'credit' ? entry.cents : -entry.cents), opening)
  })
  return [...FAN_OUT_LEDGERS.map((name, index) => `${name}=${balances[index]}`), `total=${balances.reduce((sum, balance) => sum + balance, 0)}`]
}
export function checkpointCodes(count = 10): string[] {
  return Array.from({ length: count }, (_, index) => 'CP-' + hash(`checkpoint-${index + 1}`).slice(0, 6).toUpperCase())
}
/**
 * Note paths in chain order. Hashed names reveal the next path only through
 * the preceding note, so the requested chain requires sequential reads.
 */
export function noteNames(count = 10): string[] {
  return Array.from({ length: count }, (_, index) => `notes/ledger-${hash(`note-${index + 1}`).slice(0, 6)}.md`)
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
 * - Each note is about 6 KB, so one read (call, numbered lines, and framing)
 *   is about 1,650 tokens. That is under the 2,560-token retained tail, so the
 *   newest batch stays verbatim and the older ones form a region to summarize.
 *   The prefix, one batch, and the output cap (4,500 + 1,650 + 8,192 = 14,342)
 *   fit the window, and the largest request, one batch past the threshold
 *   (12,800 + 1,650 = 14,450), still leaves room for a reply.
 * - The ten batches total about 16,500 tokens, so the session reaches the
 *   threshold around the fifth note (4,500 + 5 × 1,650 = 12,750) and
 *   compacts at least once before the last.
 * - pi and bake-rs compare the threshold with the provider's reported count,
 *   which runs well below the estimate: a note is about 940 tokens to GPT,
 *   Gemini, and DeepSeek and 1,340 to Claude at 4.4 KB, with a 1,700 to
 *   2,700-token prefix. At 6 KB the ten notes take every one of those models
 *   past 12,800 tokens with at least two notes to spare.
 */
/** The characters of each tool result pi's `serializeConversation` keeps when it builds a summary request. */
export const SUMMARY_TOOL_RESULT_CHARS = 2_000
export const LONG_SESSION_BUDGET = { prefixTokens: 4_500, maxOutputTokens: 8_192, notes: 10 }
/**
 * One note of about 6 KB, under the pruner's 8,192-character threshold, with
 * its code after five paragraphs and the next note's path at the end. The code
 * lies in the first {@link SUMMARY_TOOL_RESULT_CHARS} characters, the part of a
 * tool result that pi's compaction shows its summarizer.
 */
function note(index: number, code: string, next: string | undefined): string {
  const paragraph = (n: number) => `Section ${index}.${n}. The ledger reconciler compares each batch against the upstream journal, marks drifted rows for review, and records the operator who cleared them. Batches older than the retention window are archived with their checksums so a later audit can replay them.\n\n`
  const lines = [`# Ledger notes, part ${String(index).padStart(2, '0')}\n\n`]
  for (let n = 1; n <= 5; n++) lines.push(paragraph(n))
  lines.push(`Checkpoint code: ${code}\n\n`)
  for (let n = 6; n <= 22; n++) lines.push(paragraph(n))
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

/** An MCP server a fixture provides; the runner writes it into the sample's private Bake home as `mcp.json`. */
export interface FixtureMcpServer {
  /** The server's name, which prefixes its model-visible tools. */
  name: string
  /** The program `mcp.json` names, and its arguments. */
  command: string
  args: string[]
  /** The tools it offers, so the runner can check that the model was given `mcp__<name>__<tool>`. */
  tools: string[]
}

/**
 * A built fixture: its workspace, the file the scenario judges, for a
 * read-only scenario the digest it must keep, the hash of each fixture file
 * the agent must not change, such as a check script the predicate trusts, and
 * the MCP server the scenario provides.
 */
export interface Fixture { workspace: string; file: string; digest?: string; protected?: Record<string, string>; mcpServer?: FixtureMcpServer }
/** Options a test passes to shorten a fixture; a live run uses the defaults. */
export interface FixtureOptions { slowCheckMs?: number }

/** The hash of each named file, as it stands now; a path is relative to the workspace, or absolute for a fixture file outside it. */
function protect(workspace: string, paths: readonly string[]): Record<string, string> {
  return Object.fromEntries(paths.map(path => [path, hash(readFileSync(resolve(workspace, path), 'utf8'))]))
}
/** Whether every protected file still exists with the hash it had when the fixture was built; null when none is protected. */
export function fixturesUnchanged(built: Fixture): boolean | null {
  if (built.protected === undefined) return null
  return Object.entries(built.protected).every(([path, digest]) => existsSync(resolve(built.workspace, path)) && hash(readFileSync(resolve(built.workspace, path), 'utf8')) === digest)
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
    case 'skill_check':
    case 'skill_distractors': {
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      const skills: [string, string][] = Object.entries(DISTRACTOR_SKILLS).map(([name, text]) => [skillPath(name), text])
      if (scenario === 'skill_check') skills.push([skillPath('money-check'), moneyCheckSkill], [skillPath('money-check', 'scripts/check.cjs'), skillCheckScript])
      for (const [path, text] of skills) write(path, text)
      return { workspace, file: 'src/money.js', protected: protect(workspace, skills.map(([path]) => path)) }
    }
    case 'edit_recovery':
      write('src/money.js', twinCode); write('test.cjs', twinTests)
      return { workspace, file: 'src/money.js' }
    case 'independent_reads':
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      INDEPENDENT_NOTES.forEach((name, index) => write(name, INDEPENDENT_FACTS[index]! + '\n'))
      return { workspace, file: 'src/money.js', protected: protect(workspace, INDEPENDENT_NOTES) }
    case 'context_masking': {
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      write('notes/requirements.txt', MASKING_FACTS.join('\n') + '\n')
      const diagnostics = Array.from({ length: 20 }, (_, index) => {
        const probe = maskingProbe(index)
        const lines = Array.from({ length: 50 }, (_, line) => `${probe} record ${line}: ${'expected cache and transport diagnostic; '.repeat(3)}`)
        lines.push(`${probe} NEXT: ${index === 19 ? 'DONE' : maskingProbe(index + 1)}`)
        return lines.join('\n')
      }).join('\n') + '\n'
      write('diagnostics.log', diagnostics)
      return { workspace, file: 'src/money.js', protected: protect(workspace, ['notes/requirements.txt', 'diagnostics.log']) }
    }
    case 'mcp_docs': {
      // The server sits beside the workspace, not in it: the agent reads the
      // checklist from the server, not from the file that holds it.
      const serverDir = join(root, 'mcp')
      mkdirSync(serverDir)
      const script = join(serverDir, 'docs-server.cjs')
      copyFileSync(join(import.meta.dir, 'fixtures/mcp-docs-server.cjs'), script)
      const documents = join(serverDir, 'documents.json')
      writeFileSync(documents, JSON.stringify({ server: MCP_DOCS_SERVER, tool: MCP_DOCS_TOOL, instructions: MCP_DOCS_INSTRUCTIONS, documents: MCP_DOCUMENTS }, null, 2) + '\n')
      return {
        workspace, file: 'checklist.txt', protected: protect(workspace, [script, documents]),
        mcpServer: { name: MCP_DOCS_SERVER, command: 'node', args: [script], tools: [MCP_DOCS_TOOL] },
      }
    }
    case 'fan_out': {
      const ledgers = FAN_OUT_LEDGERS.map(name => `ledgers/${name}.log`)
      FAN_OUT_LEDGERS.forEach((name, index) => write(ledgers[index]!, ledgerText(name)))
      return { workspace, file: 'balances.txt', protected: protect(workspace, ledgers) }
    }
    case 'long_session':
    case 'background_compaction': {
      write('src/money.js', baseCode); write('test.cjs', fixedTests('src/money.js'))
      const names = noteNames(scenario === 'background_compaction' ? 20 : 10)
      checkpointCodes(names.length).forEach((code, index) => write(names[index]!, note(index + 1, code, names[index + 1])))
      return { workspace, file: 'src/money.js', protected: protect(workspace, names) }
    }
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
  if (scenario === 'reminder') return { workspace, file: 'reminder.txt' }
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
  /** Calls of `schedule_create`; absent counts as none. */
  scheduleCalls?: number
  /** The marker the stale writer leaves once it has injected its comment. */
  injectionPath: string
}
/** A scenario's verdict, and the file contents, test result, and protected-fixture check it rests on. */
export interface Verdict { validated: boolean; source: string; testsUnchanged: boolean | null; testExit: number | null; fixturesUnchanged: boolean | null }

/**
 * Judge a finished sample against its fixture; the agent's exit code is checked by the caller, except where a predicate names it.
 * @param env - the complete environment of the `node test.cjs` check; omitted, the check inherits this process's environment.
 */
export function validate(scenario: string, built: Fixture, outcome: Outcome, env?: NodeJS.ProcessEnv): Verdict {
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
  if (scenario === 'reminder') {
    const source = existsSync(join(workspace, file)) ? readFileSync(join(workspace, file), 'utf8') : ''
    const validated = outcome.code === 0 && (outcome.scheduleCalls ?? 0) > 0 && /^schedule-\d+$/.test(source.trim())
    return { validated, source, testsUnchanged: null, testExit: null, fixturesUnchanged: null }
  }
  const source = existsSync(join(workspace, file)) ? readFileSync(join(workspace, file), 'utf8') : ''
  if (scenario === 'fan_out') {
    const unchanged = fixturesUnchanged(built)
    const lines = source.split(/\r?\n/).map(line => line.trim()).filter(Boolean)
    const validated = outcome.code === 0 && unchanged !== false && lines.join('\n') === fanOutBalances().join('\n')
    return { validated, source, testsUnchanged: null, testExit: null, fixturesUnchanged: unchanged }
  }
  if (scenario === 'mcp_docs') {
    const unchanged = fixturesUnchanged(built)
    const lines = source.split(/\r?\n/).map(line => line.trim()).filter(Boolean)
    const validated = outcome.code === 0 && unchanged !== false && lines.join('\n') === MCP_CHECKLIST_ITEMS.join('\n')
    return { validated, source, testsUnchanged: null, testExit: null, fixturesUnchanged: unchanged }
  }
  if (scenario === 'explore_answer') {
    return { validated: outcome.final.trim() === EXPLORE_ANSWER && treeDigest(workspace) === built.digest, source, testsUnchanged: null, testExit: null, fixturesUnchanged: null }
  }
  const testsUnchanged = existsSync(join(workspace, 'test.cjs')) && readFileSync(join(workspace, 'test.cjs'), 'utf8') === testsFor(scenario, file)
  // A cold Windows runner took over 5 s to start the first Node process.
  const validation = spawnSync('node', ['test.cjs'], { cwd: workspace, env, stdio: ['ignore', 'pipe', 'pipe'], timeout: 30_000 })
  const testExit = validation.status
  const read = (path: string) => existsSync(join(workspace, path)) ? readFileSync(join(workspace, path), 'utf8') : ''
  const unchanged = fixturesUnchanged(built)
  const validated = validation.status === 0 && testsUnchanged && unchanged !== false
    && (scenario !== 'stale_edit' || (existsSync(outcome.injectionPath) && source.includes('EXTERNAL_CHANGE_KEEP')))
    && (scenario !== 'shell_then_edit' || source.startsWith('// build: 42'))
    && (scenario !== 'background_test' || outcome.final.includes(slowCheckLine()))
    // The stamp is the hash of the source the project check last passed on, so it matches only when that check ran after the last edit.
    && ((scenario !== 'instructions_file' && scenario !== 'skill_check') || read('.check-stamp') === hash(source))
    && (!['long_session', 'background_compaction'].includes(scenario) || read('codes.txt').split(/\r?\n/).map(line => line.trim()).filter(Boolean).join('\n') === checkpointCodes(scenario === 'background_compaction' ? 20 : 10).join('\n'))
    && (scenario !== 'independent_reads' || read('facts.txt').trim() === INDEPENDENT_FACTS.join('\n'))
    && (scenario !== 'context_masking' || read('facts.txt').trim() === MASKING_FACTS.join('\n'))
  return { validated, source, testsUnchanged, testExit, fixturesUnchanged: unchanged }
}
