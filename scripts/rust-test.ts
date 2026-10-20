#!/usr/bin/env bun
/** Run Rust tests from the pinned workspace, preserving failures and Cargo filters. */
import { resolve } from 'node:path'

/** Full Linux and Windows runs use nextest when available; Cargo keeps shared setup on macOS. */
export function rustTestCommands(args: readonly string[], nextest: boolean, ci: boolean, platform: string): readonly (readonly string[])[] {
  if (!nextest || platform === 'darwin' || args.length > 0) {
    return [['cargo', 'test', ...args.length === 0 ? ['--workspace', '--tests'] : [], '--locked', '--no-fail-fast', ...args]]
  }
  return [
    ['cargo', 'nextest', 'run', '--workspace', '--locked', '--no-fail-fast', '--retries', '0', '--flaky-result', 'fail',
      ...ci ? ['--profile', 'ci'] : [], '-E', 'not binary_id(bake-codemode::codemode)'],
    // These engine tests share a compiled Wasm engine in one process.
    // Keep workspace feature resolution too, so Cargo reuses nextest's artifacts.
    ['cargo', 'test', '--workspace', '--locked', '--test', 'codemode', '--no-fail-fast'],
  ]
}

/** Finish both disjoint suites and retain any failure, even if the last suite passes. */
export async function runRustTestCommands(
  commands: readonly (readonly string[])[], run: (command: readonly string[]) => Promise<number>,
): Promise<number> {
  let code = 0
  for (const command of commands) {
    const result = await run(command)
    if (result !== 0) code = result
  }
  return code
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const nextest = Bun.which('cargo-nextest') !== null
  if (args.length === 0) {
    if (process.platform === 'darwin') console.log('test:rust: using Cargo on macOS to share test-process setup.')
    else if (!nextest) console.log('test:rust: cargo-nextest is unavailable; using cargo test (see rust/README.md).')
  }
  const commands = rustTestCommands(args, nextest, process.env['CI'] === 'true', process.platform)
  const code = await runRustTestCommands(commands, async (command) => {
    const child = Bun.spawn([...command], {
      cwd: resolve(import.meta.dirname, '../rust'),
      stdin: 'inherit', stdout: 'inherit', stderr: 'inherit',
    })
    return child.exited
  })
  process.exit(code)
}
