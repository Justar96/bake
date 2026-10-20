/**
 * Which Rust workspace packages a change can affect, so local gates test those
 * packages and their reverse dependents instead of the whole workspace.
 *
 * A changed path inside a workspace member selects that member. A path under
 * `rust/` outside every member (the workspace manifest, lockfile, toolchain
 * pin, nextest config, shared test support) can move any test, so it widens
 * to the full suite, as do the scripts that make this decision. Rust sources
 * also read files outside their own package; `RUST_EXTERNAL_INPUTS` names
 * each one and its consumers, and `rust-scope.test.ts` scans the sources so
 * the table cannot fall behind. Any other path outside `rust/` selects nothing.
 */
import { resolve } from 'node:path'

const ROOT = resolve(import.meta.dirname, '..')

/** A file or directory (ending in `/`) outside its readers' packages, with the packages that read it. */
export interface RustExternalInput {
  /** Repository-relative path; a trailing `/` covers everything below it. */
  readonly path: string
  readonly consumers: readonly string[]
  /** How the consumers read it. */
  readonly read: string
}

/** Every non-Cargo input a Rust package reads from outside its own directory. */
export const RUST_EXTERNAL_INPUTS: readonly RustExternalInput[] = [
  { path: 'CHANGELOG.md', consumers: ['bake-tui'], read: 'include_str! in live/commands/changelog.rs' },
  { path: 'distribution/host/native-release-key.pub', consumers: ['bake-update'], read: 'include_str! in manifest.rs' },
  {
    path: 'rust/crates/bake-plugin-sdk/wit/', consumers: ['bake-cli', 'bake-coding-agent'],
    read: 'the plugin world: wasmtime bindgen! path and include_str! in tests/wasm_plugin_host/fixtures.rs',
  },
  {
    path: 'rust/plugins/guard/', consumers: ['bake-cli', 'bake-coding-agent'],
    read: 'the committed guard component, read by plugin host and installer tests',
  },
  {
    path: 'rust/plugins/fixtures/hostile/', consumers: ['bake-cli'],
    read: 'the committed hostile component, read by plugin host, session, and MCP tests',
  },
  {
    path: 'rust/crates/bake-coding-agent/tests/mcp_session/', consumers: ['bake-cli'],
    read: '#[path] modules in tests/mcp.rs',
  },
  {
    path: 'rust/crates/bake-coding-agent/src/plugins/wasm/process.rs', consumers: ['bake-cli'],
    read: '#[path] module in tests/wasm_plugin_host.rs',
  },
]

/** Scripts that decide which Rust tests run; a change to them checks itself against the whole suite. */
export const RUST_SCOPE_RUNNERS: readonly string[] = ['scripts/rust-scope.ts', 'scripts/rust-test.ts']

/** A workspace member and the members it depends on through any dependency kind. */
export interface RustPackage {
  readonly name: string
  /** Repository-relative directory holding its `Cargo.toml`, without a trailing `/`. */
  readonly dir: string
  readonly dependencies: readonly string[]
}

/** The Rust tests a change needs. */
export type RustScope =
  | { readonly kind: 'full'; readonly reason: string }
  | { readonly kind: 'none' }
  | {
    readonly kind: 'packages'
    /** Packages the change touched directly, each with the first path that selected it. */
    readonly changed: ReadonlyMap<string, string>
    /** The changed packages and every transitive reverse dependent, sorted. */
    readonly packages: readonly string[]
  }

function covers(input: string, path: string): boolean {
  return input.endsWith('/') ? path.startsWith(input) : path === input
}

/** Whether a path can affect any Rust test; false for everything a Rust package never reads. */
export function isRustInput(path: string): boolean {
  return path.startsWith('rust/') || RUST_SCOPE_RUNNERS.includes(path)
    || RUST_EXTERNAL_INPUTS.some(input => covers(input.path, path))
}

/** The packages a change selects, widened to every transitive reverse dependent. */
export function rustScope(files: readonly string[], packages: readonly RustPackage[]): RustScope {
  // The deepest member directory owns a path, should members ever nest.
  const members = [...packages].sort((a, b) => b.dir.length - a.dir.length)
  const changed = new Map<string, string>()
  const select = (name: string, why: string): void => {
    if (!changed.has(name)) changed.set(name, why)
  }
  for (const file of files) {
    if (RUST_SCOPE_RUNNERS.includes(file)) return { kind: 'full', reason: `${file} decides which Rust tests run` }
    for (const input of RUST_EXTERNAL_INPUTS) {
      if (covers(input.path, file)) for (const consumer of input.consumers) select(consumer, `${file} (${input.read})`)
    }
    if (!file.startsWith('rust/')) continue
    const owner = members.find(member => file.startsWith(`${member.dir}/`))
    if (owner === undefined) return { kind: 'full', reason: `${file} is a workspace-wide Rust input` }
    select(owner.name, file)
  }
  return changed.size === 0 ? { kind: 'none' } : withReverseDependents(changed, packages)
}

/** A scope of the given packages, each with why it was selected, and every transitive reverse dependent. */
export function withReverseDependents(changed: ReadonlyMap<string, string>, packages: readonly RustPackage[]): Extract<RustScope, { kind: 'packages' }> {
  const dependents = new Map<string, string[]>()
  for (const member of packages) {
    for (const dependency of member.dependencies) dependents.set(dependency, [...dependents.get(dependency) ?? [], member.name])
  }
  const closure = new Set<string>()
  const queue = [...changed.keys()]
  for (let name = queue.shift(); name !== undefined; name = queue.shift()) {
    if (closure.has(name)) continue
    closure.add(name)
    queue.push(...dependents.get(name) ?? [])
  }
  return { kind: 'packages', changed, packages: [...closure].sort() }
}

