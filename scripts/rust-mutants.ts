#!/usr/bin/env bun
/**
 * Run cargo-mutants on the Rust workspace: by default on the Rust lines a
 * branch changes since it left develop, including uncommitted and untracked
 * files, or on chosen files or packages. Settings shared with a plain
 * `cargo mutants` run live in rust/.cargo/mutants.toml.
 *
 * Each mutant runs the tests of the package it mutates, not the workspace's,
 * so a mutant only another crate's tests would catch reads as missed. By
 * default cargo-mutants tests in scratch copies of rust/ under
 * rust/target/mutants, each built cold; `--in-place` mutates the checkout and
 * reuses rust/target instead, one mutant at a time.
 */
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { isAbsolute, join, relative, resolve, sep } from 'node:path'
import { parseArgs } from 'node:util'

const REPO_ROOT = resolve(import.meta.dirname, '..')
const RUST_ROOT = join(REPO_ROOT, 'rust')
/** Ignored by the repository's `rust/target/` entry. */
const OUTPUT_ROOT = join(RUST_ROOT, 'target', 'mutants')

/** The package whose engine-sharing test binary must not run one process per test. */
export const CODEMODE_PACKAGE = 'bake-codemode'
const CODEMODE_DIR = 'crates/bake-codemode/'

/** Bases tried in order when `--base` is not given. */
export const DEFAULT_BASES = ['origin/develop', 'develop'] as const

const USAGE = `usage: bun run mutants:rust [--base <ref>] [--file <path>]... [-p <crate>]... [--jobs N] [--in-place] [--strict] [--list]

Without --file or -p, mutates the Rust lines changed since the merge-base of
HEAD with --base (default origin/develop, then develop), including uncommitted
and untracked files. --file and -p mutate whole files or packages instead; add
--base to keep only their changed lines.

  --file <path>   a source file or glob, relative to the repository or rust/
  -p <crate>      a workspace package
  --jobs N        parallel scratch copies of the workspace (default: CPUs / 8, 1 to 4)
  --in-place      mutate the checkout itself and reuse rust/target; serial
  --strict        exit 1 when a mutant is missed or times out
  --list          list the mutants in scope without testing them`

export interface MutantsOptions {
  /** An explicit diff base; undefined means the default base or no diff. */
  readonly base: string | undefined
  /** Source files or globs, relative to rust/. */
  readonly files: readonly string[]
  readonly packages: readonly string[]
  readonly jobs: number
  readonly inPlace: boolean
  readonly strict: boolean
  readonly list: boolean
}

/** Scratch copies build the workspace cold, so a few at a time is enough. */
export function defaultJobs(cpus: number): number {
  return Math.max(1, Math.min(4, Math.floor(cpus / 8)))
}

/** Map a path given from the repository root, from rust/, or absolutely, to rust/-relative form. */
export function workspacePath(path: string, repoRoot: string): string {
  const rustRoot = join(repoRoot, 'rust')
  const normalized = isAbsolute(path) ? relative(rustRoot, path).split(sep).join('/') : path.replaceAll('\\', '/')
  const withoutDot = normalized.startsWith('./') ? normalized.slice(2) : normalized
  return withoutDot.startsWith('rust/') ? withoutDot.slice('rust/'.length) : withoutDot
}

