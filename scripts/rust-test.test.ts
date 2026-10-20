import { describe, expect, it } from 'bun:test'
import { runRustTestCommands, rustTestCommands } from './rust-test.ts'

describe('Rust test runner', () => {
  it('keeps package, target, and test filters in both runners', () => {
    const filters = ['-p', 'bake-agent', '--lib', 'nested_tests']
    for (const nextest of [false, true]) {
      const commands = rustTestCommands(filters, nextest, false, 'linux')
      expect(commands).toHaveLength(1)
      const command = commands[0] ?? []
      expect(command.slice(0, 2)).toEqual(['cargo', 'test'])
      expect(command).not.toContain('--workspace')
      expect(command.slice(-filters.length)).toEqual(filters)
      expect(command).toContain('--locked')
      expect(command).toContain('--no-fail-fast')
    }
  })

  it('disables retries and fails flaky results with nextest', () => {
    const command = rustTestCommands([], true, false, 'linux')[0] ?? []
    expect(command.slice(0, 3)).toEqual(['cargo', 'nextest', 'run'])
    expect(command.slice(command.indexOf('--retries'), command.indexOf('--retries') + 2)).toEqual(['--retries', '0'])
    expect(command.slice(command.indexOf('--flaky-result'), command.indexOf('--flaky-result') + 2)).toEqual(['--flaky-result', 'fail'])
  })

  it('selects JUnit reporting in CI without passing nextest flags to Cargo', () => {
    expect(rustTestCommands([], true, true, 'win32')[0]).toContain('ci')
    expect(rustTestCommands([], true, false, 'linux')[0]).not.toContain('--profile')
    expect(rustTestCommands([], false, true, 'linux')).toEqual([['cargo', 'test', '--workspace', '--tests', '--locked', '--no-fail-fast']])
  })

  it('shares Cargo test processes on macOS even with nextest installed', () => {
    expect(rustTestCommands([], true, true, 'darwin')).toEqual([['cargo', 'test', '--workspace', '--tests', '--locked', '--no-fail-fast']])
  })

  it('runs the engine-sharing suite once, outside the process-per-test run', () => {
    const commands = rustTestCommands([], true, false, 'linux')
    expect(commands).toHaveLength(2)
    expect(commands[0]).toContain('not binary_id(bake-codemode::codemode)')
    expect(commands[1]).toEqual(['cargo', 'test', '--workspace', '--locked', '--test', 'codemode', '--no-fail-fast'])
  })

  it('retains an earlier failure and finishes the other suite without retrying', async () => {
    const commands = rustTestCommands([], true, false, 'linux')
    for (const failed of [0, 1]) {
      const ran: (readonly string[])[] = []
      const code = await runRustTestCommands(commands, async (command) => {
        ran.push(command)
        return ran.length - 1 === failed ? 17 : 0
      })
      expect(code).toBe(17)
      expect(ran).toEqual(commands)
    }
  })
})
