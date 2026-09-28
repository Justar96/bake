/** Generate the Node build's project list from Bake's installed workspace manifests. */
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { join, posix, resolve } from 'node:path'

const DEPENDENCY_FIELDS = ['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'] as const

/** Workspace manifest fields used for build discovery and dependency checks. */
interface Manifest {
  name: string
  workspaces?: string[]
  packageManager?: string
  catalog?: Record<string, string>
  catalogs?: Record<string, Record<string, string>>
  dependencies?: Record<string, string>
  devDependencies?: Record<string, string>
  peerDependencies?: Record<string, string>
  optionalDependencies?: Record<string, string>
}

/**
 * External dependencies that may keep a literal range in two or more workspace
 * manifests, each with the reason it cannot share the root catalog entry.
 */
const CATALOG_EXCEPTIONS: Readonly<Record<string, string>> = {}

/**
 * Resolve Node compiler projects, reject missing workspace dependencies, and
 * require one root catalog range for every shared external dependency.
 * @param root - workspace root containing package.json.
 * @param catalogExceptions - shared dependencies allowed to keep literal ranges, keyed to their reasons.
 * @returns deterministic solution config, excluding the separately checked TUI.
 */
export function workspaceConfig(root: string, catalogExceptions: Readonly<Record<string, string>> = CATALOG_EXCEPTIONS): string {
  const manifest = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as Manifest
  if (!/^bun@\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(manifest.packageManager ?? '')) {
    throw new Error('The workspace packageManager must pin an exact Bun version')
  }
  const competingFiles = ['bun.lockb', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'package-lock.json', 'npm-shrinkwrap.json', 'yarn.lock']
  for (const file of competingFiles) {
    if (existsSync(join(root, file))) throw new Error(`Bun owns the workspace; remove root ${file}`)
  }
  const packages = [...new Set((manifest.workspaces ?? []).flatMap(pattern =>
    [...new Bun.Glob(`${pattern}/package.json`).scanSync({ cwd: root })].map(path => path.replaceAll('\\', '/')),
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
      const local = posix.join(posix.dirname(pkg.path), file)
      if (existsSync(join(root, local))) throw new Error(`${local}: workspace packages must use the root bun.lock`)
    }
  }
  const manifests = [{ path: 'package.json', manifest }, ...packages]
  for (const pkg of manifests) {
    for (const field of DEPENDENCY_FIELDS) {
      for (const [name, version] of Object.entries(pkg.manifest[field] ?? {})) {
        if (version.startsWith('workspace:') && !names.has(name)) {
          throw new Error(`${pkg.path}: missing workspace dependency ${name}`)
        }
      }
    }
  }
  checkCatalog(manifest, manifests, new Set(names.keys()), catalogExceptions)
  const references = packages.flatMap(({ path }) => {
    const directory = posix.dirname(path)
    if (directory.startsWith('apps/tui/')) return []
    const host = posix.join(directory, 'tsconfig.host.json')
    const config = existsSync(join(root, host)) ? host : posix.join(directory, 'tsconfig.json')
    return existsSync(join(root, config)) ? [{ path: `./${config}` }] : []
  })
  return JSON.stringify({ extends: './tsconfig.base.json', files: [], references }, null, 2) + '\n'
}

/**
 * Require `catalog:` for external dependencies that two or more workspace manifests declare.
 * Vendor manifests are pinned upstream copies (vendor/README.md), and peer ranges state
 * compatibility rather than the installed version, so neither counts toward sharing.
 */
function checkCatalog(
  rootManifest: Manifest,
  manifests: readonly { path: string; manifest: Manifest }[],
  workspaceNames: ReadonlySet<string>,
  exceptions: Readonly<Record<string, string>>,
): void {
  // `catalog:` names the default catalog; `catalog:<name>` names `catalogs.<name>`.
  const catalogs = new Map<string, Readonly<Record<string, string>>>([
    ['', rootManifest.catalog ?? {}],
    ...Object.entries(rootManifest.catalogs ?? {}),
  ])
  const label = (catalog: string): string => catalog === '' ? 'catalog' : `catalogs.${catalog}`
  const used = new Set<string>()
  const shared = new Map<string, { manifests: Set<string>; literal: string[] }>()
  for (const { path, manifest } of manifests) {
    for (const field of DEPENDENCY_FIELDS) {
      for (const [name, version] of Object.entries(manifest[field] ?? {})) {
        if (version.startsWith('catalog:')) {
          const catalog = version.slice('catalog:'.length)
          if (catalogs.get(catalog)?.[name] === undefined) {
            throw new Error(`${path}: ${field}.${name} uses ${version}, but the root ${label(catalog)} has no ${name} entry`)
          }
          used.add(`${catalog}\0${name}`)
        }
        if (field === 'peerDependencies' || path.startsWith('vendor/') || workspaceNames.has(name)) continue
        const declarations = shared.get(name) ?? { manifests: new Set<string>(), literal: [] }
        declarations.manifests.add(path)
        if (!version.startsWith('catalog:')) declarations.literal.push(`${path} (${field})`)
        shared.set(name, declarations)
      }
    }
  }
  for (const [catalog, entries] of catalogs) {
    for (const name of Object.keys(entries).sort()) {
      if (!used.has(`${catalog}\0${name}`)) throw new Error(`package.json: ${label(catalog)}.${name} is unused; remove the entry`)
    }
  }
  for (const [name, reason] of Object.entries(exceptions).sort(([left], [right]) => left.localeCompare(right))) {
    if (reason.trim() === '') throw new Error(`Catalog exception ${name} needs a reason`)
    const declarations = shared.get(name)
    if (declarations === undefined || declarations.manifests.size < 2 || declarations.literal.length === 0) {
      throw new Error(`Catalog exception ${name} is stale: no two workspace manifests declare it with a literal range`)
    }
  }
  for (const [name, { manifests: declaring, literal }] of [...shared].sort(([left], [right]) => left.localeCompare(right))) {
    if (declaring.size < 2 || literal.length === 0 || Object.hasOwn(exceptions, name)) continue
    throw new Error(
      `${name} is declared by ${declaring.size} workspace manifests; set its range once in the root catalog `
      + `and use "catalog:" in ${literal.join(', ')}`,
    )
  }
}

if (import.meta.main) {
  const root = resolve(import.meta.dirname, '..')
  const output = join(root, 'tsconfig.host.json')
  const expected = workspaceConfig(root)
  if (process.argv.includes('--check')) {
    if (readFileSync(output, 'utf8') !== expected) throw new Error('Run bun run gen-workspace to update tsconfig.host.json')
    console.log('Workspace dependencies, catalog, and Node project list match')
  } else {
    writeFileSync(output, expected)
    console.log('Generated tsconfig.host.json')
  }
}
