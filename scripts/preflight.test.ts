import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import {
  bunTests, changelogGap, failedBuild, parseOptions, runStep, runtimeArgs, rustDocArgs, rustTestArgs, selectSteps, STEPS, strayBuildOutput,
  summaryRecord, type Result, type Scope,
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
      'verify-cordis-config', 'verify-package-invariants', 'verify-rust-migration-inventory', 'verify-rust-migration-ledger']) expect(names).toContain(gate)
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

  it('checks the Rust workspace before its PTY, package, and eval scenarios and excludes them from --fast', () => {
    expect(selected(['--only', 'native'])).toEqual(['rust-fmt', 'rust-clippy', 'rust-test', 'rust-doc', 'rust', 'rust-pty', 'rust-package', 'rust-install', 'rust-eval'])
    expect(selected(['--fast', '--only', 'native'])).toEqual([])
    expect(STEPS.find(step => step.name === 'rust')?.command?.(parseOptions([]), scope([])))
      .toEqual(['cargo', 'build', '--workspace', '--locked'])
    const rustChange = scope(['rust/crates/bake-ai/src/lib.rs'])
    expect(STEPS.find(step => step.name === 'rust-test')?.command?.(parseOptions(['--full']), rustChange))
      .toEqual(['bun', 'run', 'test:rust'])
    expect(STEPS.find(step => step.name === 'rust-doc')?.command?.(parseOptions([]), rustChange))
      .toEqual(['cargo', 'test', '--workspace', '--locked', '--doc', '--no-fail-fast'])
    expect(STEPS.find(step => step.name === 'rust-pty')?.command?.(parseOptions([]), scope([])))
      .toEqual(['bun', 'run', 'test:rust:pty'])
    expect(STEPS.find(step => step.name === 'rust-package')?.command?.(parseOptions([]), scope([])))
      .toEqual(['bun', 'run', 'test:rust:package'])
    expect(STEPS.find(step => step.name === 'rust-install')?.command?.(parseOptions([]), scope([])))
      .toEqual(process.platform === 'win32'
        ? { skip: 'the native installer rehearsal needs a POSIX shell; install.ps1 is checked on Windows' }
        : ['bun', 'run', 'test:rust:install'])
    expect(STEPS.find(step => step.name === 'rust-eval')?.command?.(parseOptions([]), scope([])))
      .toEqual(['bun', 'run', 'test:rust:eval'])
  })

  it('keeps the Cargo checks independently selectable and pins their working directory', () => {
    expect(selected(['--only', 'rust-fmt,rust-clippy'])).toEqual(['rust-fmt', 'rust-clippy'])
    for (const name of ['rust-fmt', 'rust-clippy', 'rust-doc', 'rust']) {
      expect(STEPS.find(step => step.name === name)?.cwd).toBe('rust')
    }
  })

  it('records a child failure once and runs an independent gate afterwards', async () => {
    const directory = mkdtempSync(join(tmpdir(), 'bake-preflight-'))
    try {
      const command = [process.execPath, '-e', 'console.log("fixture output"); process.exit(17)']
      const failed = await runStep({ name: 'failed', summary: 'failing child fixture', phase: 'build', group: 'native',
        command: () => command }, parseOptions([]), scope([]), directory)
      expect(failed.outcome).toBe('fail')
      expect(readFileSync(join(directory, 'failed.log'), 'utf8')).toContain('fixture output')
      const passed = await runStep({ name: 'passed', summary: 'successful child fixture', phase: 'build', group: 'native', cwd: 'rust',
        command: () => [process.execPath, '-e', 'console.log(process.cwd())'] }, parseOptions([]), scope([]), directory)
      expect(passed.outcome).toBe('pass')
      expect(readFileSync(join(directory, 'passed.log'), 'utf8')).toContain(join(import.meta.dirname, '..', 'rust'))
      expect(readdirSync(directory).sort()).toEqual(['failed.log', 'passed.log'])
    } finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it('blocks artifact checks after their own build fails, without blocking the other workspace', () => {
    for (const step of STEPS.filter(entry => ['e2e', 'rust-pty', 'rust-package', 'rust-install', 'rust-eval'].includes(entry.name))) {
      const owner = step.name === 'e2e' ? 'build' : 'rust'
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

  it('runs the Rust tests the change reaches, every one when asked or unscoped, or none', () => {
    const options = parseOptions([])
    const doc = ['cargo', 'test', '--workspace', '--locked', '--doc', '--no-fail-fast']
    expect(rustTestArgs(options, scope(['rust/crates/bake-tui-view/src/lib.rs']), false)).toEqual(['bun', 'run', 'test:rust', '--changed', 'abc123'])
    expect(rustTestArgs(options, scope(['CHANGELOG.md']), false)).toEqual(['bun', 'run', 'test:rust', '--changed', 'abc123'])
    for (const files of [[], ['docs/development.md', 'packages/core/agent/src/index.ts']]) {
      expect(rustTestArgs(options, scope(files), false)).toEqual({ skip: 'no Rust input changed (--full runs it anyway)' })
      expect(rustDocArgs(options, scope(files), false)).toEqual({ skip: 'no Rust input changed (--full runs it anyway)' })
      // --full, CI, and a missing merge base each run everything.
      expect(rustTestArgs(parseOptions(['--full']), scope(files), false)).toEqual(['bun', 'run', 'test:rust'])
      expect(rustTestArgs(options, scope(files), true)).toEqual(['bun', 'run', 'test:rust'])
      expect(rustDocArgs(options, scope(files), true)).toEqual(doc)
    }
    expect(rustTestArgs(options, unresolved, false)).toEqual(['bun', 'run', 'test:rust'])
    expect(rustDocArgs(options, unresolved, false)).toEqual(doc)
    expect(rustDocArgs(options, scope(['rust/Cargo.lock']), false)).toEqual(doc)
  })

  it('records each run step in table order with its outcome and seconds', () => {
    const results = new Map<string, Result>([
      ['rust-test', { outcome: 'fail', seconds: 12.345, note: '.preflight/rust-test.log' }],
      ['rust-fmt', { outcome: 'pass', seconds: 1.26 }],
      ['rust-doc', { outcome: 'skip', seconds: 0, note: 'no Rust input changed (--full runs it anyway)' }],
    ])
    expect(summaryRecord(results, 20.04)).toEqual({
      seconds: 20,
      steps: [
        { step: 'rust-fmt', outcome: 'pass', seconds: 1.3 },
        { step: 'rust-test', outcome: 'fail', seconds: 12.3, note: '.preflight/rust-test.log' },
        { step: 'rust-doc', outcome: 'skip', seconds: 0, note: 'no Rust input changed (--full runs it anyway)' },
      ],
    })
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
