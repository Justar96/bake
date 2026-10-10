#!/usr/bin/env bun
/**
 * Runs the cross-runtime Session write-lease and resume specs under Node
 * against the built Rust lease probe, and accepts the run only when Vitest's
 * JSON report shows every expected case of both ran.
 *
 * The specs skip themselves unless `BAKE_RUST_LEASE_PROBE` or its
 * `DSH_RUST_LEASE_PROBE` spelling names the probe. The runtime suite's setup
 * clears every `BAKE_*` name, so the launcher sets both for its child only.
 * It never builds the probe or the Node libraries; a missing artifact fails
 * with the command that produces it. Because an opt-in that silently stops
 * applying would skip every case and still exit 0, the launcher checks the
 * report itself instead of trusting the exit code (see {@link reportProblems}).
 *
 * The launcher owns one private temporary directory for the report and for
 * its children's `TMPDIR`, `TMP`, and `TEMP`, and removes it after they
 * settle. On POSIX each child runs in its own process group, so cancellation
 * and the final reap reach the holders and writers Vitest starts; on Windows
 * cancellation terminates the child's process tree with `taskkill /T /F`.
 *
 * Usage: `bun scripts/rust-lease-interop.ts [--probe <path>]`
 */
import { spawn, spawnSync, type ChildProcess, type StdioOptions } from 'node:child_process'
import { statSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { isDeepStrictEqual, parseArgs } from 'node:util'

const ROOT = resolve(import.meta.dir, '..')
const PACKAGE = join(ROOT, 'packages/session/session-persistence-jsonl')
const POSIX = process.platform !== 'win32'
export const DEFAULT_PROBE = join(ROOT, 'rust/target/debug', `bake-session-lease-probe${POSIX ? '' : '.exe'}`)
/** Built packages the specs' Node holder and writer import, resolved as they resolve them. */
const BUILT_IMPORTS = ['@deepseek-ai/cordis', 'bake-session', 'bake-session-persistence', 'bake-session-persistence-jsonl'] as const
/** Budget for the built-library import check, which loads a few modules and the flock addon. */
const IMPORT_CHECK_MS = 30_000
/** How long a cancelled process tree gets to exit after SIGTERM before SIGKILL. */
const STOP_GRACE_MS = 10_000
/** How long a process group may take to empty after its leader exits and the rest are killed. */
const REAP_MS = 5_000
const STDERR_LIMIT = 16 * 1024

/** One spec the launcher runs: its path, its suite title when it runs, and the cases it must report. */
export interface InteropSpec {
  /** The spec, relative to the repository root. */
  readonly spec: string
  /** The spec's suite title when it runs, as opposed to its opt-in skip title. */
  readonly suite: string
  /** Every case the spec must report, each exactly once. A renamed case must be renamed here too. */
  readonly cases: readonly string[]
  /**
   * Cases Windows skips, having no `SIGSTOP`, mode-based directory permissions,
   * or link-then-remove publication; every other platform must pass them.
   */
  readonly posixOnly: readonly string[]
}

const LEASE_POSIX_ONLY = [
  'a stopped Rust holder still refuses TypeScript, and only its death permits takeover (POSIX only)',
  'a stopped Node holder still refuses Rust, and only its death permits takeover (POSIX only)',
] as const

/** Write-lease exclusion and takeover between the runtimes. */
export const LEASE_SPEC: InteropSpec = {
  spec: 'packages/session/session-persistence-jsonl/tests/lease.cross-runtime.spec.ts',
  suite: 'cross-runtime write lease',
  cases: [
    'a live Rust holder refuses a TypeScript writer with the exact owned error while reads continue, and its crash permits TypeScript takeover',
    'a Rust holder\'s graceful release permits a TypeScript writer to append seq 2',
    'a live Node holder refuses a Rust writer with exit 3 and the exact message, and its crash permits Rust takeover',
    'a Node holder\'s graceful release permits a Rust writer to append seq 2',
    'an in-process TypeScript write handle refuses a Rust writer, and its close permits Rust takeover',
    'a Rust hold-open of a TypeScript-created log refuses TypeScript, and its release permits TypeScript takeover',
    'a Rust hold-open of a TypeScript-created log refuses TypeScript, and its crash permits TypeScript takeover',
    ...LEASE_POSIX_ONLY,
  ],
  posixOnly: LEASE_POSIX_ONLY,
}

const RESUME_POSIX_ONLY = [
  'a Rust writer killed after linking its migrated log leaves the temporary file as its second link, and TypeScript then Rust resume the log (POSIX only)',
  'a TypeScript writer killed after linking its migrated log leaves the temporary file as its second link, and Rust then TypeScript resume the log (POSIX only)',
  'a stopped Rust migration still refuses TypeScript, and only its death lets TypeScript migrate (POSIX only)',
  'a stopped TypeScript migration still refuses Rust, and only its death lets Rust migrate (POSIX only)',
  'a read-only Session directory refuses both runtimes\' migrations with a permission error and leaves the tree unchanged (POSIX only)',
] as const

/**
 * One runtime resuming a log the other wrote, left torn, or migrated (D31),
 * and migrations that compete, are killed, or are refused alike.
 */
export const RESUME_SPEC: InteropSpec = {
  spec: 'packages/session/session-persistence-jsonl/tests/resume.cross-runtime.spec.ts',
  suite: 'cross-runtime resume',
  cases: [
    'a TypeScript writer killed mid-append leaves a torn tail that Rust truncates and resumes, to the bytes TypeScript reads back',
    'a Zstd log a killed TypeScript writer left torn is refused by Rust with TypeScript\'s own message and kept byte for byte, and TypeScript resumes it',
    'a Rust writer killed mid-append leaves a torn tail that TypeScript truncates and resumes, to the bytes Rust resumes after',
    'a Rust writer killed while publishing a new Session leaves only its temporary file, beside which TypeScript creates the Session and Rust resumes it',
    'a v0 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged',
    'a v1 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged',
    'a v2 log migrated by Rust or by TypeScript resumes in the other runtime to the same bytes, its source unchanged',
    'the runtimes migrate one v0 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'the runtimes migrate one v0 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'the runtimes migrate one v1 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'the runtimes migrate one v1 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'the runtimes migrate one v2 log concurrently: a Rust migration paused before publication holds the lock, TypeScript is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'the runtimes migrate one v2 log concurrently: a TypeScript migration paused before publication holds the lock, Rust is refused, and both resume the one target TypeScript\'s migration writes, its source unchanged',
    'a Rust writer killed while writing its migration temporary file leaves only that file, beside which TypeScript migrates and Rust resumes',
    'a TypeScript writer killed while writing its migration temporary file leaves only that file, beside which Rust migrates and TypeScript resumes',
    ...RESUME_POSIX_ONLY,
  ],
  posixOnly: RESUME_POSIX_ONLY,
}

/** The specs one run covers, in the order Vitest is given them. */
export const SPECS: readonly InteropSpec[] = [LEASE_SPEC, RESUME_SPEC]

const record = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value)

