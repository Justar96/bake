/**
 * Validate Cordis Loader entry metadata and package resolution.
 *
 * The Loader interpolates a plugin entry's `config` (after declared injections
 * activate, against that plugin context) and the entry `disabled` field (at
 * every mount decision, against the loader context). Every other entry
 * metadata field stays static, so an expression there remains truthy data and
 * silently changes composition. Shipped and test-only dsh overlays resolve
 * named plugins from the CLI application's owning manifest; package-owned
 * Loader fixtures resolve from their package manifest. A row an agent preset
 * shares with the host plane of a shipped profile that mounts presets fails
 * the check unless {@link SHARED_PLANE_ROWS} names it with its reason.
 */

import { globSync, readFileSync } from 'node:fs'
import { dirname, relative, resolve } from 'node:path'
import { Script } from 'node:vm'
import ts from 'typescript'
import type { DshBundleManifest } from '../packages/util/package-manifest/src/types.ts'
import { bundlePatchPaths, PROFILE_TEMPLATES, type ProfileTemplate } from '../packages/boot/app-boot/src/profile.ts'
import { cordisConfigFiles } from './cordis-config-files.ts'
import { isCordisGroupEntry, isJsExpr, loadCordisYaml } from './cordis-yaml.ts'

export interface PackageManifest {
  name?: string
  dependencies?: Record<string, string>
  devDependencies?: Record<string, string>
  optionalDependencies?: Record<string, string>
  dsh?: { bundle?: DshBundleManifest }
}

export interface PluginReference {
  file: string
  name: string
}

const root = resolve(import.meta.dirname, '..')
// These overlays are consumed by the built dsh app, so their bare specifiers
// resolve from apps/cli.
const appOverlayFiles = new Set([
  ...globSync('apps/cli/config/examples/**/*.yml', { cwd: root }),
])
const metadataFields = ['id', 'name', 'group', 'inject', 'intercept', 'isolate'] as const

/** The adaptive directory-picker chooser package (mounts a backend row at boot). */
const CHOOSER_PACKAGE = '@deepseek-ai/dsh-host-directory-picker-auto'

/**
 * The packages the chooser mounts by runtime string (mirror of its exported
 * `BACKEND_PACKAGES` and `SURFACE_PACKAGES`), invisible to yml-row scanning: a
 * composition mounting the chooser must resolve every one, or keyless Linux CI
 * (which only ever resolves `browse`) hides a dropped `-native` dependency
 * until a macOS boot.
 */
const CHOOSER_BACKEND_PACKAGES = [
  '@deepseek-ai/dsh-host-directory-picker-native',
  '@deepseek-ai/dsh-host-directory-picker-browse',
  '@deepseek-ai/dsh-client-ui-directory-picker-browse',
  '@deepseek-ai/dsh-client-ui-directory-picker-native',
]
/** The package whose row mounts the agent preset roster. */
const PRESETS_PACKAGE = '@deepseek-ai/dsh-agent-presets'

/**
 * Rows a preset-hosting profile deliberately runs on its host plane while
 * presets mount them too, each with the reason the second copy is harmless.
 * Every other shared row fails the plane check, and an entry that no longer
 * matches a shared row fails it too, so the list cannot outlive its reason.
 */
export const SHARED_PLANE_ROWS: Readonly<Record<string, string>> = {
  'compaction-basic': 'the tui patch sets `auto: false`, which installs no listener, so only a preset\'s own engine '
    + 'compacts on its own; the host engine is kept for the host `/compact`',
  'command-compact': 'the host `/compact` serves a preset without its own, such as `minimal`; a preset\'s own command '
    + 'shadows it by name',
  'tool-result-pruner': 'only the host engine calls the host pruner; a preset\'s engine resolves the pruner in its own '
    + 'realm, so no result is pruned twice',
}
const errors: string[] = []
const pluginReferences: PluginReference[] = []

