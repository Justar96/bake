#!/usr/bin/env bun
/**
 * Measure Rust line and region coverage with cargo-llvm-cov and the pinned
 * toolchain's llvm-tools. The nextest suite and bake-codemode's `codemode`
 * engine binary run separately, as in scripts/rust-test.ts, and their
 * profiles merge into one report. Instrumented builds live in cargo-llvm-cov's
 * own rust/target/llvm-cov-target, and reports in rust/target/llvm-cov.
 */
import { mkdirSync, readdirSync, readFileSync, renameSync } from 'node:fs'
import { join, relative, resolve, sep } from 'node:path'
import { parseArgs } from 'node:util'

const REPO_ROOT = resolve(import.meta.dirname, '..')
const RUST_ROOT = join(REPO_ROOT, 'rust')
/** Ignored by the repository's `rust/target/` entry. */
const REPORT_ROOT = join(RUST_ROOT, 'target', 'llvm-cov')
/** Where cargo-llvm-cov points LLVM_PROFILE_FILE and merges every `*.profraw` from. */
const PROFILE_ROOT = join(RUST_ROOT, 'target', 'llvm-cov-target')

/** The cargo-llvm-cov release this script and the rust-quality workflow use. */
export const CARGO_LLVM_COV_VERSION = '0.9.1'
export const CODEMODE_PACKAGE = 'bake-codemode'

/**
 * Tests that check the exact stderr or exit status of a confined helper
 * process. Instrumented, that helper cannot write its profile, and the
 * profile runtime's error changes what the test observes, so coverage runs
 * leave them out.
 */
export const PROFILE_BLOCKED_TESTS: readonly { readonly package: string; readonly binary: string; readonly test: string }[] = [
  { package: 'bake-sandbox', binary: 'bake-sandbox::enforcement', test: 'landlock::helper_cli_contract' },
  { package: 'bake-cli', binary: 'bake-cli::wasm_plugin_host', test: 'the_helper_refuses_malformed_frames_before_allocating' },
  { package: 'bake-cli', binary: 'bake-cli::wasm_plugin_host', test: 'the_helper_keeps_one_request_and_checks_ids_and_generations' },
]
/** How many of the files with the most uncovered lines to list. */
export const LARGEST_UNCOVERED = 15

const USAGE = `usage: bun run coverage:rust [--html] [--lcov] [-p <crate>]... [--install]

Runs the Rust tests under cargo-llvm-cov and prints line and region coverage
per crate and the ${String(LARGEST_UNCOVERED)} files with the most uncovered lines. Reports go to
rust/target/llvm-cov: coverage.json and summary.md always, lcov.info with
--lcov, and html/index.html with --html.

  -p <crate>   test and report only these workspace packages
  --install    add llvm-tools to the pinned toolchain and install
               cargo-llvm-cov ${CARGO_LLVM_COV_VERSION} when missing`

export interface CoverageOptions {
  readonly html: boolean
  readonly lcov: boolean
  readonly packages: readonly string[]
  readonly install: boolean
}

export function parseCoverageArgs(args: readonly string[]): CoverageOptions {
  const { values, positionals } = parseArgs({
    args: [...args],
    allowPositionals: true,
    options: {
      html: { type: 'boolean' },
      lcov: { type: 'boolean' },
      package: { type: 'string', short: 'p', multiple: true },
      install: { type: 'boolean' },
    },
  })
  if (positionals.length > 0) throw new Error(`unexpected argument: ${positionals[0] ?? ''}\n${USAGE}`)
  return {
    html: values.html === true,
    lcov: values.lcov === true,
    packages: values.package ?? [],
    install: values.install === true,
  }
}

/** The `channel` that rust/rust-toolchain.toml pins. */
export function pinnedChannel(toolchainToml: string): string {
  const match = /^\s*channel\s*=\s*"([^"]+)"/mu.exec(toolchainToml)
  if (match?.[1] === undefined) throw new Error('coverage:rust: rust/rust-toolchain.toml pins no channel')
  return match[1]
}

/** Whether `rustup component list --installed` output includes llvm-tools, whose name carries the host triple. */
export function hasLlvmTools(installedComponents: string): boolean {
  return installedComponents.split('\n').some(line => /^llvm-tools(-preview)?(-|$)/u.test(line.trim()))
}

/**
 * Test helpers and fixtures that are not product logic, matching
 * rust/.cargo/mutants.toml. cargo-llvm-cov already ignores `tests/`,
 * `tests.rs`, and `*_tests.rs`.
 */