export function parseMutantsArgs(args: readonly string[], cpus: number, repoRoot: string): MutantsOptions {
  const { values, positionals } = parseArgs({
    args: [...args],
    allowPositionals: true,
    options: {
      base: { type: 'string' },
      file: { type: 'string', multiple: true },
      package: { type: 'string', short: 'p', multiple: true },
      jobs: { type: 'string', short: 'j' },
      'in-place': { type: 'boolean' },
      strict: { type: 'boolean' },
      list: { type: 'boolean' },
    },
  })
  if (positionals.length > 0) throw new Error(`unexpected argument: ${positionals[0] ?? ''}\n${USAGE}`)
  const inPlace = values['in-place'] === true
  let jobs = inPlace ? 1 : defaultJobs(cpus)
  if (values.jobs !== undefined) {
    if (!/^[1-9]\d*$/u.test(values.jobs)) throw new Error(`--jobs needs a positive integer, not ${values.jobs}`)
    jobs = Number(values.jobs)
    // cargo-mutants copies the tree for every job after the first.
    if (inPlace && jobs > 1) throw new Error('--in-place mutates the one checkout, so it runs with --jobs 1')
  }
  return {
    base: values.base,
    files: (values.file ?? []).map(path => workspacePath(path, repoRoot)),
    packages: values.package ?? [],
    jobs,
    inPlace,
    strict: values.strict === true,
    list: values.list === true,
  }
}

/** Whether the run compares against a base: always unless files or packages were chosen without one. */
export function usesDiff(options: MutantsOptions): boolean {
  return options.base !== undefined || (options.files.length === 0 && options.packages.length === 0)
}

/** The rust/-relative Rust files a unified diff leaves in the tree. */
export function changedRustFiles(diff: string): string[] {
  const files = new Set<string>()
  for (const line of diff.split('\n')) {
    const match = /^\+\+\+ b\/(.+\.rs)$/u.exec(line)
    if (match?.[1] !== undefined) files.add(match[1])
  }
  return [...files].sort()
}

export interface MutantsRun {
  /** Also the output subdirectory under rust/target/mutants. */
  readonly name: 'workspace' | 'codemode'
  /** Arguments after `cargo`. */
  readonly args: readonly string[]
}

const isCodemodePath = (path: string): boolean => path.startsWith(CODEMODE_DIR)
/**
 * A glob that does not name one crate directory, such as `**\/lib.rs` or
 * `crates/*\/src/lib.rs`, can match files in either run.
 */
const mayMatchCodemode = (path: string): boolean =>
  isCodemodePath(path) || !/^(crates|plugins)\//u.test(path) || /[*?[{]/u.test(path.split('/').slice(0, 2).join('/'))

/**
 * Split the scope into a nextest run of every package but bake-codemode and a
 * `cargo test` run of bake-codemode, keeping only the runs with something in scope.
 * @param diffFiles the changed Rust files when the run follows a diff.
 */
export function planMutantsRuns(
  options: MutantsOptions, diffPath: string | undefined, diffFiles: readonly string[] | undefined, outputRoot: string,
): MutantsRun[] {
  const otherPackages = options.packages.filter(name => name !== CODEMODE_PACKAGE)
  const codemodeFiles = options.files.filter(mayMatchCodemode)
  const otherFiles = options.files.filter(path => !isCodemodePath(path))
  let workspace = options.packages.length === 0 || otherPackages.length > 0
  let codemode = options.packages.length === 0 || options.packages.includes(CODEMODE_PACKAGE)
  if (options.files.length > 0) {
    workspace &&= otherFiles.length > 0
    codemode &&= codemodeFiles.length > 0
  }
  if (diffFiles !== undefined) {
    workspace &&= diffFiles.some(path => !isCodemodePath(path))
    codemode &&= diffFiles.some(isCodemodePath)
  }
  const common = (name: MutantsRun['name'], files: readonly string[]): string[] => [
    'mutants',
    ...diffPath === undefined ? [] : ['--in-diff', diffPath],
    ...files.flatMap(path => ['--file', path]),
    ...options.list ? ['--list'] : [
      '--jobs', String(options.jobs),
      '--output', join(outputRoot, name),
      ...options.inPlace ? ['--in-place'] : [],
    ],
  ]
  const runs: MutantsRun[] = []
  if (workspace) {
    runs.push({
      name: 'workspace',
      args: [
        ...common('workspace', otherFiles),
        ...otherPackages.flatMap(name => ['--package', name]),
        '--exclude', `${CODEMODE_DIR}**`,
        // A package without tests then reports its mutants as missed instead of failing the baseline.
        // The workspace nextest profile sets fail-fast = false, and plain --fail-fast still waits
        // for running tests; either way a mutant that fails some tests and hangs others is
        // reported as a timeout instead of caught. Stop and kill the run at its first failure.
        ...options.list ? [] : ['--cargo-test-arg=--no-tests=pass', '--cargo-test-arg=--max-fail=1:immediate'],
      ],
    })
  }
  if (codemode) {
    runs.push({
      name: 'codemode',
      // Under nextest every codemode test compiles the shared Wasm engine again.
      args: [...common('codemode', codemodeFiles), '--package', CODEMODE_PACKAGE, '--test-tool', 'cargo'],
    })
  }
  return runs
}

