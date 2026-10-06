import { readdirSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import {
  bunTests, changelogGap, failedBuild, parseOptions, runtimeArgs, selectSteps, STEPS, strayBuildOutput, vitestFailures, type Scope,
} from './preflight.ts'

const scope = (files: readonly string[]): Scope => ({ base: 'origin/develop', mergeBase: 'abc123', files })
const unresolved: Scope = { base: 'origin/develop', mergeBase: undefined, files: [] }
const selected = (argv: readonly string[]): string[] => selectSteps(parseOptions(argv))
  .filter(entry => entry.skip === undefined).map(entry => entry.step.name)

describe('preflight', () => {
  it('names every step once and runs each one by default', () => {
    const names = STEPS.map(step => step.name)
    expect(new Set(names).size).toBe(names.length)
    expect(selected([])).toEqual(names)
    for (const gate of ['rescope-vendor', 'typecheck', 'lint', 'actionlint', 'build', 'tui-spec', 'runtime', 'integration', 'e2e', 'verify-type-equiv',
      'verify-cordis-config', 'verify-package-invariants', 'verify-rust-migration-inventory']) expect(names).toContain(gate)
  })

  it('checks vendored names in the static phase', () => {
    const step = STEPS.find(entry => entry.name === 'rescope-vendor')
    expect(step?.phase).toBe('static')
    expect(step?.command?.(parseOptions([]), scope([]))).toEqual(['bun', 'scripts/rescope-vendor.ts', '--check'])
  })

  it('runs actionlint when it is installed and skips it otherwise', () => {
    const command = STEPS.find(step => step.name === 'actionlint')?.command?.(parseOptions([]), scope([]))
    expect(command).toEqual(Bun.which('actionlint') === null ? { skip: 'actionlint 1.7.12+ is not on PATH' } : ['actionlint', '-no-color'])
  })

  it('selects by step or group, and --fast leaves out everything that reads the build', () => {
    expect(selected(['--only', 'generated']).every(name => name.startsWith('verify-'))).toBe(true)
    expect(selected(['--only', 'lint,typecheck'])).toEqual(['typecheck', 'lint', 'actionlint'])
    expect(selected(['--skip', 'e2e'])).not.toContain('e2e')
    const fast = selected(['--fast'])
    for (const name of ['build', 'tui-spec', 'runtime', 'integration', 'e2e']) expect(fast).not.toContain(name)
    expect(fast).toContain('typecheck')
    expect(fast).toContain('tui-layout')
  })

  it('rejects what it does not know', () => {
    expect(() => parseOptions(['--only', 'nope'])).toThrow(/unknown step or group nope/u)
    expect(() => parseOptions(['--bogus'])).toThrow(/unknown argument/u)
    expect(() => parseOptions(['--base'])).toThrow(/needs a value/u)
  })

  it('checks the Rust workspace before its PTY scenarios and excludes both from --fast', () => {
    expect(selected(['--only', 'native'])).toEqual(['rust', 'rust-pty'])
    expect(selected(['--fast', '--only', 'native'])).toEqual([])
    expect(STEPS.find(step => step.name === 'rust')?.command?.(parseOptions([]), scope([])))
      .toEqual(['bun', 'run', 'check:rust'])
    expect(STEPS.find(step => step.name === 'rust-pty')?.command?.(parseOptions([]), scope([])))
      .toEqual(process.platform === 'win32' ? { skip: 'native ConPTY scenarios are not implemented' } : ['bun', 'run', 'test:rust:pty'])
  })

  it('blocks PTY checks after their own build fails, without blocking the other workspace', () => {
    for (const step of STEPS.filter(entry => ['e2e', 'rust-pty'].includes(entry.name))) {
      const owner = step.name === 'rust-pty' ? 'rust' : 'build'
      const other = owner === 'rust' ? 'build' : 'rust'
      expect(failedBuild(step, new Map([[owner, { outcome: 'fail' }], [other, { outcome: 'pass' }]]))).toBe(owner)
      expect(failedBuild(step, new Map([[owner, { outcome: 'pass' }], [other, { outcome: 'fail' }]]))).toBeUndefined()
      expect(failedBuild(step, new Map())).toBeUndefined()
    }
  })

  it('runs the runtime tests the change reaches, all of them when the change can move any, or none', () => {
    const options = parseOptions([])
    expect(runtimeArgs(options, scope(['packages/core/agent/src/index.ts']))).toContain('--changed')
    expect(runtimeArgs(options, scope(['packages/core/agent/src/index.ts']))).toContain('abc123')
    for (const wide of ['package.json', 'bun.lock', 'vitest.config.ts', 'tsconfig.base.json']) {
      expect(runtimeArgs(options, scope([wide]))).not.toContain('--changed')
    }
    expect(runtimeArgs(options, scope(['docs/development.md', 'apps/tui/packages/ui/src/app.tsx']))).toHaveProperty('skip')
    expect(runtimeArgs(options, unresolved)).toEqual(['node', 'node_modules/vitest/vitest.mjs', 'run'])
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

  it('reads the files a Vitest run failed in, could not start, or blamed for an unhandled error', () => {
    const rule = (title: string) => `⎯⎯⎯⎯⎯ ${title} ⎯⎯⎯⎯⎯`
    const log = [
      ' ✓ packages/core/agent/tests/agent.spec.ts (12 tests) 80ms',
      ' FAIL  packages/terminal/terminal-bash/tests/local.spec.ts > terminal-bash real shell > recognizes a foreground read',
      ' FAIL  packages/terminal/terminal-bash/tests/local.spec.ts > terminal-bash real shell > another case',
      ' FAIL  packages/ui/tests/placement.spec.tsx [ packages/ui/tests/placement.spec.tsx ]',
      ' FAIL  apps/cli/tests/keyless-smoke.e2e.ts > keyless smoke > starts',
      '   FAIL  not/a/test.ts > indented differently',
      rule('Unhandled Errors'),
      '',
      'Vitest caught 2 unhandled errors during the test run.',
      'This might cause false positive tests. Resolve unhandled errors to make sure your tests are not affected.',
      rule('Uncaught Exception'),
      'Error: A FileHandle object was closed during garbage collection.',
      'This error originated in "packages/subagent/tool-subagent-control/tests/tool-subagent-control.spec.ts" test file.',
      rule('Unhandled Error'),
      'Error: [vitest-pool]: Failed to start forks worker for test files /repo/scripts/vitest-environment.compat.spec.ts.',
      ' Test Files  1 failed',
    ].join('\n')
    expect(vitestFailures(log)).toEqual({ unattributed: 0, files: [
      '/repo/scripts/vitest-environment.compat.spec.ts',
      'apps/cli/tests/keyless-smoke.e2e.ts',
      'packages/subagent/tool-subagent-control/tests/tool-subagent-control.spec.ts',
      'packages/terminal/terminal-bash/tests/local.spec.ts',
      'packages/ui/tests/placement.spec.tsx',
    ] })
  })

  it('counts an unhandled error no test file was blamed for, which a rerun cannot clear', () => {
    const log = ['⎯⎯⎯ Unhandled Rejection ⎯⎯⎯', 'Error: socket hang up', ' Test Files  700 passed'].join('\n')
    expect(vitestFailures(log)).toEqual({ files: [], unattributed: 1 })
    expect(vitestFailures(' Test Files  1 failed\nError: worker crashed')).toEqual({ files: [], unattributed: 0 })
  })

  it('reruns failed files only for the Vitest steps', () => {
    const rerunnable = STEPS.filter(step => step.rerun !== undefined).map(step => step.name)
    expect(rerunnable).toEqual(['tui-spec', 'runtime', 'integration'])
    const runtime = STEPS.find(step => step.name === 'runtime')
    expect(runtime?.rerun?.(['a.spec.ts'])).toEqual(['node', 'node_modules/vitest/vitest.mjs', 'run', '--maxWorkers=1', 'a.spec.ts'])
    const integration = STEPS.find(step => step.name === 'integration')
    expect(integration?.rerun?.(['a.e2e.ts'])).toEqual(
      ['node', 'node_modules/vitest/vitest.mjs', 'run', '--config', 'vitest.e2e.config.ts', '--maxWorkers=1', 'a.e2e.ts'])
  })

  it('names every Bun test file under scripts and evals to bun test, and no Vitest spec', () => {
    const onDisk = (directory: string) => readdirSync(join(import.meta.dirname, '..', directory), { recursive: true, encoding: 'utf8' })
      .map(path => path.replaceAll('\\', '/'))
      .filter(path => path.endsWith('.test.ts') && !path.split('/').includes('node_modules'))
      .map(path => `./${directory}/${path}`).sort()
    expect(onDisk('scripts')).toContain('./scripts/preflight.test.ts')
    expect(onDisk('evals')).toContain('./evals/agent-loop/metrics.test.ts')
    expect(bunTests('scripts')).toEqual(onDisk('scripts'))
    expect(bunTests('evals')).toEqual(onDisk('evals'))
    const command = (name: string) => STEPS.find(step => step.name === name)?.command?.(parseOptions([]), scope([]))
    expect(command('scripts-unit')).toEqual(['bun', 'test', '--parallel', '--timeout=30000', ...onDisk('scripts')])
    expect(command('evals-unit')).toEqual(['bun', 'test', '--parallel', '--timeout=30000', ...onDisk('evals')])
  })
})