/** The nextest filterset selecting the changed packages' tests and their reverse dependents'. */
export function rustScopeFilter(changed: Iterable<string>): string {
  return [...changed].sort().map(name => `rdeps(=${name})`).join(' | ')
}

/** Human-readable lines naming what a scope runs and why. */
export function describeRustScope(scope: RustScope): string[] {
  if (scope.kind === 'none') return ['no Rust input changed']
  if (scope.kind === 'full') return [`every package: ${scope.reason}`]
  const dependents = scope.packages.filter(name => !scope.changed.has(name))
  return [
    `${scope.packages.length} package(s): ${scope.packages.join(', ')}`,
    ...[...scope.changed].map(([name, why]) => `  ${name}: ${why}`),
    ...dependents.length === 0 ? [] : [`  reverse dependents: ${dependents.join(', ')}`],
  ]
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error('cargo metadata: expected an object')
  return value as Record<string, unknown>
}

function text(value: unknown, field: string): string {
  if (typeof value !== 'string') throw new Error(`cargo metadata: ${field} is not a string`)
  return value
}

function list(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`cargo metadata: ${field} is not an array`)
  return value
}

/** Repository-relative POSIX form of an absolute path under the repository. */
function relativeToRoot(path: string, root: string): string {
  const normal = path.replaceAll('\\', '/')
  const prefix = `${root.replaceAll('\\', '/')}/`
  if (!normal.startsWith(prefix)) throw new Error(`cargo metadata: ${path} is outside ${root}`)
  return normal.slice(prefix.length)
}

/**
 * Workspace members from `cargo metadata --format-version 1 --no-deps` output.
 * Every member is a path dependency, so a dependency with a member's `path` is an intra-workspace edge.
 */
export function parseRustWorkspace(metadata: string, root: string = ROOT): RustPackage[] {
  const parsed = record(JSON.parse(metadata))
  const packages = list(parsed['packages'], 'packages').map(record)
  const dirOf = (manifest: unknown): string => relativeToRoot(text(manifest, 'manifest_path').replace(/[\\/]Cargo\.toml$/u, ''), root)
  const byDir = new Map(packages.map(entry => [dirOf(entry['manifest_path']), text(entry['name'], 'name')]))
  return packages.map((entry) => {
    const dependencies = new Set<string>()
    for (const dependency of list(entry['dependencies'], 'dependencies').map(record)) {
      const path = dependency['path']
      if (path === undefined || path === null) continue
      const member = byDir.get(relativeToRoot(text(path, 'path'), root))
      if (member !== undefined) dependencies.add(member)
    }
    return { name: text(entry['name'], 'name'), dir: dirOf(entry['manifest_path']), dependencies: [...dependencies].sort() }
  })
}

/** Read the workspace graph with the pinned toolchain; `--no-deps` lists only members and needs no network. */
export function loadRustWorkspace(root: string = ROOT): RustPackage[] {
  const result = Bun.spawnSync(['cargo', 'metadata', '--format-version', '1', '--no-deps', '--locked'], {
    cwd: resolve(root, 'rust'), stdout: 'pipe', stderr: 'pipe',
  })
  if (result.exitCode !== 0) throw new Error(`cargo metadata failed: ${result.stderr.toString().trim()}`)
  return parseRustWorkspace(result.stdout.toString(), root)
}

function git(args: readonly string[], root: string): string | undefined {
  const result = Bun.spawnSync(['git', '-c', 'core.quotepath=off', ...args], { cwd: root, stdout: 'pipe', stderr: 'pipe' })
  return result.exitCode === 0 ? result.stdout.toString() : undefined
}

function paths(output: string | undefined): string[] {
  return (output ?? '').split('\0').filter(Boolean)
}

// Both sides of a rename: moving a file out of a package changes that package too.
const DIFF = ['diff', '--name-only', '--no-renames', '-z'] as const
const UNTRACKED = ['ls-files', '--others', '--exclude-standard', '-z'] as const

/** Paths changed since the merge base of `base` and HEAD, including uncommitted and untracked ones. */
export function changesSinceMergeBase(base: string, root: string = ROOT): { mergeBase: string; files: string[] } | undefined {
  const mergeBase = git(['merge-base', base, 'HEAD'], root)?.trim()
  if (mergeBase === undefined || mergeBase === '') return undefined
  const diff = git([...DIFF, mergeBase, '--'], root)
  if (diff === undefined) return undefined
  return { mergeBase, files: [...new Set([...paths(diff), ...paths(git(UNTRACKED, root))])].sort() }
}

/** Uncommitted and untracked paths, or HEAD's own commit when the tree is clean. */
export function worktreeChanges(root: string = ROOT): { from: string; files: string[] } {
  const files = [...new Set([...paths(git([...DIFF, 'HEAD', '--'], root)), ...paths(git(UNTRACKED, root))])].sort()
  if (files.length > 0) return { from: 'uncommitted changes', files }
  return { from: 'HEAD~1..HEAD', files: paths(git([...DIFF, 'HEAD~1', 'HEAD', '--'], root)).sort() }
}

/** The base a scoped run measures from when none is given; the first that resolves wins. */
export const DEFAULT_BASES: readonly string[] = ['origin/develop', 'develop']
