/** Workspace discovery rejects dangling package edges and keeps compiler faces explicit. */
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

function fixture(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-workspace-'))
  roots.push(root)
  put(root, 'package.json', { name: 'bake', packageManager: 'bun@1.4.3', workspaces: ['packages/*', 'apps/tui/*'] })
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