if (import.meta.main) {
  const files = cordisConfigFiles(root)

  for (const file of files) {
    const document = loadCordisYaml(readFileSync(resolve(root, file), 'utf8'))
    if (!isUnknownArray(document)) {
      errors.push(`${file}: root must be a Loader entry array`)
      continue
    }
    for (let index = 0; index < document.length; index++) {
      validateEntry(document[index], file, `[${index}]`)
    }
  }

  errors.push(...validateAppResolution())
  errors.push(...validatePackageTestResolution())
  errors.push(...packageTestFixtureDependencyErrors())
  errors.push(...validateSourcePlaneResolution())
  errors.push(...validateClientHalvesDeclared())
  try {
    errors.push(...planeSeparationErrors(presetPlaneOverlaps()))
  } catch (error) {
    errors.push(error instanceof Error ? error.message : String(error))
  }

  if (errors.length > 0) {
    console.error('verify-cordis-config: invalid Loader metadata or plugin package resolution:')
    for (const error of errors) console.error(`- ${error}`)
    process.exitCode = 1
  } else {
    console.log(`verify-cordis-config: ${files.length} config files passed.`)
  }
}

/**
 * A browser plugin must declare the browser half it ships.
 *
 * The browser roster is discovered by scanning composed packages for a
 * `dsh.client` block, and the node half of a surface plugin is an empty
 * `apply`. A `packages/client` package that exports `./client` without that
 * block therefore composes, activates, and contributes nothing — its bundle is
 * never served and no error is raised anywhere. The mismatch is invisible in
 * the composition file, so it is checked against the manifests instead. Only
 * this group is checked: a Host package's `./client` export is the typed wire
 * face its browser consumers import, not a plugin the roster serves.
 * @returns one violation per client package whose `./client` export and
 * `dsh.client` declaration disagree.
 */
function validateClientHalvesDeclared(): string[] {
  return globSync('packages/client/*/package.json', { cwd: root }).flatMap((manifestPath) => {
    const manifest = readManifest(manifestPath) as PackageManifest & {
      exports?: Record<string, unknown>
      dsh?: { client?: unknown }
    }
    const shipsClient = manifest.exports !== undefined && Object.hasOwn(manifest.exports, './client')
    const declaresClient = manifest.dsh?.client !== undefined
    if (shipsClient === declaresClient) return []
    return [shipsClient
      ? `${manifestPath}: exports "./client" but declares no dsh.client, so its browser half is never served`
      : `${manifestPath}: declares dsh.client but exports no "./client" entry to serve`]
  })
}

/**
 * No shipped agent preset may repeat a row the host composition still runs.
 *
 * A preset contributes what ONE session adds to the host's registries. A row
 * active on both planes is therefore mounted twice — once per process and once
 * per session — and what that costs depends on what the row does: a provider
 * behind an `isolate` realm shadows the host's for its own consumers, so a host
 * contributor to that service reaches nobody; a row that registers into a host
 * singleton registers once per live session, so the second one collides.
 *
 * Both failure modes have occurred. A preset-local provider once shadowed the
 * host route that its consumer needed, and a host-registry contribution once
 * registered again for every live session until the second registration threw.
 * Neither changes a tool catalog, so no catalog assertion can see them — and the
 * shipped presets are near-copies of each other, so a fix applied to three of
 * four is the normal failure. A host row's untagged listeners also reach every
 * preset agent: the terminal profile's host `tool-skill` once stripped its
 * presets' skill catalogs and injected each `/skill` body twice.
 *
 * A preset's row counts whether or not the preset disables it. A preset that
 * turns a row off still receives the host's copy, as `ptc` once received the
 * host `workflow` tool its own disabled rows leave out.
 *
 * The host compositions are the shipped profiles that mount the preset roster.
 * @param repoRoot Repository root to scan.
 * @param templates Profile templates by name, as `dsh --profile <name>` initializes them.
 * @returns each preset's rows that a preset-hosting profile also runs, one entry per profile and preset with any.
 * @throws when no shipped profile mounts the roster, or a profile names a bundle no workspace manifest declares.
 */
