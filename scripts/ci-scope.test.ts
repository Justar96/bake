import { describe, expect, it } from 'bun:test'
import { onRustLine, plan, RUST_LINE, typeScriptNeed, type Event } from './ci-scope.ts'

const pull = (baseRef: string, headRef = 'feat/x', sameRepository = true): Event =>
  ({ name: 'pull_request', baseRef, headRef, sameRepository })

describe('typeScriptNeed', () => {
  it('asks nothing of Rust sources and their drivers', () => {
    expect(typeScriptNeed(['rust/crates/bake-tui/src/port.rs', 'rust/Cargo.lock', 'scripts/rust-preview-pty.ts',
      'scripts/rust-conformance/driver.ts'])).toBe('none')
  })

  it('asks the static half of Markdown, wherever it is', () => {
    expect(typeScriptNeed(['rust/README.md', 'CHANGELOG.md', 'docs/roadmap/rust-0.4/tui-design.md'])).toBe('static')
  })

  it('asks everything of any other file, and of an empty list', () => {
    expect(typeScriptNeed(['rust/src/a.rs', 'conformance/session/cases.json'])).toBe('full')
    expect(typeScriptNeed(['scripts/rust-migration-ledger.ts'])).toBe('full')
    expect(typeScriptNeed(['docs/roadmap/rust-0.4/scope-00/inventory.json'])).toBe('full')
    expect(typeScriptNeed([])).toBe('full')
  })
})

describe('plan', () => {
  it('runs every job on every platform outside the Rust line', () => {
    const chosen = plan(pull('develop'), false, undefined)
    expect(chosen.preflight.map(job => `${job.name}/${job.os}`)).toEqual([
      'checks/ubuntu-latest', 'runtime/ubuntu-latest', 'checks/macos-latest', 'runtime/macos-latest',
    ])
    expect(chosen.rust).toEqual(['ubuntu-latest', 'macos-latest', 'windows-latest'])
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

  it('runs the native job everywhere on the Rust line and TypeScript on Linux as needed', () => {
    const rust = plan(pull(RUST_LINE), true, ['rust/src/a.rs'])
    expect(rust.rust).toEqual(['ubuntu-latest', 'macos-latest', 'windows-latest'])
    expect(rust.preflight).toEqual([])
    expect(rust.windowsRelease).toBe(false)
    expect(plan(pull(RUST_LINE), true, ['rust/src/a.rs', 'CHANGELOG.md']).preflight)
      .toEqual([{ os: 'ubuntu-latest', name: 'checks', args: '--fast --skip native' }])
    expect(plan(pull(RUST_LINE), true, ['packages/core/x.ts']).preflight.map(job => `${job.name}/${job.os}`))
      .toEqual(['checks/ubuntu-latest', 'runtime/ubuntu-latest'])
    // Unknown files run both parts.
    expect(plan(pull(RUST_LINE), true, undefined).preflight).toHaveLength(2)
  })
})

describe('onRustLine', () => {
  const bases: Record<string, string> = {
    'feat/top': 'feat/middle', 'feat/middle': 'feat/bottom', 'feat/bottom': RUST_LINE,
    'feat/other': 'develop', 'loop/a': 'loop/b', 'loop/b': 'loop/a',
  }
  const baseOf = (head: string): string | undefined => bases[head]

  it('follows a stack down to the Rust line', () => {
    expect(onRustLine(RUST_LINE, baseOf)).toBe(true)
    expect(onRustLine('feat/top', baseOf)).toBe(true)
  })

  it('stops at another trunk, a broken chain, or a loop', () => {
    expect(onRustLine('develop', baseOf)).toBe(false)
    expect(onRustLine('feat/other', baseOf)).toBe(false)
    expect(onRustLine('feat/orphan', baseOf)).toBe(false)
    expect(onRustLine('loop/a', baseOf)).toBe(false)
  })
})