/** Why one test file's entry does not show a complete run of `plan` on `platform`. */
function fileProblems(file: Record<string, unknown>, plan: InteropSpec, windows: boolean): string[] {
  const problems: string[] = []
  if (file['status'] !== 'passed') problems.push(`${plan.spec}: the test file's status is ${JSON.stringify(file['status'])}`)
  const cases = file['assertionResults']
  if (!Array.isArray(cases)) return [...problems, `${plan.spec}: the test file lists no cases`]
  const expected = new Set<string>(plan.cases)
  const posixOnly = new Set<string>(plan.posixOnly)
  const seen = new Set<string>()
  for (const entry of cases) {
    if (!record(entry) || typeof entry['title'] !== 'string') { problems.push(`${plan.spec}: a case has no title`); continue }
    const title = entry['title']
    const status = entry['status']
    if (seen.has(title)) problems.push(`case reported more than once: ${title}`)
    seen.add(title)
    if (!expected.has(title)) problems.push(`unexpected case (${String(status)}): ${title}`)
    if (!isDeepStrictEqual(entry['ancestorTitles'], [plan.suite])) problems.push(`case outside the "${plan.suite}" suite: ${JSON.stringify(entry['ancestorTitles'])} ${title}`)
    const skippable = windows && posixOnly.has(title)
    const ok = skippable ? status === 'skipped' || status === 'pending' : status === 'passed'
    if (!ok) problems.push(`case ${String(status)}, expected ${skippable ? 'skipped' : 'passed'}: ${title}`)
  }
  for (const title of plan.cases) if (!seen.has(title)) problems.push(`case missing from the report: ${title}`)
  return problems
}

