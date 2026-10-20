#!/usr/bin/env bun
/**
 * The local Rust edit loop: format, lint, then test what the change reaches,
 * stopping at the first failure and timing each stage.
 *
 *   bun run iterate:rust [-p <crate> ...] [--base <ref>] [--all] [--no-clippy]
 *
 * By default the change is the uncommitted and untracked paths against HEAD, or
 * HEAD's own commit when the tree is clean. `--base <ref>` measures from the merge
 * base with `<ref>` instead; `-p` names the packages directly; `--all` tests
 * everything. Each selection adds the selected packages' reverse dependents.
 */
import { availableParallelism } from 'node:os'
import { resolve } from 'node:path'
import {
  changesSinceMergeBase, describeRustScope, loadRustWorkspace, type RustPackage, type RustScope, rustScope, withReverseDependents,
  worktreeChanges,
} from './rust-scope.ts'
import { runRustTestCommands, rustTestCommands, startRustTestProcess } from './rust-test.ts'

/** Options read from the command line. */
export interface IterateOptions {
  readonly packages: readonly string[]
  readonly base: string | undefined
  readonly all: boolean
  readonly clippy: boolean
}

const USAGE = 'usage: bun run iterate:rust [-p <crate> ...] [--base <ref>] [--all] [--no-clippy]'

/** Parse the command line; `-p`, `--base`, and `--all` each choose the scope, so at most one may be given. */
export function parseIterateOptions(argv: readonly string[]): IterateOptions | 'help' {
  const packages: string[] = []
  let base: string | undefined
  let all = false, clippy = true
  const rest = [...argv]
  for (let arg = rest.shift(); arg !== undefined; arg = rest.shift()) {
    const value = (): string => {
      const next = rest.shift()
      if (next === undefined || next.startsWith('-')) throw new Error(`iterate:rust: ${arg} needs a value\n${USAGE}`)
      return next
    }
    if (arg === '-p' || arg === '--package') packages.push(value())
    else if (arg === '--base') base = value()
    else if (arg === '--all') all = true
    else if (arg === '--no-clippy') clippy = false
    else if (arg === '--help' || arg === '-h') return 'help'
    else throw new Error(`iterate:rust: unknown argument ${arg}\n${USAGE}`)
  }
  if ([packages.length > 0, base !== undefined, all].filter(Boolean).length > 1) {
    throw new Error(`iterate:rust: choose one of -p, --base, and --all\n${USAGE}`)
  }
  return { packages, base, all, clippy }
}

/** Where a run reads its changed paths. */
export interface IterateChanges {
  readonly worktree: () => { from: string; files: readonly string[] }
  readonly since: (base: string) => { mergeBase: string; files: readonly string[] } | undefined
}

/** The tests a run needs, with the line saying where its change came from. */
export function iterateScope(
  options: IterateOptions, workspace: readonly RustPackage[], changes: IterateChanges,
): { readonly from: string; readonly scope: RustScope } {
  if (options.all) return { from: '--all', scope: { kind: 'full', reason: '--all' } }
  if (options.packages.length > 0) {
    const names = new Set(workspace.map(member => member.name))
    const unknown = options.packages.filter(name => !names.has(name))
    if (unknown.length > 0) throw new Error(`iterate:rust: no workspace package ${unknown.join(', ')}; packages: ${[...names].sort().join(', ')}`)
    const scope = withReverseDependents(new Map(options.packages.map(name => [name, 'named with -p'])), workspace)
    return { from: `-p ${options.packages.join(' -p ')}`, scope }
  }
  if (options.base !== undefined) {
    const since = changes.since(options.base)
    if (since === undefined) throw new Error(`iterate:rust: ${options.base} has no merge base with HEAD`)
    return { from: `the merge base with ${options.base} (${since.mergeBase.slice(0, 10)})`, scope: rustScope(since.files, workspace) }
  }
  const worktree = changes.worktree()
  return { from: worktree.from, scope: rustScope(worktree.files, workspace) }
}

/** One stage: a name and a function resolving with its exit code. */
export interface Stage {
  readonly name: string
  readonly run: () => Promise<number>
}

/** Run stages in order, stopping at the first failure; report each stage's seconds. */
export async function runStages(
  stages: readonly Stage[], now: () => number = () => performance.now(),
): Promise<{ code: number; times: [string, number][] }> {
  const times: [string, number][] = []
  for (const stage of stages) {
    const started = now()
    const code = await stage.run()
    times.push([stage.name, (now() - started) / 1000])
    if (code !== 0) return { code, times }
  }
  return { code: 0, times }
}

const RUST_DIR = resolve(import.meta.dirname, '../rust')

async function cargo(args: readonly string[]): Promise<number> {
  console.log(`\n$ cargo ${args.join(' ')}`)
  return Bun.spawn(['cargo', ...args], { cwd: RUST_DIR, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' }).exited
}

async function main(argv: readonly string[]): Promise<number> {
  let options: IterateOptions | 'help'
  try {
    options = parseIterateOptions(argv)
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    return 2
  }
  if (options === 'help') { console.log(USAGE); return 0 }
  const selected = options
  const stages: Stage[] = [{ name: 'fmt', run: () => cargo(['fmt', '--all']) }]
  if (selected.clippy) stages.push({ name: 'clippy', run: () => cargo(['clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings']) })
  stages.push({
    name: 'test',
    run: async () => {
      let resolved: { from: string; scope: RustScope }
      try {
        const changes: IterateChanges = { worktree: () => worktreeChanges(), since: base => changesSinceMergeBase(base) }
        resolved = iterateScope(selected, loadRustWorkspace(), changes)
      } catch (error) {
        console.error(error instanceof Error ? error.message : String(error))
        return 2
      }
      console.log(`\niterate:rust: testing ${resolved.from}`)
      for (const line of describeRustScope(resolved.scope)) console.log(`iterate:rust: ${line}`)
      if (resolved.scope.kind === 'none') {
        console.log('iterate:rust: no Rust input changed; skipping tests (--all runs them).')
        return 0
      }
      const nextest = Bun.which('cargo-nextest') !== null
      const commands = rustTestCommands({
        args: [], nextest, ci: false, platform: process.platform, cores: availableParallelism(),
        scope: resolved.scope.kind === 'packages' ? resolved.scope : undefined,
      })
      for (const command of commands) console.log(`$ ${command.join(' ')}`)
      return runRustTestCommands(commands, startRustTestProcess)
    },
  })
  const { code, times } = await runStages(stages)
  const total = times.reduce((sum, [, seconds]) => sum + seconds, 0)
  console.log(`\niterate:rust: ${times.map(([name, seconds]) => `${name} ${seconds.toFixed(1)}s`).join(', ')}; ${total.toFixed(1)}s total`
    + (code === 0 ? '' : ` (${times.at(-1)?.[0] ?? 'a stage'} failed)`))
  return code
}

if (import.meta.main) process.exit(await main(process.argv.slice(2)))