export function ignoreFilenameRegex(rustRoot: string): string {
  const root = rustRoot.split(sep).join('/').replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')
  return `/(test_server|http_test_server|test_support)\\.rs$|^${root}/(test-support|probes|plugins/fixtures)/`
}

const inScope = (packages: readonly string[], name: string): boolean => packages.length === 0 || packages.includes(name)

/** The nextest filter of the coverage run, naming only binaries in scope; undefined when it would exclude nothing. */
export function nextestFilter(packages: readonly string[]): string | undefined {
  const clauses = [
    // These engine tests share a compiled Wasm engine in one process; they run under cargo test.
    ...inScope(packages, CODEMODE_PACKAGE) ? ['not binary_id(bake-codemode::codemode)'] : [],
    ...PROFILE_BLOCKED_TESTS.filter(blocked => inScope(packages, blocked.package))
      .map(blocked => `not (binary_id(${blocked.binary}) and test(=${blocked.test}))`),
  ]
  return clauses.length === 0 ? undefined : clauses.join(' and ')
}

/** Arguments after `cargo`, in order: clean, the test runs, then the reports. */
export function coverageCommands(options: CoverageOptions, rustRoot: string, reportRoot: string): string[][] {
  const scope = options.packages.length === 0 ? ['--workspace'] : options.packages.flatMap(name => ['--package', name])
  const filter = nextestFilter(options.packages)
  // A child process killed mid-run leaves a truncated profile; merge the others.
  const report = ['llvm-cov', 'report', '--failure-mode', 'all', '--ignore-filename-regex', ignoreFilenameRegex(rustRoot)]
  const commands = [
    ['llvm-cov', 'clean', '--workspace'],
    // `--no-report` keeps the other run's profiles; `report` merges them.
    ['llvm-cov', 'nextest', '--no-report', '--locked', ...scope, '--no-fail-fast', ...filter === undefined ? [] : ['-E', filter]],
  ]
  if (inScope(options.packages, CODEMODE_PACKAGE)) {
    commands.push(['llvm-cov', 'test', '--no-report', '--locked', '--workspace', '--test', 'codemode', '--no-fail-fast'])
  }
  commands.push([...report, '--json', '--summary-only', '--output-path', join(reportRoot, 'coverage.json')])
  if (options.lcov) commands.push([...report, '--lcov', '--output-path', join(reportRoot, 'lcov.info')])
  if (options.html) commands.push([...report, '--html', '--output-dir', reportRoot])
  return commands
}

/** The cargo-llvm-cov steps that run tests; their failure still leaves a report. */
export const isTestCommand = (command: readonly string[]): boolean => command[1] === 'nextest' || command[1] === 'test'

/** The profile name the LLVM runtime uses without LLVM_PROFILE_FILE: `default_<module>_<pid>`. */
const STRAY_PROFILE = /^default_[\w-]+\.profraw$/u

/**
 * Move the profiles that instrumented children started with a cleared
 * environment wrote into their working directory, a package directory, into
 * the merged profile directory, so their coverage counts and the source tree
 * stays clean.
 * @returns how many profiles moved.
 */
export function collectStrayProfiles(dirs: readonly string[], profileRoot: string): number {
  let moved = 0
  for (const dir of dirs) {
    let names: string[]
    try {
      names = readdirSync(dir)
    } catch {
      continue
    }
    for (const name of names.filter(entry => STRAY_PROFILE.test(entry))) {
      renameSync(join(dir, name), join(profileRoot, `stray-${name}`))
      moved += 1
    }
  }
  return moved
}

export interface Counts {
  readonly covered: number
  readonly count: number
}

export interface FileCoverage {
  /** Relative to rust/. */
  readonly path: string
  readonly crate: string
  readonly lines: Counts
  readonly regions: Counts
}

export interface WorkspacePackage {
  readonly name: string
  /** The directory of the package manifest, relative to rust/ and ending in `/`. */
  readonly dir: string
}