export interface MutantsSummary {
  readonly caught: number
  readonly missed: number
  readonly timeout: number
  readonly unviable: number
  /** `rust/<file>:<line>:<column>: <mutation>` for each missed mutant. */
  readonly missedMutants: readonly string[]
  readonly timeoutMutants: readonly string[]
}

const EMPTY_SUMMARY: MutantsSummary = { caught: 0, missed: 0, timeout: 0, unviable: 0, missedMutants: [], timeoutMutants: [] }

function count(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : 0
}

/** Read one run's `mutants.out`; a run that found no mutants in scope leaves none. */
export function readMutantsOutput(outDir: string): MutantsSummary {
  const outcomesPath = join(outDir, 'outcomes.json')
  if (!existsSync(outcomesPath)) return EMPTY_SUMMARY
  const outcomes: unknown = JSON.parse(readFileSync(outcomesPath, 'utf8'))
  const record = typeof outcomes === 'object' && outcomes !== null ? outcomes as Record<string, unknown> : {}
  const names = (file: string): string[] => existsSync(join(outDir, file))
    ? readFileSync(join(outDir, file), 'utf8').split('\n').filter(line => line.length > 0).map(line => `rust/${line}`)
    : []
  return {
    caught: count(record['caught']),
    missed: count(record['missed']),
    timeout: count(record['timeout']),
    unviable: count(record['unviable']),
    missedMutants: names('missed.txt'),
    timeoutMutants: names('timeout.txt'),
  }
}

export function mergeSummaries(summaries: readonly MutantsSummary[]): MutantsSummary {
  return summaries.reduce((total, next) => ({
    caught: total.caught + next.caught,
    missed: total.missed + next.missed,
    timeout: total.timeout + next.timeout,
    unviable: total.unviable + next.unviable,
    missedMutants: [...total.missedMutants, ...next.missedMutants],
    timeoutMutants: [...total.timeoutMutants, ...next.timeoutMutants],
  }), EMPTY_SUMMARY)
}

export function formatSummary(summary: MutantsSummary): string {
  return [
    `mutants:rust: ${String(summary.caught)} caught, ${String(summary.missed)} missed, `
      + `${String(summary.timeout)} timeout, ${String(summary.unviable)} unviable`,
    ...summary.missedMutants.map(name => `MISSED   ${name}`),
    ...summary.timeoutMutants.map(name => `TIMEOUT  ${name}`),
  ].join('\n')
}

/** cargo-mutants exits 2 when it misses a mutant and 3 on a timeout; other codes mean the run itself failed. */
export function mutantsExitCode(runCodes: readonly number[], summary: MutantsSummary, strict: boolean): number {
  const failure = runCodes.find(code => code !== 0 && code !== 2 && code !== 3)
  if (failure !== undefined) return failure
  return strict && summary.missed + summary.timeout > 0 ? 1 : 0
}

type Git = (args: readonly string[]) => { readonly code: number; readonly stdout: string; readonly stderr: string }

