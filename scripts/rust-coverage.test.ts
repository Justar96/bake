import { afterEach, describe, expect, it } from 'bun:test'
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  CODEMODE_PACKAGE, collectStrayProfiles, type CoverageOptions, coverageCommands, crateCoverage, type FileCoverage, fileCoverage,
  formatCoverage, hasLlvmTools, ignoreFilenameRegex, isTestCommand, largestUncovered, nextestFilter, parseCoverageArgs,
  pinnedChannel, PROFILE_BLOCKED_TESTS, workspacePackages,
} from './rust-coverage.ts'

const RUST = '/repo/rust'
const OUT = '/repo/rust/target/llvm-cov'
const options = (overrides: Partial<CoverageOptions> = {}): CoverageOptions => ({
  html: false, lcov: false, packages: [], install: false, ...overrides,
})

describe('Rust coverage arguments and tools', () => {
  it('collects report formats, packages, and --install, and rejects stray arguments', () => {
    expect(parseCoverageArgs([])).toEqual(options())
    expect(parseCoverageArgs(['--html', '--lcov', '-p', 'bake-ai', '--package', 'bake-tui', '--install']))
      .toEqual({ html: true, lcov: true, packages: ['bake-ai', 'bake-tui'], install: true })
    expect(() => parseCoverageArgs(['bake-ai'])).toThrow('unexpected argument')
  })

  it('reads the pinned channel and finds llvm-tools under its host-qualified name', () => {
    expect(pinnedChannel('[toolchain]\nchannel = "1.99.0"\nprofile = "minimal"\n')).toBe('1.99.0')
    expect(() => pinnedChannel('[toolchain]\n')).toThrow('no channel')
    expect(hasLlvmTools('cargo-x86_64-unknown-linux-gnu\nllvm-tools-x86_64-unknown-linux-gnu\n')).toBe(true)
    expect(hasLlvmTools('llvm-tools-preview\n')).toBe(true)
    expect(hasLlvmTools('cargo-x86_64-unknown-linux-gnu\nrustc-x86_64-unknown-linux-gnu\n')).toBe(false)
    expect(hasLlvmTools('llvm-toolsmith\n')).toBe(false)
  })

  it('ignores test servers, test support, probes, and fixtures', () => {
    const regex = new RegExp(ignoreFilenameRegex(RUST), 'u')
    for (const path of [
      '/repo/rust/crates/bake-ai/src/test_server.rs', '/repo/rust/crates/bake-mcp/src/http_test_server.rs',
      '/repo/rust/crates/bake-tui/src/test_support.rs', '/repo/rust/test-support/harness.rs',
      '/repo/rust/probes/windows-config-trust/src/main.rs', '/repo/rust/plugins/fixtures/hostile/src/lib.rs',
    ]) expect(regex.test(path)).toBe(true)
    for (const path of ['/repo/rust/crates/bake-ai/src/utils/sse.rs', '/repo/rust/plugins/guard/src/lib.rs', '/elsewhere/probes/x.rs']) {
      expect(regex.test(path)).toBe(false)
    }
  })
})