export function presetPlaneOverlaps(
  repoRoot: string = root,
  templates: Readonly<Record<string, ProfileTemplate>> = PROFILE_TEMPLATES,
): PlaneOverlap[] {
  const hosts = presetHostProfiles(repoRoot, templates)
  if (hosts.length === 0) throw new Error(`no shipped profile mounts ${PRESETS_PACKAGE}, so the preset plane check has nothing to compare`)
  const presets = globSync('packages/preset/agent-presets/presets/*/agent.cordis.yml', { cwd: repoRoot })
    .map(file => file.replaceAll('\\', '/')).sort()
  return hosts.flatMap((host) => {
    const active = activeRowIds(host.files, repoRoot)
    return presets.flatMap((file) => {
      const ids = [...rowIds(file, repoRoot)].filter(id => active.has(id))
      return ids.length === 0 ? [] : [{ profile: host.profile, file, ids }]
    })
  })
}

/**
 * Turn plane overlaps into failures, except the rows an allowlist shares on
 * purpose, and fail an allowlist entry that no overlap uses any more.
 * @param overlaps Every preset row a preset-hosting profile also runs, from {@link presetPlaneOverlaps}.
 * @param shared Row ids both planes may run, each with the reason that is harmless.
 * @returns one diagnostic per preset and profile with unshared rows, and one per stale allowlist entry.
 */
export function planeSeparationErrors(
  overlaps: readonly PlaneOverlap[],
  shared: Readonly<Record<string, string>> = SHARED_PLANE_ROWS,
): string[] {
  const used = new Set(overlaps.flatMap(overlap => overlap.ids))
  return [
    ...overlaps.flatMap((overlap) => {
      const ids = overlap.ids.filter(id => !Object.hasOwn(shared, id))
      return ids.length === 0 ? [] : [
        `${overlap.file}: ${ids.map(id => `"${id}"`).join(', ')} also active in the ${overlap.profile} profile's host `
        + 'composition; a row belongs to exactly one plane, so disable the host copy or list it in SHARED_PLANE_ROWS '
        + 'with the reason both copies are harmless',
      ]
    }),
    ...Object.keys(shared).filter(id => !used.has(id)).map(id =>
      `SHARED_PLANE_ROWS lists "${id}", which no preset shares with a preset-hosting profile any more; remove it`),
  ]
}

/** One preset's rows that a preset-hosting profile also runs on its host plane. */
export interface PlaneOverlap {
  /** The shipped profile whose composition runs the rows. */
  readonly profile: string
  /** Repository-relative preset composition file. */
  readonly file: string
  /** The shared row ids, in the preset's declaration order. */
  readonly ids: readonly string[]
}

/**
 * Profile templates whose bundle layers mount the preset roster, each with its
 * layers' patch files in application order over the empty root.
 * @param repoRoot Repository root to scan.
 * @param templates Profile templates by name.
 * @returns one entry per preset-hosting profile.
 */
function presetHostProfiles(
  repoRoot: string,
  templates: Readonly<Record<string, ProfileTemplate>>,
): { profile: string; files: string[] }[] {
  const bundles = new Map(bundleManifestPaths(repoRoot).flatMap((manifestPath) => {
    const manifest = readManifest(manifestPath, repoRoot)
    const bundle = manifest.dsh?.bundle
    return manifest.name === undefined || bundle === undefined ? [] : [[manifest.name, { manifestPath, bundle }] as const]
  }))
  return Object.entries(templates).flatMap(([profile, template]) => {
    const files = template.bundles.flatMap((name) => {
      const found = bundles.get(name)
      if (found === undefined) throw new Error(`profile template "${profile}" names bundle ${name}, which no workspace manifest declares`)
      return bundlePatchPaths(resolve(repoRoot, dirname(found.manifestPath)), found.bundle)
        .map(file => relative(repoRoot, file).replaceAll('\\', '/'))
    })
    const mountsPresets = files.some(file => rowNames(file, repoRoot).has(PRESETS_PACKAGE))
    return mountsPresets ? [{ profile, files }] : []
  })
}

