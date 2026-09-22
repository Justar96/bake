/** Generate the Node build's project list from Bake's installed workspace manifests. */
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'

/** Workspace manifest fields used for build discovery and dependency checks. */
interface Manifest {
  name: string
  workspaces?: string[]
  packageManager?: string
  dependencies?: Record<string, string>
  devDependencies?: Record<string, string>
  peerDependencies?: Record<string, string>
  optionalDependencies?: Record<string, string>
}

/**
 * Resolve Node compiler projects and reject missing workspace dependencies.
 * @param root - workspace root containing package.json.
 * @returns deterministic solution config, excluding the separately checked TUI.
 */
export function workspaceConfig(root: string): string {
  const manifest = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as Manifest
  if (!/^bun@\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(manifest.packageManager ?? '')) {
    throw new Error('The workspace packageManager must pin an exact Bun version')
  }
  const competingFiles = ['bun.lockb', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'package-lock.json', 'npm-shrinkwrap.json', 'yarn.lock']
  for (const file of competingFiles) {
    if (existsSync(join(root, file))) throw new Error(`Bun owns the workspace; remove root ${file}`)
  }
  const packages = [...new Set((manifest.workspaces ?? []).flatMap(pattern =>
    [...new Bun.Glob(`${pattern}/package.json`).scanSync({ cwd: root })],
  ))].sort().map(path => ({
    path,
    manifest: JSON.parse(readFileSync(join(root, path), 'utf8')) as Manifest,
  }))
  if (packages.length === 0) throw new Error('No workspace packages found')
  const names = new Map<string, string>()
  for (const pkg of packages) {
    const name = pkg.manifest.name
    if (typeof name !== 'string' || name.length === 0) throw new Error(`${pkg.path}: missing workspace package name`)
    const previous = names.get(name)
    if (previous !== undefined) throw new Error(`Duplicate workspace package ${name}: ${previous} and ${pkg.path}`)
    names.set(name, pkg.path)
    for (const file of ['bun.lock', ...competingFiles]) {
      const local = join(dirname(pkg.path), file)
      if (existsSync(join(root, local))) throw new Error(`${local}: workspace packages must use the root bun.lock`)
    }
  }
  for (const pkg of [{ path: 'package.json', manifest }, ...packages]) {
    for (const field of ['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'] as const) {
      for (const [name, version] of Object.entries(pkg.manifest[field] ?? {})) {
        if (version.startsWith('workspace:') && !names.has(name)) {
          throw new Error(`${pkg.path}: missing workspace dependency ${name}`)
        }
      }
    }
  }
  const references = packages.flatMap(({ path }) => {
    const directory = dirname(path)
    if (directory.startsWith('apps/tui/')) return []
    const host = join(directory, 'tsconfig.host.json')
    const config = existsSync(join(root, host)) ? host : join(directory, 'tsconfig.json')
    return existsSync(join(root, config)) ? [{ path: `./${config}` }] : []
  })
  return JSON.stringify({ extends: './tsconfig.base.json', files: [], references }, null, 2) + '\n'
}

if (import.meta.main) {
  const root = resolve(import.meta.dirname, '..')
  const output = join(root, 'tsconfig.host.json')
  const expected = workspaceConfig(root)
  if (process.argv.includes('--check')) {
    if (readFileSync(output, 'utf8') !== expected) throw new Error('Run bun run gen-workspace to update tsconfig.host.json')
    console.log('Workspace dependencies and Node project list match')
  } else {
    writeFileSync(output, expected)
    console.log('Generated tsconfig.host.json')
  }
}
