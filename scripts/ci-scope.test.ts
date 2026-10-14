import { describe, expect, it } from 'bun:test'
import { onScopedTrunk, plan, SCOPED_TRUNK, typeScriptNeed, type Event } from './ci-scope.ts'

const pull = (baseRef: string, headRef = 'feat/x', sameRepository = true): Event =>
  ({ name: 'pull_request', baseRef, headRef, sameRepository })

describe('typeScriptNeed', () => {
  it('asks nothing of Rust sources and their drivers', () => {
    expect(typeScriptNeed(['rust/crates/bake-tui/src/port.rs', 'rust/Cargo.lock', 'scripts/rust-preview-pty.ts'])).toBe('none')
  })

  it('asks the static half of Markdown, wherever it is', () => {
    expect(typeScriptNeed(['rust/README.md', 'CHANGELOG.md', 'docs/roadmap/rust-0.4/tui-design.md'])).toBe('static')
  })

  it('asks everything of any other file, and of an empty list', () => {
    expect(typeScriptNeed(['rust/src/a.rs', 'evals/agent-loop/launch.ts'])).toBe('full')
    expect(typeScriptNeed(['scripts/rust-migration-ledger.ts'])).toBe('full')
    expect(typeScriptNeed(['docs/roadmap/rust-0.4/scope-00/inventory.json'])).toBe('full')
    expect(typeScriptNeed([])).toBe('full')
  })
})

describe('plan', () => {
  it('runs every job off develop, on Linux and Windows but never macOS', () => {
    const chosen = plan(pull('release/0.3'), false, undefined)
    expect(chosen.preflight.map(job => `${job.name}/${job.os}`)).toEqual([
      'checks/ubuntu-latest', 'runtime/ubuntu-latest',
    ])
    expect(chosen.rust).toEqual(['ubuntu-latest', 'windows-latest'])
    expect(chosen.windowsRelease).toBe(true)
    expect(plan({ name: 'push', sameRepository: true }, false, undefined)).toEqual(chosen)
  })

  it('runs Linux alone for a release pull request from develop', () => {
    const chosen = plan(pull('main', 'develop'), false, undefined)
    expect(chosen.preflight.map(job => job.os)).toEqual(['ubuntu-latest', 'ubuntu-latest'])
    expect(chosen.rust).toEqual(['ubuntu-latest'])
    expect(chosen.windowsRelease).toBe(false)
    // From a fork, the branch name proves nothing.
    expect(plan(pull('main', 'develop', false), false, undefined).windowsRelease).toBe(true)
  })

  it('runs the native job on Linux and Windows into develop and TypeScript on Linux as needed', () => {
    const rust = plan(pull(SCOPED_TRUNK), true, ['rust/src/a.rs'])
    expect(rust.rust).toEqual(['ubuntu-latest', 'windows-latest'])
    expect(rust.preflight).toEqual([])
    expect(rust.windowsRelease).toBe(false)
    const markdown = plan(pull(SCOPED_TRUNK), true, ['rust/src/a.rs', 'CHANGELOG.md'])
    expect(markdown.preflight).toEqual([{ os: 'ubuntu-latest', name: 'checks', args: '--fast --skip native' }])
    expect(markdown.windowsRelease).toBe(false)
    const typescript = plan(pull(SCOPED_TRUNK), true, ['packages/core/x.ts'])
    expect(typescript.preflight.map(job => `${job.name}/${job.os}`))
      .toEqual(['checks/ubuntu-latest', 'runtime/ubuntu-latest'])
    expect(typescript.windowsRelease).toBe(true)
    // Unknown files run both parts.
    expect(plan(pull(SCOPED_TRUNK), true, undefined).preflight).toHaveLength(2)
  })
})

describe('onScopedTrunk', () => {
  const bases: Record<string, string> = {
    'feat/top': 'feat/middle', 'feat/middle': 'feat/bottom', 'feat/bottom': SCOPED_TRUNK,
    'fix/other': 'release/0.3', 'loop/a': 'loop/b', 'loop/b': 'loop/a',
  }
  const baseOf = (head: string): string | undefined => bases[head]

  it('follows a stack down to develop', () => {
    expect(onScopedTrunk(SCOPED_TRUNK, baseOf)).toBe(true)
    expect(onScopedTrunk('feat/top', baseOf)).toBe(true)
  })

  it('stops at another trunk, a broken chain, or a loop', () => {
    expect(onScopedTrunk('main', baseOf)).toBe(false)
    expect(onScopedTrunk('release/0.3', baseOf)).toBe(false)
    expect(onScopedTrunk('fix/other', baseOf)).toBe(false)
    expect(onScopedTrunk('feat/orphan', baseOf)).toBe(false)
    expect(onScopedTrunk('loop/a', baseOf)).toBe(false)
  })
})