describe('Rust coverage commands', () => {
  it('runs nextest without the codemode binary, then codemode with cargo test, and merges one report', () => {
    const commands = coverageCommands(options(), RUST, OUT)
    expect(commands.slice(0, 3)).toEqual([
      ['llvm-cov', 'clean', '--workspace'],
      ['llvm-cov', 'nextest', '--no-report', '--locked', '--workspace', '--no-fail-fast', '-E', nextestFilter([]) ?? ''],
      ['llvm-cov', 'test', '--no-report', '--locked', '--workspace', '--test', 'codemode', '--no-fail-fast'],
    ])
    expect(commands.slice(3)).toEqual([[
      'llvm-cov', 'report', '--failure-mode', 'all', '--ignore-filename-regex', ignoreFilenameRegex(RUST),
      '--json', '--summary-only', '--output-path', `${OUT}/coverage.json`,
    ]])
    expect(commands.map(isTestCommand)).toEqual([false, true, true, false])
  })

  it('leaves out the codemode binary and the tests whose confined helper cannot write a profile', () => {
    expect(nextestFilter([])).toBe([
      'not binary_id(bake-codemode::codemode)',
      'not (binary_id(bake-sandbox::enforcement) and test(=landlock::helper_cli_contract))',
      'not (binary_id(bake-cli::wasm_plugin_host) and test(=the_helper_refuses_malformed_frames_before_allocating))',
      'not (binary_id(bake-cli::wasm_plugin_host) and test(=the_helper_keeps_one_request_and_checks_ids_and_generations))',
    ].join(' and '))
    expect(PROFILE_BLOCKED_TESTS.every(blocked => blocked.binary.startsWith(`${blocked.package}::`))).toBe(true)
    expect(nextestFilter(['bake-sandbox'])).toBe('not (binary_id(bake-sandbox::enforcement) and test(=landlock::helper_cli_contract))')
    expect(nextestFilter(['bake-ai'])).toBeUndefined()
  })

  it('adds lcov and html reports on request', () => {
    const reports = coverageCommands(options({ html: true, lcov: true }), RUST, OUT).slice(3).map(command => command.slice(6))
    expect(reports).toEqual([
      ['--json', '--summary-only', '--output-path', `${OUT}/coverage.json`],
      ['--lcov', '--output-path', `${OUT}/lcov.info`],
      ['--html', '--output-dir', OUT],
    ])
  })

  it('runs the codemode binary only when its package is in scope', () => {
    const ai = coverageCommands(options({ packages: ['bake-ai'] }), RUST, OUT)
    expect(ai[1]).toEqual(['llvm-cov', 'nextest', '--no-report', '--locked', '--package', 'bake-ai', '--no-fail-fast'])
    expect(ai.filter(isTestCommand)).toHaveLength(1)
    const codemode = coverageCommands(options({ packages: [CODEMODE_PACKAGE] }), RUST, OUT)
    expect(codemode[1]?.slice(-2)).toEqual(['-E', 'not binary_id(bake-codemode::codemode)'])
    expect(codemode[2]).toEqual(expect.arrayContaining(['--test', 'codemode']))
  })
})

describe('Rust coverage profiles', () => {
  const temporaryPaths: string[] = []
  afterEach(() => {
    for (const path of temporaryPaths.splice(0)) rmSync(path, { recursive: true, force: true })
  })

  it('moves profiles that children wrote into package directories to the merged profile directory', () => {
    const root = mkdtempSync(join(tmpdir(), 'rust-coverage-test-'))
    temporaryPaths.push(root)
    const pkg = join(root, 'crates/bake-cli')
    const profiles = join(root, 'target/llvm-cov-target')
    mkdirSync(pkg, { recursive: true })
    mkdirSync(profiles, { recursive: true })
    writeFileSync(join(pkg, 'default_9372795819043660128_0_3022326.profraw'), 'p')
    writeFileSync(join(pkg, 'Cargo.toml'), '')
    writeFileSync(join(pkg, 'notes.profraw'), '')
    expect(collectStrayProfiles([root, pkg, join(root, 'missing')], profiles)).toBe(1)
    expect(readdirSync(pkg).sort()).toEqual(['Cargo.toml', 'notes.profraw'])
    expect(readdirSync(profiles)).toEqual(['stray-default_9372795819043660128_0_3022326.profraw'])
  })
})