/**
 * Why a Vitest JSON report does not show a complete run of `specs` on
 * `platform`: one file per spec, each with exactly its cases under its suite,
 * every case passed except each spec's POSIX-only cases on Windows, which
 * must be skipped, and totals that agree.
 * @returns the problems found, empty when the report is acceptable.
 */
export function reportProblems(report: unknown, platform: NodeJS.Platform, specs: readonly InteropSpec[] = SPECS): string[] {
  if (!record(report)) return ['the report is not a JSON object']
  const problems: string[] = []
  const windows = platform === 'win32'
  const files = report['testResults']
  if (!Array.isArray(files) || files.length !== specs.length || !files.every(record)) {
    return [`the report must hold exactly ${specs.length} test files, got ${Array.isArray(files) ? files.length : 'none'}`]
  }
  const unmatched = [...files]
  for (const plan of specs) {
    const index = unmatched.findIndex(file => typeof file['name'] === 'string' && file['name'].replaceAll('\\', '/').endsWith(`/${plan.spec}`))
    const [file] = index < 0 ? [] : unmatched.splice(index, 1)
    if (file === undefined) { problems.push(`the report has no test file ${plan.spec}`); continue }
    problems.push(...fileProblems(file, plan, windows))
  }
  for (const file of unmatched) problems.push(`the report's test file ${JSON.stringify(file['name'])} is not an interop spec`)
  const total = specs.reduce((sum, plan) => sum + plan.cases.length, 0)
  const skipped = windows ? specs.reduce((sum, plan) => sum + plan.posixOnly.length, 0) : 0
  const totals = {
    numTotalTests: total, numPassedTests: total - skipped,
    numPendingTests: skipped, numFailedTests: 0, numTodoTests: 0, success: true,
  }
  for (const [key, value] of Object.entries(totals)) {
    if (report[key] !== value) problems.push(`${key} is ${JSON.stringify(report[key])}, expected ${String(value)}`)
  }
  return problems
}

/** A missing or unusable artifact; reported with exit code 2. */
class SetupError extends Error {}

/** How an owned child ended. */
interface Settled { readonly code: number | null; readonly signal: NodeJS.Signals | null; readonly error?: Error }

/** Whether any process remains in a POSIX process group. */
function groupAlive(pgid: number): boolean {
  try {
    process.kill(-pgid, 0)
    return true
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM'
  }
}

/**
 * A child and every process it starts. On POSIX the child leads its own
 * process group; on Windows its tree is found through parent ids.
 */
class OwnedTree {
  readonly child: ChildProcess
  private readonly closed: Promise<Settled>
  private killTimer: ReturnType<typeof setTimeout> | undefined
  private exited = false

  constructor(argv: readonly [string, ...string[]], cwd: string, env: NodeJS.ProcessEnv, stdio: StdioOptions) {
    // A detached child is not in the terminal's foreground group, so it must
    // not read the terminal; nothing it runs reads stdin.
    this.child = spawn(argv[0], argv.slice(1), { cwd, env, stdio, detached: POSIX, windowsHide: true })
    this.closed = new Promise<Settled>((done) => {
      let error: Error | undefined
      // A child that never started emits no `close` everywhere, and has nothing left to wait for.
      this.child.once('error', (cause) => {
        error = cause
        if (this.child.pid === undefined) done({ code: null, signal: null, error })
      })
      this.child.once('close', (code, signal) => done({ code, signal, ...error === undefined ? {} : { error } }))
    }).then((settled) => { this.exited = true; return settled })
  }