/**
 * Row ids a stack of patch layers leaves active. Each layer declares rows,
 * active unless declared `disabled: true`, then its top-level patches take an
 * id out with `disabled: true` or put a declared one back with `disabled: false`.
 * A `!!js` gate counts as active, since some host evaluates it that way.
 * @param files Repository-relative patch files in application order.
 * @param repoRoot Repository root the paths are relative to.
 * @returns the ids the composed tree mounts.
 */
function activeRowIds(files: readonly string[], repoRoot: string): Set<string> {
  const declared = new Set<string>()
  const active = new Set<string>()
  for (const file of files) {
    for (const row of rows(file, repoRoot)) {
      declared.add(row.id)
      if (row.disabled) active.delete(row.id)
      else active.add(row.id)
    }
    for (const entry of loadEntries(file, repoRoot)) {
      if (!isRecord(entry) || typeof entry.id !== 'string' || typeof entry.name === 'string') continue
      if (entry.disabled === true) active.delete(entry.id)
      else if (entry.disabled === false && declared.has(entry.id)) active.add(entry.id)
    }
  }
  return active
}

/** Every entry of one config file, or an empty list when it is not an entry array. */
function loadEntries(file: string, repoRoot: string = root): unknown[] {
  const document = loadCordisYaml(readFileSync(resolve(repoRoot, file), 'utf8'))
  return isUnknownArray(document) ? document : []
}

/**
 * Rows declared anywhere in one config file, including inside group `config`
 * lists — a preset nests most of its rows in `isolate` groups.
 * @param file - repository-relative config path.
 * @param repoRoot - repository root the path is relative to.
 * @returns each declared row's id, plugin name, and whether it is declared with a literal `disabled: true`.
 */
function rows(file: string, repoRoot: string): { id: string; name: string; disabled: boolean }[] {
  const found: { id: string; name: string; disabled: boolean }[] = []
  const walk = (value: unknown): void => {
    if (isUnknownArray(value)) {
      for (const item of value) walk(item)
      return
    }
    if (!isRecord(value)) return
    if (typeof value.id === 'string' && typeof value.name === 'string') found.push({ id: value.id, name: value.name, disabled: value.disabled === true })
    for (const child of Object.values(value)) walk(child)
  }
  walk(loadEntries(file, repoRoot))
  return found
}

/** Ids of every row one config file declares, disabled or not. */
function rowIds(file: string, repoRoot: string): Set<string> {
  return new Set(rows(file, repoRoot).map(row => row.id))
}

/** Plugin names of every row one config file declares. */
function rowNames(file: string, repoRoot: string): Set<string> {
  return new Set(rows(file, repoRoot).map(row => row.name))
}

function validateEntry(value: unknown, file: string, path: string): void {
  if (!isRecord(value)) {
    errors.push(`${file}${path}: entry must be an object`)
    return
  }
  recordPlugin(value, file)
  validateMetadata(value, file, path)
  if (isCordisGroupEntry(value)) {
    for (let index = 0; index < value.config.length; index++) {
      validateEntry(value.config[index], file, `${path}.config[${index}]`)
    }
  }
  if (isUnknownArray(value.insert)) {
    for (let index = 0; index < value.insert.length; index++) {
      validateEntry(value.insert[index], file, `${path}.insert[${index}]`)
    }
  }
  if (value.name !== '@deepseek-ai/cordis-plugin-include') return
  const config = value.config
  if (!isRecord(config) || !isUnknownArray(config.patches)) return
  for (let index = 0; index < config.patches.length; index++) {
    const patch = config.patches[index]
    const patchPath = `${path}.config.patches[${index}]`
    if (!isRecord(patch)) continue
    recordPlugin(patch, file)
    validateMetadata(patch, file, patchPath)
    if (!isUnknownArray(patch.insert)) continue
    for (let insertIndex = 0; insertIndex < patch.insert.length; insertIndex++) {
      validateEntry(patch.insert[insertIndex], file, `${patchPath}.insert[${insertIndex}]`)
    }
  }
}