describe('Rust coverage summary', () => {
  const metadata = {
    packages: [
      { name: 'bake-ai', manifest_path: '/repo/rust/crates/bake-ai/Cargo.toml' },
      { name: 'bake-ai-macros', manifest_path: '/repo/rust/crates/bake-ai/macros/Cargo.toml' },
      { name: 'bake-tui', manifest_path: '/repo/rust/crates/bake-tui/Cargo.toml' },
      { name: 7 },
    ],
  }
  const packages = workspacePackages(metadata, RUST)
  const summary = (lines: [number, number], regions: [number, number]) => ({
    lines: { covered: lines[0], count: lines[1], percent: 0 }, regions: { covered: regions[0], count: regions[1] },
  })
  const report = {
    data: [{
      files: [
        { filename: '/repo/rust/crates/bake-ai/src/sse.rs', summary: summary([80, 100], [40, 50]) },
        { filename: '/repo/rust/crates/bake-ai/src/retry.rs', summary: summary([10, 40], [5, 20]) },
        { filename: '/repo/rust/crates/bake-ai/macros/src/lib.rs', summary: summary([0, 30], [0, 10]) },
        { filename: '/repo/rust/crates/bake-tui/src/app.rs', summary: summary([30, 30], [12, 12]) },
        { filename: '/repo/vendor/x.rs', summary: summary([0, 5], [0, 1]) },
        { filename: '/repo/rust/crates/bake-tui/src/bad.rs', summary: { lines: { covered: -1, count: 'x' } } },
        { summary: summary([1, 1], [1, 1]) },
      ],
    }],
  }
  const files = fileCoverage(report, packages, RUST)

  it('attributes files to the deepest workspace package and skips malformed counts', () => {
    expect(packages.map(pkg => pkg.dir)).toEqual(['crates/bake-ai/macros/', 'crates/bake-tui/', 'crates/bake-ai/'])
    expect(files.map(file => [file.path, file.crate])).toEqual([
      ['crates/bake-ai/src/sse.rs', 'bake-ai'],
      ['crates/bake-ai/src/retry.rs', 'bake-ai'],
      ['crates/bake-ai/macros/src/lib.rs', 'bake-ai-macros'],
      ['crates/bake-tui/src/app.rs', 'bake-tui'],
      ['/repo/vendor/x.rs', '(outside the workspace)'],
      ['crates/bake-tui/src/bad.rs', 'bake-tui'],
    ])
    expect(files[5]).toMatchObject({ lines: { covered: 0, count: 0 }, regions: { covered: 0, count: 0 } })
    expect(() => fileCoverage({}, packages, RUST)).toThrow('no data')
    expect(() => workspacePackages({}, RUST)).toThrow('no packages')
  })

  it('sums crates and ranks files by uncovered lines', () => {
    expect(crateCoverage(files, [])).toEqual([
      { crate: '(outside the workspace)', lines: { covered: 0, count: 5 }, regions: { covered: 0, count: 1 } },
      { crate: 'bake-ai', lines: { covered: 90, count: 140 }, regions: { covered: 45, count: 70 } },
      { crate: 'bake-ai-macros', lines: { covered: 0, count: 30 }, regions: { covered: 0, count: 10 } },
      { crate: 'bake-tui', lines: { covered: 30, count: 30 }, regions: { covered: 12, count: 12 } },
    ])
    expect(crateCoverage(files, ['bake-tui']).map(row => row.crate)).toEqual(['bake-tui'])
    const ranked = (only: string[], limit: number): string[] => largestUncovered(files, only, limit).map((file: FileCoverage) => file.path)
    expect(ranked([], 15)).toEqual(['crates/bake-ai/macros/src/lib.rs', 'crates/bake-ai/src/retry.rs', 'crates/bake-ai/src/sse.rs', '/repo/vendor/x.rs'])
    expect(ranked([], 2)).toEqual(['crates/bake-ai/macros/src/lib.rs', 'crates/bake-ai/src/retry.rs'])
    expect(ranked(['bake-tui'], 15)).toEqual([])
  })

  it('formats a Markdown table with totals and the largest gaps', () => {
    const text = formatCoverage(crateCoverage(files, ['bake-ai', 'bake-tui']), largestUncovered(files, ['bake-ai'], 1))
    expect(text.split('\n')).toEqual([
      '| Crate | Lines | Line % | Regions | Region % |',
      '| --- | ---: | ---: | ---: | ---: |',
      '| bake-ai | 90/140 | 64.3% | 45/70 | 64.3% |',
      '| bake-tui | 30/30 | 100.0% | 12/12 | 100.0% |',
      '| **Total** | 120/170 | 70.6% | 57/82 | 69.5% |',
      '',
      'Files with the most uncovered lines (top 15):',
      '',
      '| File | Uncovered lines | Line % |',
      '| --- | ---: | ---: |',
      '| rust/crates/bake-ai/src/retry.rs | 30 | 25.0% |',
    ])
    expect(formatCoverage([], [])).toContain('| **Total** | 0/0 | - | 0/0 | - |')
  })
})
