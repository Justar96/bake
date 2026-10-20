import { describe, expect, it } from 'bun:test'
import { iterateScope, parseIterateOptions, runStages } from './rust-iterate.ts'
import type { RustPackage, RustScope } from './rust-scope.ts'

const workspace: RustPackage[] = [
  { name: 'bake-tui-view', dir: 'rust/crates/bake-tui-view', dependencies: [] },
  { name: 'bake-tui', dir: 'rust/crates/bake-tui', dependencies: ['bake-tui-view'] },
  { name: 'bake-ai', dir: 'rust/crates/bake-ai', dependencies: [] },
]

const unused = {
  worktree: () => { throw new Error('the worktree is not read') },
  since: () => { throw new Error('no base is read') },
}
const packagesOf = (scope: RustScope) => scope.kind === 'packages' ? scope.packages : scope.kind

describe('Rust iterate loop', () => {
  it('reads one scope choice and whether to lint', () => {
    expect(parseIterateOptions([])).toEqual({ packages: [], base: undefined, all: false, clippy: true })
    expect(parseIterateOptions(['-p', 'bake-ai', '--package', 'bake-tui', '--no-clippy']))
      .toEqual({ packages: ['bake-ai', 'bake-tui'], base: undefined, all: false, clippy: false })
    expect(parseIterateOptions(['--base', 'origin/develop'])).toEqual({ packages: [], base: 'origin/develop', all: false, clippy: true })
    expect(parseIterateOptions(['--help'])).toBe('help')
    expect(() => parseIterateOptions(['--all', '-p', 'bake-ai'])).toThrow(/choose one/u)
    expect(() => parseIterateOptions(['--base', 'x', '-p', 'bake-ai'])).toThrow(/choose one/u)
    expect(() => parseIterateOptions(['-p'])).toThrow(/needs a value/u)
    expect(() => parseIterateOptions(['--bogus'])).toThrow(/unknown argument/u)
  })

  it('tests named packages with their reverse dependents and rejects unknown names', () => {
    const named = iterateScope({ packages: ['bake-tui-view'], base: undefined, all: false, clippy: true }, workspace, unused)
    expect(packagesOf(named.scope)).toEqual(['bake-tui', 'bake-tui-view'])
    expect(named.scope.kind === 'packages' ? [...named.scope.changed] : []).toEqual([['bake-tui-view', 'named with -p']])
    expect(() => iterateScope({ packages: ['bake-nope'], base: undefined, all: false, clippy: true }, workspace, unused))
      .toThrow(/no workspace package bake-nope/u)
    expect(iterateScope({ packages: [], base: undefined, all: true, clippy: true }, workspace, unused).scope).toEqual({ kind: 'full', reason: '--all' })
  })

  it('reads the worktree by default and the merge base with --base', () => {
    const changes = {
      worktree: () => ({ from: 'uncommitted changes', files: ['rust/crates/bake-ai/src/lib.rs'] }),
      since: (base: string) => base === 'origin/develop' ? { mergeBase: '0123456789abcdef', files: ['docs/x.md'] } : undefined,
    }
    const local = iterateScope({ packages: [], base: undefined, all: false, clippy: true }, workspace, changes)
    expect(local.from).toBe('uncommitted changes')
    expect(packagesOf(local.scope)).toEqual(['bake-ai'])
    const based = iterateScope({ packages: [], base: 'origin/develop', all: false, clippy: true }, workspace, changes)
    expect(based.from).toBe('the merge base with origin/develop (0123456789)')
    expect(based.scope).toEqual({ kind: 'none' })
    expect(() => iterateScope({ packages: [], base: 'nowhere', all: false, clippy: true }, workspace, changes)).toThrow(/no merge base/u)
  })

  it('tests everything when git cannot list the change', () => {
    const unreadable = { worktree: () => undefined, since: unused.since }
    expect(iterateScope({ packages: [], base: undefined, all: false, clippy: true }, workspace, unreadable).scope)
      .toEqual({ kind: 'full', reason: 'git cannot list the change' })
  })

  it('stops at the first failing stage and times every stage it ran', async () => {
    let clock = 0
    const ran: string[] = []
    const stage = (name: string, code: number, seconds: number) => ({
      name, run: async () => { ran.push(name); clock += seconds * 1000; return code },
    })
    const failed = await runStages([stage('fmt', 0, 1), stage('clippy', 101, 2), stage('test', 0, 3)], () => clock)
    expect(failed).toEqual({ code: 101, times: [['fmt', 1], ['clippy', 2]] })
    expect(ran).toEqual(['fmt', 'clippy'])
    clock = 0
    expect(await runStages([stage('fmt', 0, 1), stage('test', 0, 3)], () => clock)).toEqual({ code: 0, times: [['fmt', 1], ['test', 3]] })
  })
})