function gitIn(cwd: string): Git {
  return (args) => {
    const result = Bun.spawnSync(['git', '-C', cwd, ...args], { env: { ...process.env, LC_ALL: 'C' } })
    return { code: result.exitCode, stdout: result.stdout.toString(), stderr: result.stderr.toString() }
  }
}

/** The merge-base of HEAD with the given base, or with the first default base that exists. */
export function resolveMergeBase(git: Git, base: string | undefined): { readonly base: string; readonly mergeBase: string } {
  const candidates = base === undefined ? DEFAULT_BASES : [base]
  for (const candidate of candidates) {
    const result = git(['merge-base', 'HEAD', candidate])
    if (result.code === 0) return { base: candidate, mergeBase: result.stdout.trim() }
  }
  throw new Error(`mutants:rust: no merge-base with ${candidates.join(' or ')}; pass --base <ref>`)
}

/**
 * The workspace's changes since the merge-base as one rust/-relative unified
 * diff: committed, staged, and unstaged edits, and untracked Rust files.
 */
export function workspaceDiff(git: Git, mergeBase: string): string {
  const prefixes = ['--no-color', '--no-ext-diff', '--src-prefix=a/', '--dst-prefix=b/']
  const tracked = git(['diff', ...prefixes, '--relative', mergeBase, '--', '.'])
  if (tracked.code !== 0) throw new Error(`mutants:rust: git diff failed: ${tracked.stderr}`)
  const untracked = git(['ls-files', '--others', '--exclude-standard', '-z', '--', '*.rs'])
  if (untracked.code !== 0) throw new Error(`mutants:rust: git ls-files failed: ${untracked.stderr}`)
  const added = untracked.stdout.split('\0').filter(path => path.length > 0).map((path) => {
    // `--no-index` exits 1 when the files differ, which a new file always does.
    const result = git(['diff', ...prefixes, '--no-index', '--', '/dev/null', path])
    if (result.code !== 1) throw new Error(`mutants:rust: git diff of untracked ${path} failed: ${result.stderr}`)
    return result.stdout
  })
  return [tracked.stdout, ...added].join('')
}

/**
 * A scratch-copy parent that mirrors the repository around rust/. cargo-mutants
 * copies only rust/, and crates include files above it (CHANGELOG.md and the
 * release key), so each copy's `..` must resolve to the repository's entries.
 * `.git` stays out, so Git in a copy finds the real repository above the
 * mirror, as it does from rust/.
 */
export function createRepositoryMirror(repoRoot: string, parent: string): string {
  const mirror = mkdtempSync(join(parent, 'scratch-'))
  for (const entry of readdirSync(repoRoot, { withFileTypes: true })) {
    if (entry.name === 'rust' || entry.name === '.git') continue
    symlinkSync(join(repoRoot, entry.name), join(mirror, entry.name), entry.isDirectory() ? 'dir' : 'file')
  }
  return mirror
}

/** The Cargo that cargo-mutants runs in copy mode; it gives builds and tests the caller's TMPDIR back. */
export const SCRATCH_CARGO_NAME = '.bake-cargo'
export const SCRATCH_CARGO = `#!/bin/sh
if [ -n "\${BAKE_MUTANTS_TMPDIR+set}" ]; then TMPDIR=$BAKE_MUTANTS_TMPDIR; export TMPDIR; else unset TMPDIR; fi
unset BAKE_MUTANTS_TMPDIR
exec cargo "$@"
`

/**
 * The environment of cargo-mutants itself. It makes its copies in TMPDIR,
 * which copy mode points at the mirror. Tests must not inherit that: some
 * assume Cargo's target directory lies outside their temporary directory,
 * and context-file tests must not find the repository's AGENTS.md above it.
 */
