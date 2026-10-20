#!/usr/bin/env bun
/**
 * Local verification: every gate CI runs, in one command, reporting
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
import { isRustInput } from './rust-scope.ts'

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
  /** Working directory relative to the repository root. */
  readonly cwd?: string
  /** Whether the step reads build output, and is skipped under `--fast`. */
  readonly needsBuild?: boolean
  /** The build that owns this step's artifacts; defaults to the TypeScript `build` step. */
  readonly buildStep?: BuildStep
  /** A command to run, or undefined with a reason to skip it for this run. */
  readonly command?: (options: Options, scope: Scope) => readonly string[] | { readonly skip: string }
  /** A check done in-process instead of a command. */
  readonly inline?: (options: Options, scope: Scope) => Promise<Omit<Result, 'seconds'>>
}

/** A step that produces artifacts other steps read. */
export type BuildStep = 'build' | 'rust'

const bun = (...args: string[]): readonly string[] => ['bun', ...args]

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

/** Whether this run tests the whole Rust workspace instead of the packages the change reaches. */
function rustFull(options: Options, scope: Scope, ci: boolean): boolean {
  return options.full || ci || scope.mergeBase === undefined
}

const RUST_SKIP = { skip: 'no Rust input changed (--full runs it anyway)' } as const

/**
 * The Rust test command for this run: every test, the packages the change reaches
 * and their reverse dependents (see rust-scope.ts), or none when no Rust input changed.
 */
export function rustTestArgs(options: Options, scope: Scope, ci: boolean): readonly string[] | { readonly skip: string } {
  if (rustFull(options, scope, ci)) return bun('run', 'test:rust')
  if (!scope.files.some(isRustInput)) return RUST_SKIP
  return bun('run', 'test:rust', '--changed', scope.mergeBase ?? '')
}

/** Documentation tests run whole, since they are quick, unless no Rust input changed. */
export function rustDocArgs(options: Options, scope: Scope, ci: boolean): readonly string[] | { readonly skip: string } {
  if (!rustFull(options, scope, ci) && !scope.files.some(isRustInput)) return RUST_SKIP
  return ['cargo', 'test', '--workspace', '--locked', '--doc', '--no-fail-fast']
}

const CI = process.env['CI'] === 'true'

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
    ['rust-migration-ledger', 'Rust qualification records are structurally valid and internally consistent'],
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
    name: 'rust-fmt', phase: 'static', group: 'native', cwd: 'rust',
    summary: 'Rust workspace formatting',
    command: () => ['cargo', 'fmt', '--all', '--', '--check'],
  },
  {
    name: 'rust-clippy', phase: 'build', group: 'native', needsBuild: true, cwd: 'rust',
    summary: 'locked Rust workspace lint, including test targets',
    command: () => ['cargo', 'clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings'],
  },
  {
    name: 'rust-test', phase: 'build', group: 'native', needsBuild: true,
    summary: 'Rust tests the change reaches, with parallel execution when cargo-nextest is installed',
    command: (options, scope) => rustTestArgs(options, scope, CI),
  },
  {
    name: 'rust-doc', phase: 'build', group: 'native', needsBuild: true, cwd: 'rust',
    summary: 'locked Rust workspace documentation tests',
    command: (options, scope) => rustDocArgs(options, scope, CI),
  },
  {
    name: 'rust', phase: 'build', group: 'native', needsBuild: true,
    summary: 'locked Rust workspace build for PTY, package, and eval scenarios',
    cwd: 'rust',
    command: () => ['cargo', 'build', '--workspace', '--locked'],
  },
  ...(['peers', 'unit', 'layout', 'docs', 'spec'] as const).map((target): Step => ({
    name: `tui-${target}`, phase: 'tui', group: 'tui',
    summary: `TUI check target \`${target}\``,
    // Component specs mount the built runtime; the rest read sources.
    ...target === 'spec' ? { needsBuild: true } : {},
    command: () => bun('apps/tui/scripts/tui.ts', 'check', target),
  })),
  {
    name: 'runtime', phase: 'runtime', group: 'runtime', needsBuild: true,
    summary: 'shared runtime, CLI, and tooling specs under Node',
    command: runtimeArgs,
  },
  {
    name: 'integration', phase: 'runtime', group: 'runtime', needsBuild: true,
    summary: 'assembled profiles, sandbox backends, and built artifacts (`*.e2e.ts`), keyless',
    // Always whole: these suites boot built output, which no import graph from a changed source reaches.
    command: () => node(VITEST, 'run', '--config', INTEGRATION_CONFIG),
  },
  {
    name: 'e2e', phase: 'e2e', group: 'e2e', needsBuild: true,
    summary: 'keyless PTY scenarios against the built profile',
    command: () => process.platform === 'win32' ? { skip: 'the PTY driver needs a POSIX terminal' }
      : bun('apps/tui/scripts/tui.ts', 'e2e', '--no-build'),
  },
  {
    name: 'rust-pty', phase: 'e2e', group: 'native', needsBuild: true, buildStep: 'rust',
    summary: 'Rust preview and live sessions in a real PTY, or a ConPTY pseudoconsole on Windows: input, resize, and terminal restoration',
    command: () => bun('run', 'test:rust:pty'),
  },
  {
    name: 'rust-package', phase: 'e2e', group: 'native', needsBuild: true, buildStep: 'rust',
    summary: 'native preview archive of the debug bake, unpacked and run without Node or Bun',
    command: () => bun('run', 'test:rust:package'),
  },
  {
    name: 'rust-install', phase: 'e2e', group: 'native', needsBuild: true, buildStep: 'rust',
    summary: 'native release rehearsal: the built bake packed, published, and installed by the shipped installer',
    command: () => process.platform === 'win32'
      ? { skip: 'the native installer rehearsal needs a POSIX shell; install.ps1 is checked on Windows' }
      : bun('run', 'test:rust:install'),
  },
  {
    name: 'rust-eval', phase: 'e2e', group: 'native', needsBuild: true, buildStep: 'rust',
    summary: 'compiled Rust eval fixture arm, tamper rejection, and the keyless native bake arm',
    command: () => bun('run', 'test:rust:eval'),
  },
]

