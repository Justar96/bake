#!/usr/bin/env bun
/**
 * The D18 performance harness: measures fresh terminal processes of a native
 * `bake` binary and of a built Bake 0.3 checkout by one external method, and
 * proposes budgets from a baseline report.
 *
 *   bun scripts/rust-perf.ts --arm native:rust/target/release/bake --arm ts:../bake-v0.3.8 [options]
 *   bun scripts/rust-perf.ts budgets --baseline <report.json> [--arm <label>] [--out <file>]
 *
 * Run options:
 *   --arm <kind:path>    native:<bake binary> or ts:<built v0.3.8 worktree>; repeatable
 *   --workload <name>    fresh, small, typical, tail, tools, or large-output; repeatable, default all
 *   --samples <n>        measured samples per arm, workload, and cache mode (default 20)
 *   --mode <mode>        cold, warm, or both (default both)
 *   --out <file>         report path (default .preflight/perf/<time>.json)
 *   --max-load <load>    refuse to start above this 1-minute load average (default max(1, CPUs / 4))
 *   --idle-ms <ms>       idle CPU window after the streamed turn (default 5000)
 *   --pace-ms <ms>       interval between the faux model's deltas (default 10)
 *   --timeout-ms <ms>    deadline of each wait in a sample (default 120000)
 *   --shutdown-timeout-ms <ms>  deadline of the exit after SIGTERM, after which the tree is killed and the sample fails (default 10000)
 *   --node <path>        Node executable for ts arms (default node)
 *   --allow-non-release  measure a native binary that is not under target/release/, for a run that sets no budget
 *
 * The first SIGINT or SIGTERM stops the run after the current sample's
 * cleanup and writes the report; a second one kills every in-flight process,
 * removes the temporary directories, and exits at once.
 *
 * See scripts/rust-perf/README.md for the method.
 */
import { rmSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { cpus, tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { armSlug, createArm, parseArm, type Arm } from './rust-perf/arms.ts'
import { proposeBudgets, refuseDocsPath } from './rust-perf/budgets.ts'
import { DEFAULT_PACE_MS, PacedModel } from './rust-perf/model.ts'
import { clockTicksPerSecond } from './rust-perf/proc-tree.ts'
import { harnessMetadata, hostMetadata, REPORT_SCHEMA, summarize, type Method, type Report } from './rust-perf/report.ts'
import { abandonSamples, CACHE_MODES, loadAverage, PREPARE_ITERATION, runSample, WARMUP_ITERATION, type CacheMode, type SampleRecord, type WarmState } from './rust-perf/sample.ts'
import { COLUMNS, ROWS } from './rust-perf/terminal.ts'
import { isWorkloadName, WORKLOADS, type WorkloadName } from './rust-perf/workloads.ts'

const repository = resolve(import.meta.dir, '..')

function positiveInteger(name: string, text: string): number {
  const value = Number(text)
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be a positive integer, got ${JSON.stringify(text)}`)
  return value
}

async function writeAtomically(path: string, text: string): Promise<void> {
  await mkdir(dirname(path), { recursive: true })
  await writeFile(`${path}.tmp`, text)
  await rename(`${path}.tmp`, path)
}

async function budgets(argv: string[]): Promise<number> {
  const { values } = parseArgs({ args: argv, options: { baseline: { type: 'string' }, arm: { type: 'string' }, out: { type: 'string' } }, strict: true })
  if (values.baseline === undefined) throw new Error('budgets needs --baseline <report.json>')
  const reportPath = resolve(values.baseline)
  const bytes = await readFile(reportPath)
  const proposal = proposeBudgets(JSON.parse(bytes.toString('utf8')) as Report, bytes, reportPath, values.arm)
  const out = resolve(values.out ?? join(repository, '.preflight/perf', `budgets-proposal-${new Date().toISOString().replace(/[:.]/gu, '-')}.json`))
  refuseDocsPath(out, repository)
  await writeAtomically(out, `${JSON.stringify(proposal, null, 2)}\n`)
  console.log(`budget proposal written to ${out}; it is not a budget until the owner approves it`)
  for (const warning of proposal.warnings) console.log(`warning: ${warning}`)
  return 0
}

async function measure(argv: string[]): Promise<number> {
  const { values } = parseArgs({
    args: argv, strict: true,
    options: {
      arm: { type: 'string', multiple: true }, workload: { type: 'string', multiple: true }, samples: { type: 'string', default: '20' },
      mode: { type: 'string', default: 'both' }, out: { type: 'string' }, 'max-load': { type: 'string' }, 'idle-ms': { type: 'string', default: '5000' },
      'pace-ms': { type: 'string', default: String(DEFAULT_PACE_MS) }, 'timeout-ms': { type: 'string', default: '120000' }, 'shutdown-timeout-ms': { type: 'string', default: '10000' },
      node: { type: 'string', default: 'node' }, 'allow-non-release': { type: 'boolean', default: false },
    },
  })
  if (process.platform !== 'linux') throw new Error('rust-perf reads /proc and runs on Linux only')
  const specs = (values.arm ?? []).map(text => parseArm(text))
  if (specs.length === 0) throw new Error('name at least one --arm native:<path> or ts:<worktree>')
  if (new Set(specs.map(spec => spec.label)).size !== specs.length) throw new Error('each --arm may be named once')
  const workloads = (values.workload ?? Object.keys(WORKLOADS)).map((name) => {
    if (!isWorkloadName(name)) throw new Error(`unknown workload ${name}; choose from ${Object.keys(WORKLOADS).join(', ')}`)
    return name
  })
  const modes: readonly CacheMode[] = values.mode === 'both' ? CACHE_MODES : values.mode === 'cold' || values.mode === 'warm' ? [values.mode] : (() => { throw new Error('--mode must be cold, warm, or both') })()
  const samples = positiveInteger('--samples', values.samples)
  const idleMs = positiveInteger('--idle-ms', values['idle-ms'])
  const paceMs = positiveInteger('--pace-ms', values['pace-ms'])
  const timeoutMs = positiveInteger('--timeout-ms', values['timeout-ms'])
  const shutdownTimeoutMs = positiveInteger('--shutdown-timeout-ms', values['shutdown-timeout-ms'])
  const maxLoad = values['max-load'] === undefined ? Math.max(1, cpus().length / 4) : Number(values['max-load'])
  if (!(maxLoad > 0)) throw new Error('--max-load must be a positive number')
  const out = resolve(values.out ?? join(repository, '.preflight/perf', `${new Date().toISOString().replace(/[:.]/gu, '-')}.json`))
  refuseDocsPath(out, repository)
  const arms: Arm[] = specs.map(spec => createArm(spec, { node: values.node, allowNonRelease: values['allow-non-release'] }))
  for (const arm of arms) arm.check()
  const startLoad = loadAverage()
  if (startLoad > maxLoad) {
    console.error(`refusing to measure: the 1-minute load average is ${startLoad}, above --max-load ${maxLoad}; another heavy process is running`)
    return 2
  }
  const clockTicks = clockTicksPerSecond()
  const method: Method = {
    samplesPerCell: samples,
    order: 'Iterations are outermost; within one, each workload and cache mode runs every arm once, the arm order reversed on odd iterations. Every sample is a fresh process with a freshly seeded session.',
    viewport: { columns: COLUMNS, rows: ROWS }, paceMs, idleMs, rssIntervalMs: 10, timeoutMs, shutdownTimeoutMs, maxLoad,
    cache: {
      cold: 'A new empty BAKE_HOME, user home, and private TMPDIR (Node\'s compile cache lives there) per sample, so the arm sets up its home on every launch. The kernel page cache is not dropped: that needs privileges, so the seeded files and the binary may still be cached.',
      warm: 'Per arm, one unmeasured launch of the fresh workload prepares a BAKE_HOME, kept without its sessions. Every warm sample starts from a copy of it at the same path, with its own seeded session, a new user home, and the TMPDIR the arm\'s warm samples share, after one discarded warm-up launch per arm and workload.',
    },
    endpoints: 'Times are from the monotonic clock at output arrival in the PTY, not terminal paint. Spawn is the handshake that lets the shell exec the program. See scripts/rust-perf/README.md.',
  }
  const builds = arms.map(arm => ({ label: arm.spec.label, kind: arm.spec.kind, build: arm.build() }))
  for (const { label, build } of builds) {
    if (build.budgetEligible === false) console.log(`warning: ${label} is a ${typeof build.profile === 'string' ? `${build.profile} build` : 'build of unknown profile'}; this report sets no D18 budget`)
  }
  const records: SampleRecord[] = []
  const aborter = new AbortController()
  const sharedCaches = await mkdtemp(join(tmpdir(), 'bake-perf-warm-'))
  const interrupt = (): void => {
    if (!aborter.signal.aborted) {
      console.error('rust-perf: interrupted; cleaning up the current sample (interrupt again to exit at once)')
      aborter.abort(new Error('rust-perf interrupted'))
      return
    }
    abandonSamples()
    rmSync(sharedCaches, { recursive: true, force: true })
    process.exit(130)
  }
  process.on('SIGINT', interrupt)
  process.on('SIGTERM', interrupt)
  const model = new PacedModel(paceMs)
  const save = async (): Promise<void> => {
    const report: Report = {
      schema: REPORT_SCHEMA, createdAt: new Date().toISOString(), command: process.argv.slice(1), interrupted: aborter.signal.aborted,
      host: hostMetadata(clockTicks, startLoad), harness: harnessMetadata(repository), method, arms: builds, workloads, modes,
      summary: summarize(records), samples: records,
    }
    await writeAtomically(out, `${JSON.stringify(report, null, 2)}\n`)
  }
  const settings = { idleMs, timeoutMs, shutdownTimeoutMs, clockTicks, signal: aborter.signal }
  const warmStates: WarmState[] = arms.map((arm, index) => {
    const slug = join(sharedCaches, armSlug(arm.spec, index))
    return { cache: slug, home: `${slug}.home`, template: `${slug}.template` }
  })
  type Sample = (index: number, workload: WorkloadName, mode: CacheMode, iteration: number, warmup: boolean) => Promise<SampleRecord>
  const sample: Sample = async (index, workload, mode, iteration, warmup) => {
    const arm = arms[index]
    const warm = warmStates[index]
    if (arm === undefined || warm === undefined) throw new Error(`no arm ${index}`)
    const key = { arm: arm.spec.label, workload, mode, iteration, warmup }
    const record = await runSample(arm, key, model, warm, settings, iteration === PREPARE_ITERATION)
    records.push(record)
    const summary = record.ok ? Object.entries(record.metrics).map(([name, value]) => `${name}=${value === null ? '-' : Number(value.toFixed(2))}`).join(' ') : `FAILED ${record.error.split('\n')[0]}`
    const phase = iteration === PREPARE_ITERATION ? 'warm-home preparation ' : warmup ? 'warm-up ' : ''
    const leaks = record.leakedProcesses.length > 0 ? ` leaked: ${record.leakedProcesses.join(', ')}` : ''
    console.log(`${phase}${arm.spec.label} ${workload} ${mode} #${iteration}: ${summary}${leaks}`)
    await save()
    return record
  }
  try {
    if (modes.includes('warm')) {
      for (let index = 0; index < arms.length; index++) {
        const prepared = await sample(index, 'fresh', 'warm', PREPARE_ITERATION, true)
        if (!prepared.ok) throw new Error(`could not prepare the warm home of ${prepared.arm}: ${prepared.error.split('\n')[0]}`)
      }
      for (const workload of workloads) for (let index = 0; index < arms.length; index++) await sample(index, workload, 'warm', WARMUP_ITERATION, true)
    }
    for (let iteration = 0; iteration < samples; iteration++) {
      for (const workload of workloads) {
        for (const mode of modes) {
          const order = arms.map((_, index) => index)
          if (iteration % 2 === 1) order.reverse()
          for (const index of order) await sample(index, workload, mode, iteration, false)
        }
      }
    }
  } catch (error) {
    if (!aborter.signal.aborted) throw error
  } finally {
    process.off('SIGINT', interrupt)
    process.off('SIGTERM', interrupt)
    await model.stop()
    await rm(sharedCaches, { recursive: true, force: true })
    await save()
  }
  const failed = records.filter(record => !record.ok).length
  console.log(`report written to ${out}: ${records.length} sample(s), ${failed} failed${aborter.signal.aborted ? ', interrupted' : ''}`)
  if (model.unexpected.length > 0) console.log(`the faux model refused ${model.unexpected.length} request(s): ${[...new Set(model.unexpected)].join(', ')}`)
  return aborter.signal.aborted || failed > 0 ? 1 : 0
}

const [command, ...rest] = process.argv.slice(2)
try {
  process.exitCode = command === 'budgets' ? await budgets(rest) : await measure(command === 'run' ? rest : process.argv.slice(2))
} catch (error) {
  console.error(`rust-perf: ${error instanceof Error ? error.message : String(error)}`)
  process.exitCode = 2
}