/** Workspace packages from `cargo metadata --no-deps`, deepest directory first. */
export function workspacePackages(metadata: unknown, rustRoot: string): WorkspacePackage[] {
  const packages = typeof metadata === 'object' && metadata !== null ? (metadata as { packages?: unknown }).packages : undefined
  if (!Array.isArray(packages)) throw new Error('coverage:rust: cargo metadata lists no packages')
  return packages.flatMap((entry: unknown) => {
    const { name, manifest_path: manifest } = entry as { name?: unknown; manifest_path?: unknown }
    if (typeof name !== 'string' || typeof manifest !== 'string') return []
    const dir = relative(rustRoot, resolve(manifest, '..')).split(sep).join('/')
    return [{ name, dir: dir === '' ? '' : `${dir}/` }]
  }).sort((a, b) => b.dir.length - a.dir.length)
}

function counts(value: unknown): Counts {
  const { covered, count } = typeof value === 'object' && value !== null ? value as { covered?: unknown; count?: unknown } : {}
  const valid = (n: unknown): number => typeof n === 'number' && Number.isSafeInteger(n) && n >= 0 ? n : 0
  return { covered: valid(covered), count: valid(count) }
}

/** Per-file coverage from an `llvm-cov export` JSON report, attributed to workspace packages. */
export function fileCoverage(report: unknown, packages: readonly WorkspacePackage[], rustRoot: string): FileCoverage[] {
  const data = typeof report === 'object' && report !== null ? (report as { data?: unknown }).data : undefined
  if (!Array.isArray(data)) throw new Error('coverage:rust: the coverage report has no data')
  const root = rustRoot.split(sep).join('/')
  return data.flatMap((export_: unknown) => {
    const files = typeof export_ === 'object' && export_ !== null ? (export_ as { files?: unknown }).files : undefined
    return Array.isArray(files) ? files : []
  }).flatMap((file: unknown) => {
    const { filename, summary } = file as { filename?: unknown; summary?: { lines?: unknown; regions?: unknown } }
    if (typeof filename !== 'string') return []
    const absolute = filename.split(sep).join('/')
    const path = absolute.startsWith(`${root}/`) ? absolute.slice(root.length + 1) : absolute
    const crate = packages.find(pkg => path.startsWith(pkg.dir))?.name ?? '(outside the workspace)'
    return [{ path, crate, lines: counts(summary?.lines), regions: counts(summary?.regions) }]
  })
}

export interface CrateCoverage {
  readonly crate: string
  readonly lines: Counts
  readonly regions: Counts
}

const add = (a: Counts, b: Counts): Counts => ({ covered: a.covered + b.covered, count: a.count + b.count })
const ZERO: Counts = { covered: 0, count: 0 }

/** Sum files per crate, sorted by name, keeping only the chosen crates when there are any. */
export function crateCoverage(files: readonly FileCoverage[], only: readonly string[]): CrateCoverage[] {
  const byCrate = new Map<string, CrateCoverage>()
  for (const file of files) {
    if (only.length > 0 && !only.includes(file.crate)) continue
    const total = byCrate.get(file.crate) ?? { crate: file.crate, lines: ZERO, regions: ZERO }
    byCrate.set(file.crate, { crate: file.crate, lines: add(total.lines, file.lines), regions: add(total.regions, file.regions) })
  }
  return [...byCrate.values()].sort((a, b) => a.crate.localeCompare(b.crate))
}

/** The files with the most uncovered lines, most first. */
export function largestUncovered(files: readonly FileCoverage[], only: readonly string[], limit: number): FileCoverage[] {
  const uncovered = (file: FileCoverage): number => file.lines.count - file.lines.covered
  return files
    .filter(file => (only.length === 0 || only.includes(file.crate)) && uncovered(file) > 0)
    .sort((a, b) => uncovered(b) - uncovered(a) || a.path.localeCompare(b.path))
    .slice(0, limit)
}

const percent = ({ covered, count }: Counts): string => count === 0 ? '-' : `${(100 * covered / count).toFixed(1)}%`

/** A Markdown summary, readable in a terminal and in a GitHub step summary. */
export function formatCoverage(crates: readonly CrateCoverage[], largest: readonly FileCoverage[]): string {
  const total = crates.reduce(
    (sum, row) => ({ lines: add(sum.lines, row.lines), regions: add(sum.regions, row.regions) }), { lines: ZERO, regions: ZERO },
  )
  const row = (name: string, lines: Counts, regions: Counts): string =>
    `| ${name} | ${String(lines.covered)}/${String(lines.count)} | ${percent(lines)} | ${String(regions.covered)}/${String(regions.count)} | ${percent(regions)} |`
  return [
    '| Crate | Lines | Line % | Regions | Region % |',
    '| --- | ---: | ---: | ---: | ---: |',
    ...crates.map(crate => row(crate.crate, crate.lines, crate.regions)),
    row('**Total**', total.lines, total.regions),
    '',
    `Files with the most uncovered lines (top ${String(LARGEST_UNCOVERED)}):`,
    '',
    '| File | Uncovered lines | Line % |',
    '| --- | ---: | ---: |',
    ...largest.map(file => `| rust/${file.path} | ${String(file.lines.count - file.lines.covered)} | ${percent(file.lines)} |`),
  ].join('\n')
}

