/** Workspace discovery rejects dangling package edges, keeps compiler faces explicit, and shares external ranges via the catalog. */
import { afterEach, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { workspaceConfig } from './gen-workspace.ts'
import { removeMissingAliases } from './gen-tsconfig-paths.ts'

const roots: string[] = []
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

function fixture(rootFields: Record<string, unknown> = {}): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-workspace-'))
  roots.push(root)
  put(root, 'package.json', { name: 'bake', packageManager: 'bun@1.4.3', workspaces: ['packages/*', 'apps/tui/*'], ...rootFields })
  return root
}

function put(root: string, path: string, value: unknown): void {
  mkdirSync(dirname(join(root, path)), { recursive: true })
  writeFileSync(join(root, path), JSON.stringify(value))
}

test('discovers host leaf configs and excludes separately checked TUI projects', () => {
  const root = fixture()
  for (const name of ['b', 'a']) {
    put(root, `packages/${name}/package.json`, { name })
    put(root, `packages/${name}/tsconfig.json`, {})
  }
  put(root, 'packages/b/tsconfig.host.json', {})
  put(root, 'apps/tui/ui/package.json', { name: 'ui', dependencies: { a: 'workspace:*' } })
  put(root, 'apps/tui/ui/tsconfig.json', {})
  expect(JSON.parse(workspaceConfig(root))).toEqual({ extends: './tsconfig.base.json', files: [], references: [
    { path: './packages/a/tsconfig.json' },
    { path: './packages/b/tsconfig.host.json' },
  ] })
})

test.each(['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'])(
  'rejects a missing %s workspace package', (field) => {
    const root = fixture()
    put(root, 'packages/a/package.json', { name: 'a', [field]: { missing: 'workspace:*' } })
    expect(() => workspaceConfig(root)).toThrow('packages/a/package.json: missing workspace dependency missing')
  },
)

test('removes deleted source aliases while retaining live and wildcard paths', () => {
  const root = fixture()
  put(root, 'packages/a/src/index.ts', {})
  const live = '  "@deepseek-ai/a": ["./packages/a/src"],'
  const wildcard = '  "@deepseek-ai/a/*": ["./packages/a/src/*"],'
  const absent = '  "@deepseek-ai/gone": ["./packages/gone/src"],'
  expect(removeMissingAliases([live, absent, wildcard].join('\n'), root)).toBe([live, wildcard].join('\n'))
})

test.each(['bun.lockb', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'package-lock.json', 'npm-shrinkwrap.json', 'yarn.lock'])(
  'rejects a competing root workspace file: %s', (file) => {
    const root = fixture()
    put(root, file, {})
    expect(() => workspaceConfig(root)).toThrow(`Bun owns the workspace; remove root ${file}`)
  },
)

test.each(['pnpm@11', 'bun@latest', 'bun@^1.4.3', 'bun@1.4', 'bun@'])(
  'rejects an unpinned or non-Bun package manager: %s', (packageManager) => {
    const root = fixture()
    put(root, 'package.json', { name: 'bake', packageManager, workspaces: ['packages/*'] })
    expect(() => workspaceConfig(root)).toThrow('The workspace packageManager must pin an exact Bun version')
  },
)

test.each(['bun.lock', 'bun.lockb', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'package-lock.json', 'npm-shrinkwrap.json', 'yarn.lock'])(
  'rejects a competing package-local installation: %s', (file) => {
    const root = fixture()
    put(root, 'packages/a/package.json', { name: 'a' })
    put(root, `packages/a/${file}`, {})
    expect(() => workspaceConfig(root)).toThrow(`packages/a/${file}: workspace packages must use the root bun.lock`)
  },
)

test('rejects duplicate package names instead of selecting an arbitrary dependency', () => {
  const root = fixture()
  for (const directory of ['a', 'b']) put(root, `packages/${directory}/package.json`, { name: 'shared' })
  expect(() => workspaceConfig(root)).toThrow('Duplicate workspace package shared: packages/a/package.json and packages/b/package.json')
})

test('rejects unnamed workspace packages', () => {
  const root = fixture()
  put(root, 'packages/a/package.json', {})
  expect(() => workspaceConfig(root)).toThrow('packages/a/package.json: missing workspace package name')
})

test('discovers each manifest once when workspace globs overlap', () => {
  const root = fixture()
  put(root, 'package.json', { name: 'bake', packageManager: 'bun@1.4.3', workspaces: ['packages/*', 'packages/a'] })
  put(root, 'packages/a/package.json', { name: 'a' })
  put(root, 'packages/a/tsconfig.json', {})
  expect(JSON.parse(workspaceConfig(root)).references).toEqual([{ path: './packages/a/tsconfig.json' }])
})

test('rejects an empty workspace', () => {
  expect(() => workspaceConfig(fixture())).toThrow('No workspace packages found')
})