function recordPlugin(entry: Record<string, unknown>, file: string): void {
  if (typeof entry.name === 'string') pluginReferences.push({ file, name: entry.name })
}

function validateAppResolution(): string[] {
  const violations: string[] = []
  const bundleManifests = bundleManifestPaths()
  // App overlays (and any config left under apps/cli/config) resolve from the
  // dsh app's own dependency surface — the profile module fallback mirrors it.
  const appManifest = readManifest('apps/cli/package.json')
  const appDependencies = {
    ...appManifest.dependencies,
    // The fallback also links every in-box bundle's own dependencies
    // (healProfilesModuleFallback). Optional Profile bundles stay outside the
    // app installation until that Profile installs them.
    ...Object.fromEntries(globSync('packages/bundle/*/package.json', { cwd: root })
      .flatMap(file => Object.entries(readManifest(file).dependencies ?? {}))),
  }
  const shipped = new Set(globSync('*.cordis.yml', { cwd: resolve(root, 'apps/cli/config') })
    .map(file => `apps/cli/config/${file}`))
  const appReferences = pluginReferences.filter(reference => shipped.has(reference.file) || appOverlayFiles.has(reference.file))
  violations.push(...missingPluginDependencies(
    appReferences,
    appDependencies,
    'apps/cli/package.json dependencies or a bundle manifest',
  ))
  const appTestReferences = pluginReferences.filter(reference => reference.file.startsWith('apps/cli/tests/'))
  violations.push(...missingPluginDependencies(
    appTestReferences,
    { ...appManifest.dependencies, ...appManifest.devDependencies },
    'apps/cli/package.json dependencies or devDependencies',
  ))
  // Each bundle's patch rows must resolve from that bundle's own dependencies:
  // per-layer resolution anchors on the bundle package directory.
  for (const manifestPath of bundleManifests) {
    const bundleDir = manifestPath.replace(/\/package\.json$/, '')
    const manifest = readManifest(manifestPath)
    const bundle = manifest.dsh?.bundle
    if (bundle === undefined) continue
    const patchFiles = new Set(bundlePatchPaths(resolve(root, bundleDir), bundle)
      .map(file => relative(root, file).replaceAll('\\', '/')))
    const references = pluginReferences.filter(reference => patchFiles.has(reference.file))
    violations.push(...bundlePluginDependencyErrors(manifestPath, manifest, references))
  }
  return violations
}

/**
 * Package-owned Loader fixtures resolve named plugins from their package's
 * dependency surface, not from a repository-level test umbrella.
 * @returns one violation per configured package absent from the owner manifest.
 */
function validatePackageTestResolution(): string[] {
  const referencesByManifest = new Map<string, PluginReference[]>()
  for (const reference of pluginReferences) {
    const manifestPath = packageTestManifestPath(reference.file)
    if (manifestPath === undefined) continue
    const references = referencesByManifest.get(manifestPath) ?? []
    references.push(reference)
    referencesByManifest.set(manifestPath, references)
  }
  return [...referencesByManifest].flatMap(([manifestPath, references]) =>
    packageTestPluginDependencyErrors(manifestPath, readManifest(manifestPath), references))
}

/**
 * Validate the named plugins one package-owned Loader fixture resolves.
 * Self-references use Node package self-resolution; every other package must
 * be an ordinary production or test dependency of the owner.
 * @param manifestPath Repository-relative owner manifest path.
 * @param manifest Parsed owner manifest.
 * @param references Named plugin references from owner-local test configs.
 * @returns Missing dependency diagnostics.
 */