export function mutantsEnvironment(env: Readonly<Record<string, string | undefined>>, mirror: string | undefined): Record<string, string> {
  // cargo-mutants runs `$CARGO` when set; only copy mode sets it.
  const result: Record<string, string> = {}
  for (const [key, value] of Object.entries(env)) if (value !== undefined && key !== 'CARGO') result[key] = value
  if (mirror === undefined) return result
  const callerTmpdir = env['TMPDIR']
  delete result['BAKE_MUTANTS_TMPDIR']
  if (callerTmpdir !== undefined) result['BAKE_MUTANTS_TMPDIR'] = callerTmpdir
  return { ...result, TMPDIR: mirror, CARGO: join(mirror, SCRATCH_CARGO_NAME) }
}

async function main(): Promise<number> {
  if (process.argv.includes('--help') || process.argv.includes('-h')) {
    console.log(USAGE)
    return 0
  }
  const options = parseMutantsArgs(process.argv.slice(2), availableParallelism(), REPO_ROOT)
  if (process.platform === 'win32' && !options.inPlace && !options.list) {
    console.error('mutants:rust: copy mode restores TMPDIR with a POSIX shell; pass --in-place on Windows.')
    return 2
  }
  if (Bun.which('cargo-mutants') === null) {
    console.error('mutants:rust: cargo-mutants is not installed; run `cargo install --locked cargo-mutants@27.1.0`.')
    return 1
  }
  mkdirSync(OUTPUT_ROOT, { recursive: true })
  let diffPath: string | undefined
  let diffFiles: string[] | undefined
  if (usesDiff(options)) {
    const git = gitIn(RUST_ROOT)
    const { base, mergeBase } = resolveMergeBase(git, options.base)
    const diff = workspaceDiff(git, mergeBase)
    diffFiles = changedRustFiles(diff)
    console.log(`mutants:rust: ${String(diffFiles.length)} changed Rust files since ${base} (${mergeBase.slice(0, 12)}).`)
    if (diffFiles.length === 0) return 0
    diffPath = join(OUTPUT_ROOT, 'diff.patch')
    writeFileSync(diffPath, diff)
  }
  const runs = planMutantsRuns(options, diffPath, diffFiles, OUTPUT_ROOT)
  if (runs.length === 0) {
    console.log('mutants:rust: nothing in scope.')
    return 0
  }

  // cargo-mutants restores a mutated file when interrupted; wait for it instead of exiting first.
  const ignoreInterrupt = (): void => {}
  process.on('SIGINT', ignoreInterrupt)
  const mirror = options.inPlace || options.list ? undefined : createRepositoryMirror(REPO_ROOT, OUTPUT_ROOT)
  const codes: number[] = []
  try {
    if (mirror !== undefined) writeFileSync(join(mirror, SCRATCH_CARGO_NAME), SCRATCH_CARGO, { mode: 0o755 })
    const env = mutantsEnvironment(process.env, mirror)
    for (const run of runs) {
      if (!options.list) rmSync(join(OUTPUT_ROOT, run.name), { recursive: true, force: true })
      console.log(`mutants:rust: cargo ${run.args.join(' ')}`)
      // Run the binary directly: as `cargo mutants`, Cargo would replace CARGO with itself.
      const child = Bun.spawn(['cargo-mutants', ...run.args], { cwd: RUST_ROOT, env, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' })
      codes.push(await child.exited)
    }
  } finally {
    if (mirror !== undefined) rmSync(mirror, { recursive: true, force: true })
    process.off('SIGINT', ignoreInterrupt)
  }
  if (options.list) return mutantsExitCode(codes, EMPTY_SUMMARY, false)
  const summary = mergeSummaries(runs.map(run => readMutantsOutput(join(OUTPUT_ROOT, run.name, 'mutants.out'))))
  console.log(formatSummary(summary))
  console.log(`mutants:rust: logs and diffs under ${relative(REPO_ROOT, OUTPUT_ROOT)}/<run>/mutants.out`)
  return mutantsExitCode(codes, summary, options.strict)
}

if (import.meta.main) {
  try {
    process.exit(await main())
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(2)
  }
}