/**
 * A failed artifact producer prevents a dependent check from testing an older build.
 * @returns the step's build when it failed, or undefined.
 */
export function failedBuild(step: Step, results: ReadonlyMap<string, Pick<Result, 'outcome'>>): BuildStep | undefined {
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
    if (options.fast && (step.needsBuild === true || step.group === 'native')) return { step, skip: '--fast' }
    return { step }
  })
}

const USAGE = `usage: bun run preflight [options]

Run every gate CI runs and report all of them. Exit 1 if any failed.

  --base <ref>    measure the change from <ref> (default: origin/develop, else develop)
  --full          run the whole runtime and Rust suites, not only the tests the change reaches
  --fast          skip the build, component specs, runtime suite, and PTY scenarios
  --only <names>  run only these steps or groups (comma-separated, repeatable)
  --skip <names>  leave out these steps or groups
  --list          print the steps and groups
Logs: .preflight/<step>.log; step times: .preflight/summary.json`

/** Run a command and return its stdout; reject when it fails. */
async function capture(argv: readonly string[]): Promise<string> {
  const child = Bun.spawn([...argv], { cwd: ROOT, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', env: process.env })
  const [out, err, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  if (code !== 0) throw new Error(`${argv.join(' ')}: ${err.trim()}`)
  return out
}

/** Run a command with its output written to a log file; resolve with its exit code. */
async function logged(argv: readonly string[], log: string, cwd: string): Promise<number> {
  writeFileSync(log, `$ ${argv.join(' ')}\n`)
  // One append descriptor for both streams keeps them interleaved as the command wrote them.
  const file = openSync(log, 'a')
  try {
    const env = { ...process.env, FORCE_COLOR: '0' }
    const child = Bun.spawn([...argv], { cwd, stdin: 'ignore', stdout: file, stderr: file, env })
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
    // Both sides of a rename, so moving a file out of a tree still changes that tree.
    // Raw NUL-separated paths: git quotes unusual names in its line output.
    const changed = await capture(['git', '-c', 'core.quotepath=off', 'diff', '--name-only', '--no-renames', '-z', mergeBase])
    const untracked = await capture(['git', '-c', 'core.quotepath=off', 'ls-files', '--others', '--exclude-standard', '-z'])
    const files = [...new Set([...changed.split('\0'), ...untracked.split('\0')].filter(Boolean))].sort()
    return { base, mergeBase, files }
  }
  return { base: candidates.join(' or '), mergeBase: undefined, files: [] }
}

/** Execute one gate once; a nonzero child exit always fails the gate. */
export async function runStep(step: Step, options: Options, scope: Scope, logDirectory = LOG_DIR): Promise<Result> {
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
  const log = join(logDirectory, `${step.name}.log`)
  const code = await logged(command, log, resolve(ROOT, step.cwd ?? '.'))
  const note = `.preflight/${step.name}.log`
  return { outcome: code === 0 ? 'pass' : 'fail', seconds: seconds(), note }
}

/** One step's line in `.preflight/summary.json`. */
export interface SummaryEntry {
  readonly step: string
  readonly outcome: Outcome
  readonly seconds: number
  readonly note?: string
}

/** The machine-readable record of a run, in step order, for CI timing summaries. */
export function summaryRecord(
  results: ReadonlyMap<string, Result>, seconds: number,
): { readonly seconds: number; readonly steps: readonly SummaryEntry[] } {
  const round = (value: number) => Math.round(value * 10) / 10
  const steps = STEPS.flatMap((step): SummaryEntry[] => {
    const result = results.get(step.name)
    if (result === undefined) return []
    const entry = { step: step.name, outcome: result.outcome, seconds: round(result.seconds) }
    return [result.note === undefined ? entry : { ...entry, note: result.note }]
  })
  return { seconds: round(seconds), steps }
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
  for (const [name, result] of failed) {
    if (result.note?.endsWith('.log') !== true) continue
    console.log(`\n--- ${name}: last lines of ${result.note}`)
    console.log(tail(result.note, CI ? 400 : 40))
  }
  const elapsed = (performance.now() - started) / 1000
  writeFileSync(join(LOG_DIR, 'summary.json'), `${JSON.stringify(summaryRecord(results, elapsed), null, 2)}\n`)
  const counted = (outcome: Outcome) => [...results.values()].filter(result => result.outcome === outcome).length
  console.log(`\npreflight: ${counted('pass')} passed, ${counted('fail')} failed, ${counted('warn')} warned, ${counted('skip')} skipped in ${elapsed.toFixed(0)}s`)
  return failed.length === 0 ? 0 : 1
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