function run(command: readonly string[], cwd: string): Promise<number> {
  console.log(`coverage:rust: ${command.join(' ')}`)
  return Bun.spawn([...command], { cwd, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' }).exited
}

/** Check, and with --install provide, cargo-llvm-cov and the pinned toolchain's llvm-tools. */
async function ensureTools(install: boolean): Promise<boolean> {
  const channel = pinnedChannel(readFileSync(join(RUST_ROOT, 'rust-toolchain.toml'), 'utf8'))
  let ready = true
  if (Bun.which('rustup') !== null) {
    const listed = Bun.spawnSync(['rustup', 'component', 'list', '--installed', '--toolchain', channel], { cwd: RUST_ROOT })
    if (listed.exitCode !== 0 || !hasLlvmTools(listed.stdout.toString())) {
      const add = ['rustup', 'component', 'add', 'llvm-tools', '--toolchain', channel]
      if (install) ready &&= await run(add, RUST_ROOT) === 0
      else {
        console.error(`coverage:rust: the pinned toolchain ${channel} lacks llvm-tools; run \`${add.join(' ')}\` or pass --install.`)
        ready = false
      }
    }
  }
  if (Bun.which('cargo-llvm-cov') === null) {
    const add = ['cargo', 'install', '--locked', `cargo-llvm-cov@${CARGO_LLVM_COV_VERSION}`]
    if (install) ready &&= await run(add, RUST_ROOT) === 0
    else {
      console.error(`coverage:rust: cargo-llvm-cov is not installed; run \`${add.join(' ')}\` or pass --install.`)
      ready = false
    }
  }
  if (Bun.which('cargo-nextest') === null) {
    console.error('coverage:rust: cargo-nextest is not installed; see rust/README.md.')
    ready = false
  }
  return ready
}

async function main(): Promise<number> {
  if (process.argv.includes('--help') || process.argv.includes('-h')) {
    console.log(USAGE)
    return 0
  }
  const options = parseCoverageArgs(process.argv.slice(2))
  if (!await ensureTools(options.install)) return 1
  mkdirSync(REPORT_ROOT, { recursive: true })
  const metadata = Bun.spawnSync(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'], { cwd: RUST_ROOT })
  if (metadata.exitCode !== 0) throw new Error(`coverage:rust: cargo metadata failed: ${metadata.stderr.toString()}`)
  const packages = workspacePackages(JSON.parse(metadata.stdout.toString()), RUST_ROOT)
  let testFailure = 0
  let collected = false
  for (const command of coverageCommands(options, RUST_ROOT, REPORT_ROOT)) {
    if (!collected && command[1] === 'report') {
      collected = true
      const moved = collectStrayProfiles([RUST_ROOT, ...packages.map(pkg => join(RUST_ROOT, pkg.dir))], PROFILE_ROOT)
      if (moved > 0) console.log(`coverage:rust: merging ${String(moved)} profiles that children wrote into package directories.`)
    }
    const code = await run(['cargo', ...command], RUST_ROOT)
    if (code === 0) continue
    // A failing test still leaves profiles to report; anything else stops the run.
    if (!isTestCommand(command)) return code
    testFailure = code
  }
  const files = fileCoverage(JSON.parse(readFileSync(join(REPORT_ROOT, 'coverage.json'), 'utf8')), packages, RUST_ROOT)
  const summary = formatCoverage(
    crateCoverage(files, options.packages), largestUncovered(files, options.packages, LARGEST_UNCOVERED),
  )
  await Bun.write(join(REPORT_ROOT, 'summary.md'), `${summary}\n`)
  console.log(`\n${summary}\n`)
  console.log(`coverage:rust: reports under ${relative(REPO_ROOT, REPORT_ROOT)}`)
  if (testFailure !== 0) console.error('coverage:rust: some tests failed; the report covers the runs that finished.')
  return testFailure
}

if (import.meta.main) {
  try {
    process.exit(await main())
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(2)
  }
}