export function packageTestPluginDependencyErrors(
  manifestPath: string,
  manifest: PackageManifest,
  references: readonly PluginReference[],
): string[] {
  return missingPluginDependencies(
    references.filter(reference => packageNameFromSpecifier(reference.name) !== manifest.name),
    { ...manifest.dependencies, ...manifest.devDependencies },
    `${manifestPath} dependencies or devDependencies`,
  )
}

/**
 * Validate imports made by fixture modules adjacent to package-owned Loader
 * configs. These files execute as plain Node/tsx children, so a stale root
 * `node_modules` link must not hide an undeclared dependency.
 * @param repoRoot Repository root to scan.
 * @returns Missing dependency diagnostics.
 */
export function packageTestFixtureDependencyErrors(repoRoot: string = root): string[] {
  const fixtureDirectories = new Set(cordisConfigFiles(repoRoot)
    .filter(file => packageTestManifestPath(file) !== undefined)
    .map(file => dirname(file).replaceAll('\\', '/')))
  if (fixtureDirectories.size === 0) {
    return ['package test fixture dependency scan found no package-owned Loader configs']
  }
  const referencesByManifest = new Map<string, PluginReference[]>()
  let fixtureModuleCount = 0
  for (const fixtureDirectory of fixtureDirectories) {
    const files = globSync([
      `${fixtureDirectory}/**/*.ts`,
      `${fixtureDirectory}/**/*.mjs`,
    ], { cwd: repoRoot })
    fixtureModuleCount += files.length
    for (const file of files) {
      const manifestPath = packageTestManifestPath(file)
      if (manifestPath === undefined) continue
      const references = referencesByManifest.get(manifestPath) ?? []
      const source = readFileSync(resolve(repoRoot, file), 'utf8')
      for (const imported of ts.preProcessFile(source, true, true).importedFiles) {
        references.push({ file: file.replaceAll('\\', '/'), name: imported.fileName })
      }
      referencesByManifest.set(manifestPath, references)
    }
  }
  if (fixtureModuleCount === 0) {
    return ['package test fixture dependency scan found no fixture modules beside Loader configs']
  }
  return [...referencesByManifest].flatMap(([manifestPath, references]) =>
    packageTestPluginDependencyErrors(
      manifestPath,
      readManifest(manifestPath, repoRoot),
      references,
    ))
}

/** Owner manifest for a package-local test path. */
function packageTestManifestPath(file: string): string | undefined {
  const match = /^(packages\/[^/]+\/[^/]+)\/tests(?:\/|$)/.exec(file.replaceAll('\\', '/'))
  return match?.[1] === undefined ? undefined : `${match[1]}/package.json`
}

/**
 * Discover workspace Bundle packages from their manifest declaration: the
 * shared runtime's, and the terminal app's, which the `tui` profile layers
 * over `dsh-base`.
 * @param repoRoot Repository root to scan.
 * @returns Sorted slash-normalized repository-relative package manifest paths.
 */
export function bundleManifestPaths(repoRoot: string = root): string[] {
  return globSync(['packages/*/*/package.json', 'apps/tui/packages/*/package.json'], { cwd: repoRoot })
    .filter(path => readManifest(path, repoRoot).dsh?.bundle?.patch !== undefined)
    .map(path => path.replaceAll('\\', '/'))
    .sort()
}

/**
 * Validate plugin packages referenced by one Bundle patch.
 * @param manifestPath Repository-relative Bundle manifest path.
 * @param manifest Parsed Bundle manifest.
 * @param references Plugin rows read from the Bundle package directory.
 * @returns Missing production dependency diagnostics.
 */
export function bundlePluginDependencyErrors(
  manifestPath: string,
  manifest: PackageManifest,
  references: readonly PluginReference[],
): string[] {
  return missingPluginDependencies(
    // A Bundle may mount its own package (for example, its provider or runtime row).
    references.filter(reference => packageNameFromSpecifier(reference.name) !== manifest.name),
    manifest.dependencies ?? {},
    `${manifestPath} dependencies`,
  )
}

