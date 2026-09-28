import { describe, expect, it } from 'vitest'
import { changelogGap, parseOptions, runtimeArgs, selectSteps, STEPS, strayBuildOutput, type Scope } from './preflight.ts'

const scope = (files: readonly string[], mergeBase: string | undefined = 'abc123'): Scope => ({ base: 'origin/develop', mergeBase, files })
const selected = (argv: readonly string[]): string[] => selectSteps(parseOptions(argv))
  .filter(entry => entry.skip === undefined).map(entry => entry.step.name)

describe('preflight', () => {
  it('names every step once and runs each one by default', () => {
    const names = STEPS.map(step => step.name)
    expect(new Set(names).size).toBe(names.length)
    expect(selected([])).toEqual(names)
    for (const gate of ['typecheck', 'lint', 'build', 'tui-spec', 'runtime', 'e2e', 'verify-type-equiv']) expect(names).toContain(gate)
  })

  it('selects by step or group, and --fast leaves out everything that reads the build', () => {
    expect(selected(['--only', 'generated']).every(name => name.startsWith('verify-'))).toBe(true)
    expect(selected(['--only', 'lint,typecheck'])).toEqual(['typecheck', 'lint'])
    expect(selected(['--skip', 'e2e'])).not.toContain('e2e')
    const fast = selected(['--fast'])
    for (const name of ['build', 'tui-spec', 'runtime', 'e2e']) expect(fast).not.toContain(name)
    expect(fast).toContain('typecheck')
    expect(fast).toContain('tui-layout')
  })

  it('rejects what it does not know', () => {
    expect(() => parseOptions(['--only', 'nope'])).toThrow(/unknown step or group nope/u)
    expect(() => parseOptions(['--bogus'])).toThrow(/unknown argument/u)
    expect(() => parseOptions(['--base'])).toThrow(/needs a value/u)
  })

  it('runs the runtime tests the change reaches, all of them when the change can move any, or none', () => {
    const options = parseOptions([])
    expect(runtimeArgs(options, scope(['packages/core/agent/src/index.ts']))).toContain('--changed')
    expect(runtimeArgs(options, scope(['packages/core/agent/src/index.ts']))).toContain('abc123')
    for (const wide of ['package.json', 'bun.lock', 'vitest.config.ts', 'tsconfig.base.json']) {
      expect(runtimeArgs(options, scope([wide]))).not.toContain('--changed')
    }
    expect(runtimeArgs(options, scope(['docs/development.md', 'apps/tui/packages/ui/src/app.tsx']))).toHaveProperty('skip')
    expect(runtimeArgs(options, scope([], undefined))).not.toContain('--changed')
    expect(runtimeArgs(parseOptions(['--full']), scope(['docs/x.md']))).not.toHaveProperty('skip')
  })

  it('finds compiled output that shadows a source file, and nothing else', () => {
    expect(strayBuildOutput([
      'packages/goal/tool-goal/src/index.js',
      'packages/goal/tool-goal/src/index.d.ts',
      'apps/tui/packages/ui/src/app.js',
      'packages/goal/tool-goal/lib/index.js',
      'packages/web/x/src/css-modules.d.ts',
      'node_modules/',
      'packages/goal/tool-goal/node_modules/',
      'packages/goal/tool-goal/src/index.ts',
    ])).toEqual(['packages/goal/tool-goal/src/index.js', 'packages/goal/tool-goal/src/index.d.ts', 'apps/tui/packages/ui/src/app.js'])
  })

  it('asks for a CHANGELOG entry only for shipped source', () => {
    expect(changelogGap(['packages/goal/tool-goal/src/index.ts'])).toEqual(['packages/goal/tool-goal/src/index.ts'])
    expect(changelogGap(['apps/tui/packages/ui/src/app.tsx'])).toEqual(['apps/tui/packages/ui/src/app.tsx'])
    expect(changelogGap(['packages/goal/tool-goal/src/index.ts', 'CHANGELOG.md'])).toEqual([])
    expect(changelogGap(['packages/goal/tool-goal/tests/tool-goal.spec.ts', 'docs/development.md', 'scripts/preflight.ts'])).toEqual([])
  })
})
