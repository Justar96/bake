#!/usr/bin/env bun
/**
 * Local pre-PR verification: every gate CI runs, in one command, reporting
 * all of them rather than stopping at the first failure.
 *
 * Steps run in phases. Static checks that neither build nor share output run
 * concurrently; the build, the Node test suites, and the PTY scenarios run one
 * at a time because they share `lib/`, temporary trees, and every core. Each
 * step writes its output to `.preflight/<step>.log`; a failing step's tail is
 * printed with the summary. The same step table drives CI through `--only`.
 */
import { closeSync, mkdirSync, openSync, readFileSync, writeFileSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { dropRepositoryGitEnv } from './git-env.ts'

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const LOG_DIR = join(ROOT, '.preflight')

/** What a step concluded. `warn` reports without failing the run. */
export type Outcome = 'pass' | 'fail' | 'warn' | 'skip'

/** Where a step runs relative to the others. */
export type Phase = 'static' | 'build' | 'tui' | 'runtime' | 'e2e'

/** The change a run verifies, relative to its base. */
export interface Scope {
  /** Base the change is measured from, as given. */
  readonly base: string
  /** Merge base of the base and HEAD, or undefined when it cannot be resolved. */
  readonly mergeBase: string | undefined
  /** Repository paths changed since the merge base: committed, staged, unstaged, and untracked. */
  readonly files: readonly string[]
}

/** Options read from the command line. */
export interface Options {
  readonly base: string | undefined
  /** Run the whole runtime suite instead of the tests related to the change. */
  readonly full: boolean
  /** Skip the build and everything that needs it. */
  readonly fast: boolean
  readonly only: readonly string[]
  readonly skip: readonly string[]
  readonly list: boolean
  readonly help: boolean
}

/** A step's result, with how long it took. */
export interface Result {
  readonly outcome: Outcome
  readonly seconds: number
  /** One line shown beside the outcome: a skip reason, a warning, or the log path. */
  readonly note?: string
}

/** One gate. */
export interface Step {
  readonly name: string
  readonly phase: Phase
  /** Selectable group; `--only` and `--skip` accept a group or a step name. */
  readonly group: 'hygiene' | 'generated' | 'types' | 'lint' | 'unit' | 'build' | 'tui' | 'runtime' | 'e2e' | 'native'
  readonly summary: string
  /** Whether the step reads build output, and is skipped under `--fast`. */
  readonly needsBuild?: boolean
  /** Build that owns this step's artifacts; defaults to the TypeScript `build` step. */
  readonly buildStep?: 'build' | 'rust'
  /** A command to run, or undefined with a reason to skip it for this run. */
  readonly command?: (options: Options, scope: Scope) => readonly string[] | { readonly skip: string }
  /** A check done in-process instead of a command. */
  readonly inline?: (options: Options, scope: Scope) => Promise<Omit<Result, 'seconds'>>
  /**
   * For a Vitest step, the command that reruns some of its test files alone.
   * A file that failed in the loaded run and passes alone is reported, not
   * failed: a real-process test can miss a deadline on a busy machine.
   */
  readonly rerun?: (files: readonly string[]) => readonly string[]
}

const bun = (...args: string[]): readonly string[] => ['bun', ...args]

/** Past this many failed files a rerun is not a load check; something is broken. */
const RERUN_LIMIT = 12

/** What a failed Vitest run blames. */
export interface VitestFailures {
  /** Test files that failed, could not start, or were blamed for an unhandled error. */
  readonly files: readonly string[]
  /** Unhandled errors Vitest could not tie to a test file, which no rerun can clear. */
  readonly unattributed: number
}

const TEST_FILE = String.raw`\S+?\.(?:spec|test|e2e)\.[cm]?tsx?`

/**
 * What a Vitest run failed on, read from its uncoloured output.
 * @param log - the run's output.
 */
export function vitestFailures(log: string): VitestFailures {
  const files = new Set<string>()
  const add = (pattern: RegExp) => { for (const match of log.matchAll(pattern)) if (match[1] !== undefined) files.add(match[1]) }
  add(new RegExp(String.raw`^ FAIL {2}(${TEST_FILE})\b`, 'gmu'))
  add(new RegExp(String.raw`originated in "(${TEST_FILE})" test file`, 'gu'))
  add(new RegExp(String.raw`Failed to start \w+ worker for test files (${TEST_FILE})`, 'gu'))
  // Each unhandled error is a block under its own rule; a block naming no test file is unattributed.
  const blocks = log.split(/^⎯+ (?:Unhandled Errors?|Uncaught Exception|Unhandled Rejection) ⎯+$/mu).slice(1)
  const unattributed = blocks.filter(block => !/originated in "|Failed to start \w+ worker for test files/u.test(block)
    && /\S/u.test(block.replace(/^\s*Vitest caught \d+ unhandled errors?[^\n]*\n(?:[^\n]*\n)?/u, ''))).length
  return { files: [...files].sort(), unattributed }
}

/** The Bun-run tests under a directory, as `./`-prefixed paths `bun test` reads as files. */
export function bunTests(directory: string): string[] {
  return [...new Bun.Glob('**/*.test.ts').scanSync({ cwd: join(ROOT, directory) })]
    .map(path => path.replaceAll('\\', '/'))
    .filter(path => !path.split('/').includes('node_modules'))
    .map(path => `./${directory}/${path}`).sort()
}
const node = (...args: string[]): readonly string[] => ['node', ...args]
const VITEST = 'node_modules/vitest/vitest.mjs'
const INTEGRATION_CONFIG = 'vitest.e2e.config.ts'

/** Paths whose change can move any runtime test, so a scoped run widens to the whole suite. */
const RUNTIME_WIDE = /^(package\.json|bun\.lock|vitest\.(config|shared)\.ts|tsconfig[^/]*\.json)$/u

/** Source trees a runtime change lives in; others (docs, the TUI's own tree) have their own gates. */
const RUNTIME_SOURCE = /^(packages|apps\/cli|scripts|vendor)\//u

/** Paths a user-visible change lives in, which should come with a CHANGELOG entry. */
const SHIPPED_SOURCE = /^(packages|apps)\/(?:[^/]+\/)+src\//u

/**
 * Compiled output left in a source tree, which the tsconfig `src` aliases load
 * instead of the `.ts` module beside it. `.gitignore` hides these files, so
 * nothing else reports them.
 */
export function strayBuildOutput(ignored: readonly string[]): string[] {
  return ignored.filter(path => /^(packages|apps|vendor)\/(?:.*\/)?src\/.*\.(js|d\.ts)$/u.test(path)
    && !/\/(node_modules|lib)\//u.test(path)
    && !/\/(css-modules|asset-imports)\.d\.ts$/u.test(path))
}

/**
 * Whether the change lacks a CHANGELOG entry it probably needs.
 * @returns the shipped paths that changed, empty when none did or CHANGELOG.md changed too.
 */
export function changelogGap(files: readonly string[]): string[] {
  if (files.includes('CHANGELOG.md')) return []
  return files.filter(path => SHIPPED_SOURCE.test(path) && !/\/tests?\//u.test(path))
}

/**
 * The runtime-suite arguments for this run: all of it, the tests related to
 * the change, or none when the change touches no runtime source.
 */
export function runtimeArgs(options: Options, scope: Scope): readonly string[] | { readonly skip: string } {
  if (options.full) return node(VITEST, 'run')
  if (scope.mergeBase === undefined) return node(VITEST, 'run')
  if (scope.files.some(path => RUNTIME_WIDE.test(path))) return node(VITEST, 'run')
  if (!scope.files.some(path => RUNTIME_SOURCE.test(path))) return { skip: 'no runtime source changed (--full runs it anyway)' }
  // Vitest follows the import graph from each changed file to the tests that reach it.
  return node(VITEST, 'run', '--changed', scope.mergeBase)
}

export const STEPS: readonly Step[] = [
  {
    name: 'stray-output', phase: 'static', group: 'hygiene',
    summary: 'no compiled .js/.d.ts shadowing sources under src/',
    inline: async () => {
      const listed = await capture(['git', 'ls-files', '--others', '--ignored', '--exclude-standard', '--directory'])
      const stray = strayBuildOutput(listed.split('\n').filter(Boolean))
      if (stray.length === 0) return { outcome: 'pass' }
      return { outcome: 'fail', note: `${stray.length} file(s), e.g. ${stray[0]}; delete them (they load instead of the .ts beside them)` }
    },
  },
  {
    name: 'whitespace', phase: 'static', group: 'hygiene',
    summary: 'no whitespace errors in the change',
    command: (_options, scope) => scope.mergeBase === undefined ? { skip: 'no merge base' } : ['git', 'diff', '--check', scope.mergeBase],
  },
  {
    name: 'rescope-vendor', phase: 'static', group: 'hygiene',
    summary: 'vendored Cordis names and exact edits stay scoped',
    command: () => bun('scripts/rescope-vendor.ts', '--check'),
  },
  {
    name: 'changelog', phase: 'static', group: 'hygiene',
    summary: 'a shipped change comes with a CHANGELOG [Unreleased] entry',
    inline: async (_options, scope) => {
      const gap = changelogGap(scope.files)
      return gap.length === 0 ? { outcome: 'pass' }
        : { outcome: 'warn', note: `${gap.length} shipped source file(s) changed without CHANGELOG.md, e.g. ${gap[0]}` }
    },
  },
  ...([
    ['workspace', 'workspace manifests match the generator'],
    ['tsconfig-paths', 'tsconfig path aliases match the workspace'],
    ['config-catalog', 'docs/config-catalog.md matches the config schemas'],
    ['tool-catalog', 'docs/tool-catalog.md matches the shipped tools'],
    ['cordis-catalog', 'Cordis API catalog matches the services'],
    ['doc-graphs', 'event and dependency graphs in docs match the source'],
    ['module-graph', 'module graph artifacts match the source'],
    ['persistence-catalog', 'persistence catalog and schema match the persisted types'],
    ['type-equiv', 'types pasted in docs match their declarations'],
    ['cordis-config', 'Loader rows keep static metadata and resolve from their owner'],
    ['package-invariants', 'invariant companions are wired, or their omission is explained'],
    ['rust-migration-inventory', 'Rust migration ownership covers the current packages, profiles, tools, and tests'],
  ] as const).map(([name, summary]): Step => ({
    name: `verify-${name}`, phase: 'static', group: 'generated', summary,
    command: () => bun('run', `verify-${name}`),
  })),
  {
    name: 'typecheck', phase: 'static', group: 'types',
    summary: 'strict types for the runtime and the TUI',
    command: () => bun('run', 'typecheck'),
  },
  {
    name: 'lint', phase: 'static', group: 'lint',
    summary: 'Oxlint over apps, packages, and scripts',
    command: () => bun('run', 'lint'),
  },
  {
    name: 'actionlint', phase: 'static', group: 'lint',
    summary: 'GitHub workflow syntax and expressions',
    command: () => Bun.which('actionlint') === null ? { skip: 'actionlint 1.7.12+ is not on PATH' } : ['actionlint', '-no-color'],
  },
  {
    name: 'scripts-unit', phase: 'static', group: 'unit',
    summary: 'workspace, generator, and release tooling tests under bun test',
    // Named files: a `bun test` filter also matches `.spec.` files, which run on Node.
    // `--parallel` gives each file its own global; the timeout is the Vitest budget these tests had.
    command: () => bun('test', '--parallel', '--timeout=30000', ...bunTests('scripts')),
  },
  {
    name: 'evals-unit', phase: 'static', group: 'unit',
    summary: 'eval runner unit tests and fixture dry checks under bun test',
    // A separate run: sharing the scripts-unit worker left it spinning in a synchronous spawn on Linux CI.
    command: () => bun('test', '--parallel', '--timeout=30000', ...bunTests('evals')),
  },
  {
    name: 'build', phase: 'build', group: 'build', needsBuild: true,
    summary: 'native addon, runtime libraries, and the TUI bundle',
    command: () => bun('run', 'build'),
  },
  {
    name: 'rust', phase: 'build', group: 'native', needsBuild: true,
    summary: 'locked Rust preview format, lint, tests, and build',
    command: () => bun('run', 'check:rust'),
  },
  ...(['peers', 'unit', 'layout', 'docs', 'spec'] as const).map((target): Step => ({
    name: `tui-${target}`, phase: 'tui', group: 'tui',
    summary: `TUI check target \`${target}\``,
    // Component specs mount the built runtime; the rest read sources.
    ...target === 'spec' ? {
      needsBuild: true,
      rerun: (files: readonly string[]) => node(VITEST, 'run', '--config', 'apps/tui/vitest.config.ts', '--maxWorkers=1', ...files),
    } : {},
    command: () => bun('apps/tui/scripts/tui.ts', 'check', target),
  })),
  {
    name: 'runtime', phase: 'runtime', group: 'runtime', needsBuild: true,
    summary: 'shared runtime, CLI, and tooling specs under Node',
    command: runtimeArgs,
    rerun: files => node(VITEST, 'run', '--maxWorkers=1', ...files),
  },
  {
    name: 'integration', phase: 'runtime', group: 'runtime', needsBuild: true,
    summary: 'assembled profiles, sandbox backends, and built artifacts (`*.e2e.ts`), keyless',
    // Always whole: these suites boot built output, which no import graph from a changed source reaches.
    command: () => node(VITEST, 'run', '--config', INTEGRATION_CONFIG),
    rerun: files => node(VITEST, 'run', '--config', INTEGRATION_CONFIG, '--maxWorkers=1', ...files),
  },
  {
    name: 'e2e', phase: 'e2e', group: 'e2e', needsBuild: true,
    summary: 'keyless PTY scenarios against the built profile',
    command: () => process.platform === 'win32' ? { skip: 'the PTY driver needs a POSIX terminal' }
      : bun('apps/tui/scripts/tui.ts', 'e2e', '--no-build'),
  },
  {
    name: 'rust-pty', phase: 'e2e', group: 'native', needsBuild: true, buildStep: 'rust',
    summary: 'Rust preview input, resize, inspection, and terminal restoration',
    command: () => process.platform === 'win32' ? { skip: 'native ConPTY scenarios are not implemented' }
      : bun('run', 'test:rust:pty'),
  },
]

/** A failed artifact producer prevents a dependent check from testing an older build. */
export function failedBuild(step: Step, results: ReadonlyMap<string, Pick<Result, 'outcome'>>): string | undefined {
  if (step.needsBuild !== true || step.phase === 'build') return undefined
  const build = step.buildStep ?? 'build'
  return results.get(build)?.outcome === 'fail' ? build : undefined
}

/** Parse the command line. */
export function parseOptions(argv: readonly string[]): Options {
  let base: string | undefined
  let full = false, fast = false, list = false, help = false
  const only: string[] = [], skip: string[] = []
  const rest = [...argv]
  for (let arg = rest.shift(); arg !== undefined; arg = rest.shift()) {
    const value = (): string => {
      const next = rest.shift()
      if (next === undefined || next.startsWith('--')) throw new Error(`preflight: ${arg} needs a value`)
      return next
    }
    if (arg === '--base') base = value()
    else if (arg === '--full') full = true
    else if (arg === '--fast') fast = true
    else if (arg === '--only') only.push(...value().split(','))
    else if (arg === '--skip') skip.push(...value().split(','))
    else if (arg === '--list') list = true
    else if (arg === '--help' || arg === '-h') help = true
    else throw new Error(`preflight: unknown argument ${arg}`)
  }
  const known = new Set(STEPS.flatMap(step => [step.name, step.group]))
  const unknown = [...only, ...skip].filter(name => !known.has(name))
  if (unknown.length > 0) throw new Error(`preflight: unknown step or group ${unknown.join(', ')} (see --list)`)
  return { base, full, fast, only, skip, list, help }
}

/**
 * The steps this run executes, and why the others are left out.
 * @returns every step in order, each either selected or with its skip reason.
 */
export function selectSteps(options: Options): readonly { readonly step: Step; readonly skip?: string }[] {
  return STEPS.map((step) => {
    const named = (names: readonly string[]) => names.includes(step.name) || names.includes(step.group)
    if (options.only.length > 0 && !named(options.only)) return { step, skip: 'not selected' }
    if (named(options.skip)) return { step, skip: '--skip' }
    if (options.fast && step.needsBuild === true) return { step, skip: '--fast' }
    return { step }
  })
}

const USAGE = `usage: bun run preflight [options]

Run every gate CI runs and report all of them. Exit 1 if any failed.

  --base <ref>    measure the change from <ref> (default: origin/develop, else develop)
  --full          run the whole runtime suite, not only the tests the change reaches
  --fast          skip the build, component specs, runtime suite, and PTY scenarios
  --only <names>  run only these steps or groups (comma-separated, repeatable)
  --skip <names>  leave out these steps or groups
  --list          print the steps and groups
Logs: .preflight/<step>.log`

/** Run a command and return its stdout; reject when it fails. */
async function capture(argv: readonly string[]): Promise<string> {
  const child = Bun.spawn([...argv], { cwd: ROOT, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', env: process.env })
  const [out, err, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  if (code !== 0) throw new Error(`${argv.join(' ')}: ${err.trim()}`)
  return out
}

/** Run a command with its output written to a log file; resolve with its exit code. */
async function logged(argv: readonly string[], log: string): Promise<number> {
  writeFileSync(log, `$ ${argv.join(' ')}\n`)
  // One append descriptor for both streams keeps them interleaved as the command wrote them.
  const file = openSync(log, 'a')
  try {
    const env = { ...process.env, FORCE_COLOR: '0' }
    const child = Bun.spawn([...argv], { cwd: ROOT, stdin: 'ignore', stdout: file, stderr: file, env })
    await child.exited
    return child.exitCode ?? 1
  } catch (error) {
    writeFileSync(file, `\n${String(error)}\n`)
    return 127
  } finally {
    closeSync(file)
  }
}

async function resolveScope(options: Options): Promise<Scope> {
  const candidates = options.base === undefined ? ['origin/develop', 'develop'] : [options.base]
  for (const base of candidates) {
    const mergeBase = await capture(['git', 'merge-base', base, 'HEAD']).then(out => out.trim(), () => undefined)
    if (mergeBase === undefined) continue
    const changed = await capture(['git', 'diff', '--name-only', mergeBase])
    const untracked = await capture(['git', 'ls-files', '--others', '--exclude-standard'])
    const files = [...new Set([...changed.split('\n'), ...untracked.split('\n')].filter(Boolean))].sort()
    return { base, mergeBase, files }
  }
  return { base: candidates.join(' or '), mergeBase: undefined, files: [] }
}

async function runStep(step: Step, options: Options, scope: Scope): Promise<Result> {
  const started = performance.now()
  const seconds = () => (performance.now() - started) / 1000
  if (step.inline !== undefined) {
    try {
      return { ...await step.inline(options, scope), seconds: seconds() }
    } catch (error) {
      return { outcome: 'fail', seconds: seconds(), note: error instanceof Error ? error.message : String(error) }
    }
  }
  const command = step.command?.(options, scope) ?? { skip: 'nothing to run' }
  if ('skip' in command) return { outcome: 'skip', seconds: 0, note: command.skip }
  const log = join(LOG_DIR, `${step.name}.log`)
  const code = await logged(command, log)
  const note = `.preflight/${step.name}.log`
  if (code === 0) return { outcome: 'pass', seconds: seconds(), note }
  if (step.rerun === undefined) return { outcome: 'fail', seconds: seconds(), note }
  const { files: failed, unattributed } = vitestFailures(readFileSync(log, 'utf8'))
  // A rerun of the named files cannot clear an error no file was blamed for.
  if (unattributed > 0 || failed.length === 0 || failed.length > RERUN_LIMIT) return { outcome: 'fail', seconds: seconds(), note }
  const again = `.preflight/${step.name}.rerun.log`
  if (await logged(step.rerun(failed), join(ROOT, again)) !== 0) return { outcome: 'fail', seconds: seconds(), note: again }
  return { outcome: 'warn', seconds: seconds(),
    note: `${failed.length} file(s) failed only under load, passing alone: ${failed.join(', ')} (${note})` }
}

/** Run up to `limit` tasks at once. */
async function pooled(tasks: readonly (() => Promise<void>)[], limit: number): Promise<void> {
  const queue = [...tasks]
  await Promise.all(Array.from({ length: Math.min(limit, queue.length) }, async () => {
    for (let task = queue.shift(); task !== undefined; task = queue.shift()) await task()
  }))
}

const MARK: Record<Outcome, string> = { pass: 'PASS', fail: 'FAIL', warn: 'WARN', skip: 'SKIP' }

function tail(path: string, lines: number): string {
  try {
    return readFileSync(join(ROOT, path), 'utf8').trimEnd().split('\n').slice(-lines).join('\n')
  } catch {
    return ''
  }
}

async function main(argv: readonly string[]): Promise<number> {
  // A pre-push hook exports GIT_DIR; tests that build fixture repositories
  // would otherwise rewrite the repository being pushed. See git-env.ts.
  dropRepositoryGitEnv(process.env)
  let options: Options
  try {
    options = parseOptions(argv)
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    return 2
  }
  if (options.help) { console.log(USAGE); return 0 }
  if (options.list) {
    for (const step of STEPS) console.log(`${step.group.padEnd(10)} ${step.name.padEnd(26)} ${step.summary}`)
    return 0
  }
  mkdirSync(LOG_DIR, { recursive: true })
  const scope = await resolveScope(options)
  const selected = selectSteps(options)
  console.log(scope.mergeBase === undefined
    ? `preflight: no merge base with ${scope.base}; running every selected step in full`
    : `preflight: ${scope.files.length} file(s) changed since ${scope.base} (${scope.mergeBase.slice(0, 10)})`)
  const results = new Map<string, Result>()
  const started = performance.now()
  const report = (step: Step, result: Result): void => {
    results.set(step.name, result)
    const time = result.outcome === 'skip' ? '' : `${result.seconds.toFixed(1)}s`
    console.log(`  ${MARK[result.outcome]}  ${step.name.padEnd(26)} ${time.padStart(7)}  ${result.outcome === 'pass' ? '' : result.note ?? ''}`)
  }
  const phases: readonly Phase[] = ['static', 'build', 'tui', 'runtime', 'e2e']
  // Static steps read sources and their own temporary trees; four at a time
  // keeps tsc and the generators from starving each other.
  const width = Math.max(1, Math.min(4, Math.floor(availableParallelism() / 4)))
  for (const phase of phases) {
    const inPhase = selected.filter(entry => entry.step.phase === phase)
    const tasks = inPhase.map(entry => async () => {
      if (entry.skip !== undefined) {
        if (entry.skip !== 'not selected') report(entry.step, { outcome: 'skip', seconds: 0, note: entry.skip })
        return
      }
      // Nothing after a failed build can be trusted to test this tree.
      const failed = failedBuild(entry.step, results)
      if (failed !== undefined) {
        report(entry.step, { outcome: 'skip', seconds: 0, note: `the ${failed} step failed` })
        return
      }
      // A step run alone can take minutes; say what is running meanwhile.
      if (phase !== 'static' && phase !== 'tui') console.log(`  ....  ${entry.step.name}`)
      report(entry.step, await runStep(entry.step, options, scope))
    })
    await pooled(tasks, phase === 'static' || phase === 'tui' ? width : 1)
  }
  const failed = [...results.entries()].filter(([, result]) => result.outcome === 'fail')
  const ci = process.env['CI'] === 'true'
  for (const [name, result] of failed) {
    if (result.note?.endsWith('.log') !== true) continue
    console.log(`\n--- ${name}: last lines of ${result.note}`)
    console.log(tail(result.note, ci ? 400 : 40))
  }
  const counted = (outcome: Outcome) => [...results.values()].filter(result => result.outcome === outcome).length
  console.log(`\npreflight: ${counted('pass')} passed, ${counted('fail')} failed, ${counted('warn')} warned, ${counted('skip')} skipped in ${((performance.now() - started) / 1000).toFixed(0)}s`)
  return failed.length === 0 ? 0 : 1
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