/**
 * Every configured specifier of a local workspace package must resolve through
 * the tsconfig `paths` facade to a `.ts`/`.tsx` source file. The `dsh` source
 * launch (tsx) and vitest resolve in the source plane; without a `paths` match
 * they fall back to package `exports`, which reach built `lib/` — present on a
 * built dev tree, absent on a clean one — so a missing mapping boots locally
 * yet breaks every clean checkout. Anything but a `.ts`/`.tsx` hit (a `.d.ts`
 * or `.js` under built `lib/`) is that artifact-plane fallback, not source.
 */
function validateSourcePlaneResolution(): string[] {
  const violations: string[] = []
  const localPackages = localPackageDirectories()
  const config = ts.readConfigFile(resolve(root, 'tsconfig.base.json'), path => ts.sys.readFile(path))
  if (config.error !== undefined) {
    throw new Error(ts.flattenDiagnosticMessageText(config.error.messageText, '\n'))
  }
  const { options, errors: optionErrors } = ts.convertCompilerOptionsFromJson(
    (config.config as { compilerOptions?: unknown }).compilerOptions,
    root,
    'tsconfig.base.json',
  )
  if (optionErrors.length > 0) {
    throw new Error(optionErrors.map(error => ts.flattenDiagnosticMessageText(error.messageText, '\n')).join('\n'))
  }
  // convertCompilerOptionsFromJson leaves `pathsBasePath` unset, so relative
  // `paths` targets resolve against the host's current directory; anchor it to
  // the repository root to keep the gate cwd-independent.
  const host: ts.ModuleResolutionHost = {
    fileExists: path => ts.sys.fileExists(path),
    readFile: path => ts.sys.readFile(path),
    directoryExists: path => ts.sys.directoryExists(path),
    getCurrentDirectory: () => root,
  }
  const sourceExtensions = new Set<string>([ts.Extension.Ts, ts.Extension.Tsx])
  const containingFile = resolve(root, 'scripts/verify-cordis-config.ts')
  const locationsBySpecifier = new Map<string, Set<string>>()
  for (const reference of pluginReferences) {
    const packageName = packageNameFromSpecifier(reference.name)
    if (packageName === undefined || !localPackages.has(packageName)) continue
    const locations = locationsBySpecifier.get(reference.name) ?? new Set<string>()
    locations.add(reference.file)
    locationsBySpecifier.set(reference.name, locations)
  }
  for (const [specifier, locations] of locationsBySpecifier) {
    const resolved = ts.resolveModuleName(specifier, containingFile, options, host).resolvedModule
    if (resolved !== undefined && sourceExtensions.has(resolved.extension)) continue
    violations.push(`${[...locations].join(', ')}: ${specifier} does not resolve to workspace source through tsconfig.base.json paths (add a mapping so the tsx source launch does not depend on built lib/)`)
  }
  return violations
}

function missingPluginDependencies(
  references: readonly PluginReference[],
  dependencies: Readonly<Record<string, string>>,
  dependencyOwner: string,
): string[] {
  const requiredPackages = new Map<string, Set<string>>()
  const require = (packageName: string, file: string): void => {
    const locations = requiredPackages.get(packageName) ?? new Set<string>()
    locations.add(file)
    requiredPackages.set(packageName, locations)
  }
  for (const reference of references) {
    const packageName = packageNameFromSpecifier(reference.name)
    if (packageName === undefined) continue
    require(packageName, reference.file)
    if (packageName === CHOOSER_PACKAGE) {
      for (const backend of CHOOSER_BACKEND_PACKAGES) require(backend, reference.file)
    }
  }
  return [...requiredPackages].flatMap(([packageName, locations]) => packageName in dependencies
    ? []
    : `${[...locations].join(', ')}: ${packageName} must be declared in ${dependencyOwner}`)
}

