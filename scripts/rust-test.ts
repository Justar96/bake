#!/usr/bin/env bun
/**
 * Run Rust tests from the pinned workspace, preserving failures and Cargo filters.
 *
 * `bun run test:rust` runs every test. `bun run test:rust --changed [<base>]`
 * runs the tests of the packages changed since the merge base with `<base>`
 * (default origin/develop, else develop) and their reverse dependents; see
 * rust-scope.ts. Any other arguments pass to `cargo test`.
 */
import { closeSync, mkdtempSync, openSync, readFileSync, rmSync } from 'node:fs'
import { availableParallelism, tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import {
  changesSinceMergeBase, DEFAULT_BASES, describeRustScope, loadRustWorkspace, type RustScope, rustScope, rustScopeFilter,
} from './rust-scope.ts'

const CODEMODE = 'binary_id(bake-codemode::codemode)'

/** What one `test:rust` run executes. */
export interface RustTestPlan {
  /** Cargo arguments after `test`; when given, the run is one plain `cargo test`. */
  readonly args: readonly string[]
  readonly nextest: boolean
  /** CI selects nextest's JUnit profile and runs the suites in sequence. */
  readonly ci: boolean
  readonly platform: string
  /** Logical cores, which bound the engine suite's threads beside nextest. */
  readonly cores: number
  /** The packages a change reaches; absent for every test. */
  readonly scope?: Extract<RustScope, { kind: 'packages' }> | undefined
}

/**
 * Full Linux and Windows runs use nextest when available; Cargo keeps shared setup on macOS.
 *
 * The codemode suite shares one compiled Wasm engine per process, so it runs as
 * one `cargo test` process instead of nextest's process per test, started beside
 * nextest. Its build is already part of nextest's workspace build; while that build
 * holds Cargo's lock it waits. Warm runs on a 32-core Linux host (three each):
 * nextest then the codemode suite in sequence took 16.1-20.6 s; codemode under
 * nextest in a test group took 17.1-19.9 s at 8 threads and 16.3-37.7 s at 16-32,
 * paying an engine compile per test; the suite beside nextest with a quarter of
 * the cores took 12.0-12.3 s, and with every core 13.5-14.0 s. CI runners have
 * a few cores and no such measurement, so CI keeps the suites in sequence, the
 * codemode suite on every core; see `runRustTestCommands`.
 *
 * A scoped run keeps `--workspace`, so features and artifacts match the full run,
 * and filters which tests run. Plain Cargo has no such filter: `-p` selection
 * resolves features for the selected packages only and rebuilds shared
 * dependencies, so macOS and runs without nextest test everything instead.
 */
export function rustTestCommands(plan: RustTestPlan): readonly (readonly string[])[] {
  const { args, nextest, ci, platform, cores, scope } = plan
  if (!nextest || platform === 'darwin' || args.length > 0) {
    return [['cargo', 'test', ...args.length === 0 ? ['--workspace', '--tests'] : [], '--locked', '--no-fail-fast', ...args]]
  }
  const filter = scope === undefined ? `not ${CODEMODE}` : `(${rustScopeFilter(scope.changed.keys())}) & not ${CODEMODE}`
  const nextestRun = ['cargo', 'nextest', 'run', '--workspace', '--locked', '--no-fail-fast', '--retries', '0', '--flaky-result', 'fail',
    ...ci ? ['--profile', 'ci'] : [],
    // A package can have no tests on this platform; the filter still chose it correctly.
    ...scope === undefined ? [] : ['--no-tests', 'warn'],
    '-E', filter]
  if (scope !== undefined && !scope.packages.includes('bake-codemode')) return [nextestRun]
  // Workspace feature resolution again, so Cargo reuses nextest's artifacts.
  const codemode = ['cargo', 'test', '--workspace', '--locked', '--test', 'codemode', '--no-fail-fast']
  if (ci) return [nextestRun, codemode]
  return [nextestRun, [...codemode, '--', `--test-threads=${Math.max(2, Math.floor(cores / 4))}`]]
}

/** A started command: its exit code, and a way to write output it held back. */
export interface RustTestProcess {
  readonly exited: Promise<number>
  readonly release: () => void
}

/**
 * Finish every command, retaining the first failure; nothing is retried.
 * Concurrent commands start at once, and each after the first holds its output,
 * which is written once every earlier command has finished, so suites never
 * interleave in a gate log. Otherwise each starts when the one before exits.
 */
export async function runRustTestCommands(
  commands: readonly (readonly string[])[], start: (command: readonly string[], held: boolean) => RustTestProcess,
  concurrent: boolean,
): Promise<number> {
  const started = concurrent ? commands.map((command, index) => start(command, index > 0)) : undefined
  let code = 0
  for (const [index, command] of commands.entries()) {
    const child = started?.[index] ?? start(command, false)
    const result = await child.exited
    child.release()
    if (result !== 0 && code === 0) code = result
  }
  return code
}

const RUST_DIR = resolve(import.meta.dirname, '../rust')

/** Spawn one test command in `rust/`, its output either live or held in a private temporary file. */
export function startRustTestProcess(command: readonly string[], held: boolean): RustTestProcess {
  if (!held) {
    try {
      const child = Bun.spawn([...command], { cwd: RUST_DIR, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' })
      return { exited: child.exited, release: () => {} }
    } catch (error) {
      console.error(`test:rust: cannot start ${command.join(' ')}: ${String(error)}`)
      return { exited: Promise.resolve(127), release: () => {} }
    }
  }
  const directory = mkdtempSync(join(tmpdir(), 'bake-rust-test-'))
  const log = join(directory, 'output.log')
  // One descriptor for both streams keeps them in the order the command wrote them.
  const file = openSync(log, 'w')
  let exited: Promise<number>
  try {
    exited = Bun.spawn([...command], { cwd: RUST_DIR, stdin: 'ignore', stdout: file, stderr: file }).exited
  } catch (error) {
    exited = Promise.resolve(127)
    console.error(`test:rust: cannot start ${command.join(' ')}: ${String(error)}`)
  } finally {
    closeSync(file)
  }
  return {
    exited,
    release: () => {
      try {
        process.stdout.write(`\n$ ${command.join(' ')}\n${readFileSync(log, 'utf8')}`)
      } finally {
        rmSync(directory, { recursive: true, force: true })
      }
    },
  }
}

/** `--changed [<base>]`, or undefined when the arguments are Cargo's. */
export function parseChangedArgs(args: readonly string[]): { readonly base: string | undefined } | undefined {
  if (args[0] !== '--changed') {
    if (args.includes('--changed')) throw new Error('test:rust: --changed must come first and takes only an optional base')
    return undefined
  }
  if (args.length > 2 || (args[1]?.startsWith('-') ?? false)) throw new Error('test:rust: usage: bun run test:rust --changed [<base>]')
  return { base: args[1] }
}

/** Resolve a scoped run: the scope, or undefined when no base has a merge base with HEAD. */
export function changedScope(base: string | undefined): { from: string; files: readonly string[]; scope: RustScope } | undefined {
  for (const candidate of base === undefined ? DEFAULT_BASES : [base]) {
    const changes = changesSinceMergeBase(candidate)
    if (changes === undefined) continue
    const from = `${candidate} (${changes.mergeBase.slice(0, 10)})`
    return { from, files: changes.files, scope: rustScope(changes.files, loadRustWorkspace()) }
  }
  return undefined
}

async function main(args: readonly string[]): Promise<number> {
  let changed: { readonly base: string | undefined } | undefined
  try {
    changed = parseChangedArgs(args)
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    return 2
  }
  const cargoArgs = changed === undefined ? args : []
  let scope: Extract<RustScope, { kind: 'packages' }> | undefined
  if (changed !== undefined) {
    const resolved = changedScope(changed.base)
    if (resolved === undefined) {
      console.log(`test:rust: no merge base with ${changed.base ?? DEFAULT_BASES.join(' or ')}; running every test.`)
    } else {
      console.log(`test:rust: ${resolved.files.length} path(s) changed since ${resolved.from}`)
      for (const line of describeRustScope(resolved.scope)) console.log(`test:rust: ${line}`)
      if (resolved.scope.kind === 'none') {
        console.log('test:rust: skipping the Rust tests (bun run test:rust runs them all).')
        return 0
      }
      if (resolved.scope.kind === 'packages') scope = resolved.scope
    }
  }
  const nextest = Bun.which('cargo-nextest') !== null
  if (cargoArgs.length === 0) {
    if (process.platform === 'darwin') console.log('test:rust: using Cargo on macOS to share test-process setup.')
    else if (!nextest) console.log('test:rust: cargo-nextest is unavailable; using cargo test (see rust/README.md).')
    if (scope !== undefined && (process.platform === 'darwin' || !nextest)) console.log('test:rust: plain Cargo cannot filter a workspace run; running every test.')
  }
  const ci = process.env['CI'] === 'true'
  const commands = rustTestCommands({ args: cargoArgs, nextest, ci, platform: process.platform, cores: availableParallelism(), scope })
  return runRustTestCommands(commands, startRustTestProcess, !ci)
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