  /** Terminate the tree: SIGTERM to the group, then SIGKILL; on Windows, terminate the tree at once. */
  stop(): void {
    const pid = this.child.pid
    if (pid === undefined || this.exited) return
    if (!POSIX) {
      // Outcome unchecked: the tree may already be gone, as a signal to an empty group may fail.
      spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true })
      return
    }
    this.signalGroup('SIGTERM')
    this.killTimer ??= setTimeout(() => this.signalGroup('SIGKILL'), STOP_GRACE_MS)
  }

  private signalGroup(signal: NodeJS.Signals): void {
    const pid = this.child.pid
    if (pid === undefined) return
    try { process.kill(-pid, signal) } catch { /* the group is already gone */ }
  }

  /**
   * Resolve once the child has closed and, on POSIX, its group is empty:
   * processes left behind are killed, and a group that will not empty throws.
   */
  async settle(): Promise<Settled> {
    try {
      const settled = await this.closed
      const pid = this.child.pid
      if (POSIX && pid !== undefined && groupAlive(pid)) {
        this.signalGroup('SIGKILL')
        const deadline = performance.now() + REAP_MS
        while (groupAlive(pid) && performance.now() < deadline) await new Promise(done => setTimeout(done, 20))
        if (groupAlive(pid)) throw new Error(`processes of group ${pid} survived SIGKILL`)
        return { ...settled, error: settled.error ?? new Error(`processes of group ${pid} outlived ${String(this.child.spawnargs[0])} and were killed`) }
      }
      return settled
    } finally {
      clearTimeout(this.killTimer)
    }
  }
}

/** Stops the current tree on SIGINT or SIGTERM and remembers that the run was cancelled. */
class Cancellation {
  signal: NodeJS.Signals | undefined
  private current: OwnedTree | undefined
  private readonly onSignal = (signal: NodeJS.Signals): void => {
    this.signal ??= signal
    this.current?.stop()
  }

  constructor() {
    process.on('SIGINT', this.onSignal)
    process.on('SIGTERM', this.onSignal)
  }

  /** Run a tree to completion; a signal already received stops it at once. */
  async run(tree: OwnedTree): Promise<Settled> {
    this.current = tree
    if (this.signal !== undefined) tree.stop()
    try {
      return await tree.settle()
    } finally {
      this.current = undefined
    }
  }

  dispose(): void {
    process.off('SIGINT', this.onSignal)
    process.off('SIGTERM', this.onSignal)
  }
}

function requireProbe(probe: string): void {
  let file
  try { file = statSync(probe) } catch {
    throw new SetupError(`Rust lease probe not found at ${probe}; build it with \`cd rust && cargo build --locked -p bake-conformance\``)
  }
  if (!file.isFile()) throw new SetupError(`Rust lease probe at ${probe} is not a file`)
}

/** Import the built libraries under Node from the spec's package, as its holder process does. */
async function requireBuiltLibraries(cancellation: Cancellation, env: NodeJS.ProcessEnv): Promise<void> {
  const source = `for (const name of ${JSON.stringify(BUILT_IMPORTS)}) await import(name)`
  const tree = new OwnedTree(['node', '--input-type=module', '--eval', source], PACKAGE, env, ['ignore', 'ignore', 'pipe'])
  let stderr = ''
  tree.child.stderr?.setEncoding('utf8').on('data', (chunk: string) => { if (stderr.length < STDERR_LIMIT) stderr += chunk })
  const timer = setTimeout(() => tree.stop(), IMPORT_CHECK_MS)
  const result = await cancellation.run(tree).finally(() => clearTimeout(timer))
  if (cancellation.signal !== undefined) return
  if (result.code === null && result.signal === null && result.error !== undefined) throw new SetupError(`could not start node: ${result.error.message}`)
  if (result.code !== 0 || result.error !== undefined) {
    const reason = result.error?.message ?? result.signal ?? `exit ${result.code}`
    throw new SetupError(`the built Node libraries do not load (${reason}); run \`bun run build:runtime\`\n${stderr.slice(0, STDERR_LIMIT).trimEnd()}`)
  }
}