test('requires the root catalog for an external dependency two manifests declare', () => {
  const root = fixture()
  put(root, 'packages/a/package.json', { name: 'a', dependencies: { zod: '^4.4.3' } })
  put(root, 'packages/b/package.json', { name: 'b', devDependencies: { zod: '^4.4.3' } })
  expect(() => workspaceConfig(root)).toThrow('zod is declared by 2 workspace manifests; set its range once in the root catalog and use '
    + '"catalog:" in packages/a/package.json (dependencies), packages/b/package.json (devDependencies)')
})

test('counts the root manifest and names only the literal declarations', () => {
  const root = fixture({ catalog: { zod: '^4.4.3' }, devDependencies: { zod: '^4.4.3' } })
  put(root, 'packages/a/package.json', { name: 'a', dependencies: { zod: 'catalog:' } })
  expect(() => workspaceConfig(root)).toThrow('zod is declared by 2 workspace manifests; set its range once in the root catalog and use '
    + '"catalog:" in package.json (devDependencies)')
})

test('keeps a catalog entry while one reference remains and rejects it once the last is removed', () => {
  const root = fixture({ catalog: { zod: '^4.4.3' } })
  put(root, 'packages/a/package.json', { name: 'a', dependencies: { zod: 'catalog:' } })
  put(root, 'packages/b/package.json', { name: 'b', devDependencies: { zod: 'catalog:' } })
  expect(() => workspaceConfig(root)).not.toThrow()
  put(root, 'packages/b/package.json', { name: 'b' })
  expect(() => workspaceConfig(root)).not.toThrow()
  put(root, 'packages/a/package.json', { name: 'a' })
  expect(() => workspaceConfig(root)).toThrow('package.json: catalog.zod is unused; remove the entry')
})

test.each(['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'])(
  'rejects a catalog reference without an entry in %s', (field) => {
    const root = fixture({ catalog: { zod: '^4.4.3' } })
    put(root, 'packages/a/package.json', { name: 'a', dependencies: { zod: 'catalog:' }, [field]: { yaml: 'catalog:' } })
    expect(() => workspaceConfig(root))
      .toThrow(`packages/a/package.json: ${field}.yaml uses catalog:, but the root catalog has no yaml entry`)
  },
)

test('resolves named catalogs and rejects their missing and unused entries', () => {
  const root = fixture({ catalogs: { legacy: { chokidar: '^4.0.3' } } })
  put(root, 'packages/a/package.json', { name: 'a', dependencies: { chokidar: 'catalog:legacy' } })
  put(root, 'packages/b/package.json', { name: 'b', dependencies: { chokidar: 'catalog:legacy' } })
  expect(() => workspaceConfig(root)).not.toThrow()
  put(root, 'packages/b/package.json', { name: 'b', dependencies: { chokidar: 'catalog:next' } })
  expect(() => workspaceConfig(root))
    .toThrow('packages/b/package.json: dependencies.chokidar uses catalog:next, but the root catalogs.next has no chokidar entry')
  put(root, 'packages/a/package.json', { name: 'a' })
  put(root, 'packages/b/package.json', { name: 'b' })
  expect(() => workspaceConfig(root)).toThrow('package.json: catalogs.legacy.chokidar is unused; remove the entry')
})

test('leaves vendor manifests, peer ranges, and workspace packages outside the sharing rule', () => {
  const root = fixture({ workspaces: ['vendor/*', 'packages/*'] })
  put(root, 'vendor/upstream/package.json', { name: 'upstream', dependencies: { chokidar: '^4.0.3' } })
  const peers = { peerDependencies: { react: '>=19' } }
  put(root, 'packages/a/package.json', { name: 'a', dependencies: { chokidar: '^4.0.3', c: 'workspace:^' }, ...peers })
  put(root, 'packages/b/package.json', { name: 'b', dependencies: { c: 'workspace:^' }, ...peers })
  put(root, 'packages/c/package.json', { name: 'c' })
  expect(() => workspaceConfig(root)).not.toThrow()
})

test('accepts an allowlisted literal range only while it has a reason and is still shared', () => {
  const root = fixture()
  put(root, 'packages/a/package.json', { name: 'a', devDependencies: { '@types/node': '^22.20.0' } })
  put(root, 'packages/b/package.json', { name: 'b', devDependencies: { '@types/node': '^26.0.1' } })
  expect(() => workspaceConfig(root, { '@types/node': 'b targets a newer Node' })).not.toThrow()
  expect(() => workspaceConfig(root, { '@types/node': ' ' })).toThrow('Catalog exception @types/node needs a reason')
  put(root, 'packages/b/package.json', { name: 'b' })
  expect(() => workspaceConfig(root, { '@types/node': 'b targets a newer Node' }))
    .toThrow('Catalog exception @types/node is stale: no two workspace manifests declare it with a literal range')
})