function readManifest(path: string, repoRoot: string = root): PackageManifest {
  return JSON.parse(readFileSync(resolve(repoRoot, path), 'utf8')) as PackageManifest
}

function localPackageDirectories(): Map<string, string> {
  const manifests = globSync(['packages/*/*/package.json', 'vendor/*/package.json'], { cwd: root })
  const packages = new Map<string, string>()
  for (const manifestPath of manifests) {
    const manifest = readManifest(manifestPath)
    if (manifest.name !== undefined) packages.set(manifest.name, resolve(root, dirname(manifestPath)))
  }
  return packages
}

function packageNameFromSpecifier(specifier: string): string | undefined {
  if (specifier.startsWith('.') || specifier.startsWith('/') || /^[a-z][a-z+.-]*:/i.test(specifier)) return undefined
  const segments = specifier.split('/')
  if (specifier.startsWith('@')) {
    return segments.length >= 2 ? `${segments[0]}/${segments[1]}` : undefined
  }
  return segments[0] || undefined
}

function validateMetadata(entry: Record<string, unknown>, file: string, path: string): void {
  for (const problem of metadataExpressionErrors(entry, path)) {
    errors.push(`${file}${problem}`)
  }
}

/**
 * Expression-node diagnostics for one entry. `disabled` is the single
 * interpolated metadata field: its own `!!js` expression node is allowed and
 * must parse, while expressions nested below it stay truthy data; every other
 * metadata field must stay fully static.
 * @param entry - one loader entry (or patch row).
 * @param path - the entry's diagnostic path prefix.
 * @returns one diagnostic per offending expression.
 */
export function metadataExpressionErrors(entry: Record<string, unknown>, path: string): string[] {
  const problems: string[] = []
  for (const field of metadataFields) {
    if (!(field in entry)) continue
    const expressionPaths: string[] = []
    collectExpressionPaths(entry[field], `${path}.${field}`, expressionPaths)
    for (const expressionPath of expressionPaths) problems.push(`${expressionPath}: !!js is not interpolated here`)
  }
  const disabled = entry.disabled
  if (disabled !== undefined) {
    if (isJsExpr(disabled)) {
      const detail = disabledExpressionProblem(disabled.__jsExpr)
      if (detail !== undefined) problems.push(`${path}.disabled${detail}`)
    } else {
      // A non-expression value gates on Boolean() at mount; an expression
      // nested anywhere below it never evaluates, so it must stay literal.
      const expressionPaths: string[] = []
      collectExpressionPaths(disabled, `${path}.disabled`, expressionPaths)
      for (const expressionPath of expressionPaths) problems.push(`${expressionPath}: !!js is not interpolated here`)
    }
  }
  return problems
}

/**
 * Parse-only validation of a `disabled` expression: the Loader evaluates it
 * at every mount decision, and a syntax error would fail the boot — rejecting
 * it here moves that failure to the earliest resolvable point.
 * @param expression - the `!!js` expression text.
 * @returns the diagnostic suffix, or `undefined` when the expression parses.
 */
function disabledExpressionProblem(expression: string): string | undefined {
  try {
    // Compilation only — constructing a Script does not execute its source.
    new Script(`(${expression})`)
    return undefined
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error)
    return `: disabled expression does not parse: ${detail}`
  }
}

function collectExpressionPaths(value: unknown, path: string, output: string[]): void {
  if (isJsExpr(value)) {
    output.push(path)
    return
  }
  if (isUnknownArray(value)) {
    for (let index = 0; index < value.length; index++) collectExpressionPaths(value[index], `${path}[${index}]`, output)
    return
  }
  if (!isRecord(value)) return
  for (const [key, child] of Object.entries(value)) collectExpressionPaths(child, `${path}.${key}`, output)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object'
}

function isUnknownArray(value: unknown): value is unknown[] {
  return Array.isArray(value)
}