/** Run the focused specs under Node with the probe selected for that child alone, then check its report. */
async function runSpecs(cancellation: Cancellation, env: NodeJS.ProcessEnv, reportPath: string): Promise<number> {
  const argv = ['node', join(ROOT, 'node_modules/vitest/vitest.mjs'), 'run', ...SPECS.map(plan => plan.spec),
    '--reporter=default', '--reporter=json', `--outputFile.json=${reportPath}`] as const
  const result = await cancellation.run(new OwnedTree(argv, ROOT, env, ['ignore', 'inherit', 'inherit']))
  if (cancellation.signal !== undefined) return 1
  if (result.code === null && result.signal === null && result.error !== undefined) throw new SetupError(`could not start node: ${result.error.message}`)
  if (result.error !== undefined) {
    console.error(`rust lease interop: ${result.error.message}`)
    return 1
  }
  if (result.code !== 0) return result.code ?? 1
  let report: unknown
  try {
    report = JSON.parse(await readFile(reportPath, 'utf8'))
  } catch (error) {
    console.error(`rust lease interop: Vitest exited 0 but its JSON report is missing or unreadable: ${(error as Error).message}`)
    return 1
  }
  const problems = reportProblems(report, process.platform)
  if (problems.length === 0) return 0
  console.error(`rust lease interop: Vitest exited 0, but the report does not show a complete run:\n${problems.map(problem => `  - ${problem}`).join('\n')}`)
  return 1
}

const HELP = `usage: bun scripts/rust-lease-interop.ts [--probe <path>]

Run ${SPECS.map(plan => plan.spec).join('\nand ')}
under Node against the built Rust lease probe, and require Vitest's JSON
report to show every expected case passed (on Windows, the POSIX-only
cases skipped). Build the probe with
\`cd rust && cargo build --locked -p bake-conformance\` and the Node libraries
with \`bun run build:runtime\` first; this script builds neither.

  --probe <path>  lease probe (default rust/target/debug/bake-session-lease-probe)

Exit 0 when the run is complete and passes, 1 when it fails or is
incomplete, 2 for a missing artifact or invalid arguments, 130 or 143 when
cancelled.`

async function main(argv: readonly string[]): Promise<number> {
  let probe = DEFAULT_PROBE
  try {
    const { values } = parseArgs({ args: [...argv], options: { probe: { type: 'string' }, help: { type: 'boolean' } }, strict: true })
    if (values.help === true) { console.log(HELP); return 0 }
    if (values.probe !== undefined) probe = resolve(values.probe)
  } catch (error) {
    console.error(`${(error as Error).message}; see --help`)
    return 2
  }
  const cancellation = new Cancellation()
  let run: string | undefined
  let code = 1
  try {
    requireProbe(probe)
    run = await mkdtemp(join(tmpdir(), 'bake-lease-interop-'))
    const temp = join(run, 'tmp')
    await mkdir(temp)
    const env = { ...process.env, TMPDIR: temp, TMP: temp, TEMP: temp }
    if (cancellation.signal === undefined) await requireBuiltLibraries(cancellation, env)
    code = cancellation.signal === undefined
      ? await runSpecs(cancellation, { ...env, BAKE_RUST_LEASE_PROBE: probe, DSH_RUST_LEASE_PROBE: probe }, join(run, 'report.json'))
      : 1
  } catch (error) {
    // A setup problem exits 2; anything else, such as a process group that
    // survived SIGKILL, fails the run after the directory is removed.
    console.error(`rust lease interop: ${error instanceof Error ? error.message : String(error)}`)
    code = error instanceof SetupError ? 2 : 1
  } finally {
    // Every child has settled by now, so nothing still writes beneath the run directory.
    if (run !== undefined) {
      await rm(run, { recursive: true, force: true, maxRetries: 5 }).catch((error: unknown) => {
        console.error(`rust lease interop: could not remove ${run}: ${(error as Error).message}`)
        code = code === 0 ? 1 : code
      })
    }
    cancellation.dispose()
  }
  if (cancellation.signal !== undefined) return cancellation.signal === 'SIGINT' ? 130 : 143
  return code
}

if (import.meta.main) process.exitCode = await main(process.argv.slice(2))
