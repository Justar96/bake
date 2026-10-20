import { describe, expect, it } from 'bun:test'
import { parseChangedArgs, type RustTestPlan, runRustTestCommands, rustTestCommands } from './rust-test.ts'
import type { RustScope } from './rust-scope.ts'

const plan = (overrides: Partial<RustTestPlan>): RustTestPlan => ({
  args: [], nextest: true, ci: false, platform: 'linux', cores: 32, ...overrides,
})
const scoped = (changed: readonly string[], packages: readonly string[]): Extract<RustScope, { kind: 'packages' }> => ({
  kind: 'packages', changed: new Map(changed.map(name => [name, `rust/crates/${name}/src/lib.rs`])), packages,
})
const CODEMODE_SUITE = ['cargo', 'test', '--workspace', '--locked', '--test', 'codemode', '--no-fail-fast', '--', '--test-threads=8']

describe('Rust test runner', () => {
  it('keeps package, target, and test filters in both runners', () => {
    const filters = ['-p', 'bake-agent', '--lib', 'nested_tests']
    for (const nextest of [false, true]) {
      const commands = rustTestCommands(plan({ args: filters, nextest }))
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
    for (const scope of [undefined, scoped(['bake-ai'], ['bake-ai', 'bake-agent'])]) {
      const command = rustTestCommands(plan({ scope }))[0] ?? []
      expect(command.slice(0, 3)).toEqual(['cargo', 'nextest', 'run'])
      expect(command.slice(command.indexOf('--retries'), command.indexOf('--retries') + 2)).toEqual(['--retries', '0'])
      expect(command.slice(command.indexOf('--flaky-result'), command.indexOf('--flaky-result') + 2)).toEqual(['--flaky-result', 'fail'])
    }
  })

  it('selects JUnit reporting in CI without passing nextest flags to Cargo', () => {
    expect(rustTestCommands(plan({ ci: true, platform: 'win32' }))[0]).toContain('ci')
    expect(rustTestCommands(plan({}))[0]).not.toContain('--profile')
    expect(rustTestCommands(plan({ nextest: false, ci: true }))).toEqual([['cargo', 'test', '--workspace', '--tests', '--locked', '--no-fail-fast']])
  })

  it('shares Cargo test processes on macOS even with nextest installed', () => {
    expect(rustTestCommands(plan({ ci: true, platform: 'darwin' }))).toEqual([['cargo', 'test', '--workspace', '--tests', '--locked', '--no-fail-fast']])
  })

  it('runs every test with plain Cargo when it cannot filter a scoped workspace run', () => {
    const scope = scoped(['bake-tui-view'], ['bake-cli', 'bake-tui', 'bake-tui-view'])
    for (const overrides of [{ platform: 'darwin' }, { nextest: false }]) {
      expect(rustTestCommands(plan({ ...overrides, scope }))).toEqual([['cargo', 'test', '--workspace', '--tests', '--locked', '--no-fail-fast']])
    }
  })

  it('runs the engine-sharing suite once, outside the process-per-test run', () => {
    const commands = rustTestCommands(plan({}))
    expect(commands).toHaveLength(2)
    expect(commands[0]).toContain('not binary_id(bake-codemode::codemode)')
    expect(commands[1]).toEqual(CODEMODE_SUITE)
    expect(rustTestCommands(plan({ cores: 4 }))[1]).toContain('--test-threads=2')
  })

  it('filters a scoped run by package over the whole workspace, with the engine suite only when it is reached', () => {
    const [view] = rustTestCommands(plan({ scope: scoped(['bake-tui-view'], ['bake-cli', 'bake-tui', 'bake-tui-view']) }))
      .map(command => [...command])
    expect(view).toContain('--workspace')
    expect(view).not.toContain('-p')
    expect(view?.slice(view.indexOf('-E'))).toEqual(['-E', '(rdeps(=bake-tui-view)) & not binary_id(bake-codemode::codemode)'])
    expect(rustTestCommands(plan({ scope: scoped(['bake-tui-view'], ['bake-cli', 'bake-tui', 'bake-tui-view']) }))).toHaveLength(1)
    const engine = rustTestCommands(plan({ scope: scoped(['bake-codemode', 'bake-ai'], ['bake-ai', 'bake-codemode', 'bake-tui']) }))
    expect(engine[0]?.slice(-1)).toEqual(['(rdeps(=bake-ai) | rdeps(=bake-codemode)) & not binary_id(bake-codemode::codemode)'])
    expect(engine[1]).toEqual(CODEMODE_SUITE)
  })

  it('retains an earlier failure and finishes the other suite without retrying', async () => {
    const commands = rustTestCommands(plan({}))
    for (const failed of [0, 1]) {
      const ran: (readonly string[])[] = []
      const code = await runRustTestCommands(commands, (command) => {
        ran.push(command)
        return { exited: Promise.resolve(ran.length - 1 === failed ? 17 : 0), release: () => {} }
      })
      expect(code).toBe(17)
      expect(ran).toEqual([...commands])
    }
  })

  it('starts the suites together and writes held output only after the earlier suites finish', async () => {
    const events: string[] = []
    let finishFirst = (_code: number): void => {}
    const first = new Promise<number>((done) => { finishFirst = done })
    const running = runRustTestCommands([['first'], ['second']], (command, held) => {
      events.push(`start ${command[0]} held=${held}`)
      const exited = command[0] === 'first' ? first : Promise.resolve(3)
      return { exited, release: () => { events.push(`release ${command[0]}`) } }
    })
    await Promise.resolve()
    expect(events).toEqual(['start first held=false', 'start second held=true'])
    finishFirst(0)
    expect(await running).toBe(3)
    expect(events.slice(2)).toEqual(['release first', 'release second'])
  })

  it('reads a scoped run only from a leading --changed', () => {
    expect(parseChangedArgs([])).toBeUndefined()
    expect(parseChangedArgs(['-p', 'bake-ai'])).toBeUndefined()
    expect(parseChangedArgs(['--changed'])).toEqual({ base: undefined })
    expect(parseChangedArgs(['--changed', 'HEAD~3'])).toEqual({ base: 'HEAD~3' })
    expect(() => parseChangedArgs(['--changed', 'a', 'b'])).toThrow(/usage/u)
    expect(() => parseChangedArgs(['--changed', '--lib'])).toThrow(/usage/u)
    expect(() => parseChangedArgs(['-p', 'x', '--changed'])).toThrow(/must come first/u)
  })
})
