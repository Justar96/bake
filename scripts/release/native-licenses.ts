/**
 * Generate and check the third-party license bundle for the native agent,
 * `bake-rs`, one text file per release platform plus a machine-readable
 * inventory.
 *
 * Selection, from Cargo's own resolver: a platform's `runtime` crates are
 * those `cargo tree -p bake-cli --target <triple> -e normal,no-proc-macro`
 * lists, the crates linked into the binary, with their target `cfg()` and
 * features evaluated for that triple. Build scripts, proc macros, and their
 * dependencies run on the build host, and `cargo tree` evaluates their `cfg()`
 * for the host it runs on, not for the release runner that builds the triple
 * (a `cfg(windows)` dependency of a build dependency is missing from a
 * `--target x86_64-pc-windows-msvc` tree printed on Linux). So each platform
 * also lists, as `build`, every crate that
 * `cargo tree --target all -e normal,build --no-dedupe` shows below a
 * build-dependency or proc-macro edge of its linked crates, with no host
 * `cfg()` applied: the union over all build hosts, a superset of what its
 * runner builds.
 * Dev-dependencies are excluded. Bake's own workspace crates are listed with
 * their NOTICE files, not as third-party crates.
 *
 * Collection: cargo-about (exact version {@link CARGO_ABOUT_VERSION}, config
 * `rust/about.toml`) owns license policy: it parses each crate's SPDX
 * expression, chooses from the `accepted` list, fails a crate no accepted
 * license satisfies, and finds the license texts in the crate's source package.
 * This script adds no license policy. It checks provenance: the crate set must
 * equal the union of the per-target trees, every text must be read from a file
 * of the crate's own source package and appear in it verbatim (cargo-about's
 * generic substitute for a text it could not find fails), and each
 * clarification must name a crate locked at one version, keep its declared
 * license string, and hash its files. NOTICE, COPYRIGHT, and COPYING files at a
 * crate's root are reproduced verbatim as well.
 *
 * `Cargo.lock` is the authority: every Cargo call is `--locked`, and checking
 * is offline. `--write` first runs `cargo fetch --locked`, the only step that
 * may use the network, then writes the bundle to `.artifacts/native/licenses`
 * or `--out <dir>`. The default mode, `--check`, regenerates in memory and
 * compares, writing nothing.
 */

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'

/** The cargo-about release the bundle is generated with; any other version is refused. */
export const CARGO_ABOUT_VERSION = '0.9.2'
/** The inventory's schema id. */
export const SCHEMA = 'bake-native-licenses/1'
/** Each platform's license file, at `<out>/<platform>/`. */
export const BUNDLE_FILE = 'THIRD-PARTY-LICENSES.txt'
/** The machine-readable inventory, at `<out>/`. */
export const INVENTORY_FILE = 'inventory.json'
/** The package whose binary, `bake-rs`, is distributed. */
export const ROOT_PACKAGE = 'bake-cli'
/** Default output, relative to the repository root; ignored by Git. */
export const DEFAULT_OUT = '.artifacts/native/licenses'

/** Release platform ids and the Rust targets their release runners build natively. */
export const PLATFORMS = [
  { id: 'darwin-arm64', triple: 'aarch64-apple-darwin' },
  { id: 'darwin-x64', triple: 'x86_64-apple-darwin' },
  { id: 'linux-arm64', triple: 'aarch64-unknown-linux-gnu' },
  { id: 'linux-x64', triple: 'x86_64-unknown-linux-gnu' },
  { id: 'win32-x64', triple: 'x86_64-pc-windows-msvc' },
] as const
/** A release platform id. */
export type PlatformId = typeof PLATFORMS[number]['id']

const CRATES_IO = 'registry+https://github.com/rust-lang/crates.io-index'
const ATTRIBUTION_FILE = /^(?:notice|copyright|copying)(?:[._-][^/\\]*)?$/iu
const CONFIG_KEYS = new Set(['accepted', 'targets', 'ignore-dev-dependencies', 'ignore-build-dependencies', 'private'])
const RULE = '='.repeat(80)
const THIN_RULE = '-'.repeat(80)

/** An input or consistency failure; the message lists every problem found. */
export class LicenseBundleError extends Error {}

// ---------------------------------------------------------------- inputs

/** A crate as `cargo tree` prints it. */
export interface TreeEntry { readonly name: string; readonly version: string }

/** One platform's linked crates: `cargo tree --target <triple> -e normal,no-proc-macro --prefix none -f {p}`. */
export interface PlatformTrees {
  readonly id: PlatformId
  readonly linked: string
}

/** The `cargo metadata` fields the bundle reads. */
export interface CargoMetadata {
  readonly packages: readonly {
    readonly name: string
    readonly version: string
    readonly id: string
    readonly source: string | null
    readonly license: string | null
    readonly manifest_path: string
  }[]
  readonly workspace_members: readonly string[]
}

/** Everything generation reads, gathered by {@link collect}. */
export interface BundleInputs {
  /** Repository root, which every reported workspace path is relative to. */
  readonly root: string
  readonly cargoLock: Uint8Array
  readonly config: string
  readonly toolchain: { readonly channel: string; readonly cargo: string }
  readonly cargoAbout: string
  readonly metadata: CargoMetadata
  readonly trees: readonly PlatformTrees[]
  /** `cargo tree --target all -e normal,build --no-dedupe -f {p}`, with its edge-kind headings. */
  readonly hostTree: string
  /** cargo-about's `--format json` output, unparsed. */
  readonly about: string
}

/** Parse `cargo tree --prefix none -f {p}` output; duplicates and the `(*)` marks are dropped. */
export function parseTree(text: string): TreeEntry[] {
  const seen = new Map<string, TreeEntry>()
  for (const line of text.split(/\r?\n/u)) {
    if (line.trim() === '') continue
    const match = /^(\S+) v(\S+)(?: \([^()]*\))*$/u.exec(line.trim())
    if (match === null) throw new LicenseBundleError(`cargo tree printed an unexpected line: ${JSON.stringify(line)}`)
    const entry = { name: match[1] as string, version: match[2] as string }
    seen.set(`${entry.name}@${entry.version}`, entry)
  }
  return [...seen.values()]
}

/**
 * The crates one platform's build runs on the build host: those under a
 * `[build-dependencies]` heading or a proc macro, in an indented,
 * undeduplicated `cargo tree --target all` listing, below a chain of crates
 * all in `linked`, the platform's own linked set. A build host's `cfg()` is
 * not evaluated, so this is the union over every host that could build it.
 */
export function parseHostSide(text: string, linked: ReadonlySet<string>): TreeEntry[] {
  const seen = new Map<string, TreeEntry>()
  // By depth: whether the package there is built for the host, whether it is
  // part of this platform's build, and whether its children printed so far sit
  // under a `[build-dependencies]` heading.
  const host: boolean[] = []
  const active: boolean[] = []
  const build: boolean[] = []
  for (const line of text.split(/\r?\n/u)) {
    if (line.trim() === '') continue
    const heading = /^((?:│ {3}| {4})*)\[(dependencies|build-dependencies|dev-dependencies)\]$/u.exec(line)
    if (heading !== null) {
      const depth = (heading[1] as string).length / 4
      if (depth >= host.length) throw new LicenseBundleError(`cargo tree printed a heading with no package: ${JSON.stringify(line)}`)
      build[depth] = heading[2] === 'build-dependencies'
      continue
    }
    const match = /^((?:│ {3}| {4})*(?:├── |└── )?)(\S+) v(\S+)((?: \([^()]*\))*)$/u.exec(line)
    if (match === null) throw new LicenseBundleError(`cargo tree printed an unexpected line: ${JSON.stringify(line)}`)
    const depth = (match[1] as string).length / 4
    if (depth > host.length || (depth === 0 && host.length > 0)) throw new LicenseBundleError(`cargo tree printed an unexpected nesting: ${JSON.stringify(line)}`)
    const entry = { name: match[2] as string, version: match[3] as string }
    const key = `${entry.name}@${entry.version}`
    const isHost = depth > 0 && ((host[depth - 1] as boolean) || (build[depth - 1] as boolean) || (match[4] as string).includes('(proc-macro)'))
    const isActive = depth === 0 ? linked.has(key) : (active[depth - 1] as boolean) && (isHost || linked.has(key))
    host.length = depth
    active.length = depth
    build.length = depth
    host[depth] = isHost
    active[depth] = isActive
    build[depth] = false
    if (isHost && isActive) seen.set(key, entry)
  }
  return [...seen.values()]
}

// ---------------------------------------------------------------- generation

interface Package {
  readonly key: string
  readonly name: string
  readonly version: string
  readonly source: string | null
  readonly license: string | null
  readonly dir: string
  readonly workspace: boolean
}

interface Text {
  readonly id: string
  readonly license: string
  readonly sha256: string
  readonly body: string
  /** `<crate> <version>, <path inside its package>` of the file it was read from. */
  readonly source: string
  readonly users: Set<string>
}

interface Notice { readonly file: string; readonly sha256: string; readonly body: string }

interface ThirdParty {
  readonly pkg: Package
  readonly roles: Map<PlatformId, 'runtime' | 'build'>
  /** License text ids, each with the file of this crate that holds it. */
  readonly texts: Map<string, string>
  readonly notices: Notice[]
  clarified: boolean
}

/** The generated files, by path relative to the output directory, and a summary for reports. */
export interface Bundle {
  readonly files: Readonly<Record<string, string>>
  readonly summary: {
    readonly crates: number
    readonly texts: number
    readonly perPlatform: Readonly<Record<PlatformId, number>>
    readonly clarified: readonly string[]
    /** Locked crates cargo-about reported that no release platform compiles. */
    readonly excluded: readonly string[]
  }
}

function sha256(bytes: Uint8Array | string): string {
  return createHash('sha256').update(bytes).digest('hex')
}

function decode(bytes: Uint8Array, what: string): string {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
  } catch {
    throw new LicenseBundleError(`${what} is not UTF-8 text`)
  }
}

function inside(dir: string, path: string): boolean {
  const rel = relative(dir, path)
  return rel !== '' && !rel.startsWith('..') && !isAbsolute(rel)
}

function slash(path: string): string {
  return path.split(sep).join('/')
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new LicenseBundleError(`${what} is not a table`)
  return value as Record<string, unknown>
}

const SEARCH_LIMIT = 4 << 20

/**
 * The slash path, inside `dir`, of a file holding `text` verbatim: `preferred`
 * when it does, otherwise, unless `exact`, the first such file in sorted order.
 */
function findText(dir: string, preferred: string, text: string, exact: boolean): string | undefined {
  const holds = (path: string): boolean => {
    const stat = lstatSync(path, { throwIfNoEntry: false })
    if (stat === undefined || !stat.isFile() || stat.size > SEARCH_LIMIT) return false
    const body = readFileSync(path)
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(body).includes(text)
    } catch {
      return false
    }
  }
  const first = resolve(dir, preferred)
  if (!inside(dir, first)) throw new LicenseBundleError(`${preferred} is outside ${dir}`)
  if (text !== '' && holds(first)) return slash(relative(dir, first))
  if (exact || text === '') return undefined
  const walk = (at: string): string | undefined => {
    for (const entry of readdirSync(at, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name, 'en'))) {
      const path = join(at, entry.name)
      const hit = entry.isDirectory() ? walk(path) : entry.isFile() && holds(path) ? slash(relative(dir, path)) : undefined
      if (hit !== undefined) return hit
    }
    return undefined
  }
  return walk(dir)
}

interface Clarification { readonly license: string; readonly files: readonly { readonly path: string; readonly checksum: string }[] }

/** Read `about.toml`'s policy, refusing settings that would change the selection or allow licenses broadly. */
function readConfig(text: string): { clarify: Map<string, Clarification> } {
  let parsed: Record<string, unknown>
  try {
    parsed = record(Bun.TOML.parse(text), 'about.toml')
  } catch (error) {
    if (error instanceof LicenseBundleError) throw error
    throw new LicenseBundleError(`about.toml does not parse: ${(error as Error).message}`)
  }
  const problems: string[] = []
  const accepted = parsed.accepted
  if (!Array.isArray(accepted) || accepted.length === 0 || !accepted.every(item => typeof item === 'string')) problems.push('about.toml `accepted` must be a non-empty list of license ids')
  const targets = parsed.targets
  const triples = PLATFORMS.map(platform => platform.triple)
  if (!Array.isArray(targets) || targets.length !== triples.length || !triples.every((triple, index) => targets[index] === triple)) problems.push(`about.toml \`targets\` must be exactly ${JSON.stringify(triples)}`)
  if (parsed['ignore-dev-dependencies'] !== true) problems.push('about.toml must set `ignore-dev-dependencies = true`')
  if (parsed['ignore-build-dependencies'] !== false) problems.push('about.toml must set `ignore-build-dependencies = false`')
  if (record(parsed.private ?? {}, 'about.toml `private`').ignore !== true) problems.push('about.toml must set `private = { ignore = true }`')
  const clarify = new Map<string, Clarification>()
  for (const [key, value] of Object.entries(parsed)) {
    if (CONFIG_KEYS.has(key)) continue
    const table = typeof value === 'object' && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined
    const extra = table === undefined ? [key] : Object.keys(table).filter(field => field !== 'clarify')
    if (table === undefined || extra.length > 0 || table.clarify === undefined) {
      problems.push(`about.toml \`${key}\` is not a setting this bundle allows; only per-crate \`clarify\` tables are (no workarounds or per-crate acceptance)`)
      continue
    }
    const entry = record(table.clarify, `about.toml \`${key}.clarify\``)
    const files = entry.files
    if (typeof entry.license !== 'string' || !Array.isArray(files) || files.length === 0 || Object.keys(entry).some(field => field !== 'license' && field !== 'files')) {
      problems.push(`about.toml \`${key}.clarify\` must have a license and hashed files, and nothing else (no git sources)`)
      continue
    }
    const checked: { path: string; checksum: string }[] = []
    for (const file of files) {
      const item = record(file, `about.toml \`${key}.clarify.files\``)
      if (typeof item.path !== 'string' || typeof item.checksum !== 'string' || !/^[0-9a-f]{64}$/u.test(item.checksum) || 'start' in item || 'end' in item) {
        problems.push(`about.toml \`${key}.clarify.files\` entries need a path and a SHA-256 checksum, and take the whole file`)
        continue
      }
      checked.push({ path: item.path, checksum: item.checksum })
    }
    clarify.set(key, { license: entry.license, files: checked })
  }
  if (problems.length > 0) throw new LicenseBundleError(problems.join('\n'))
  return { clarify }
}

interface AboutLicense { readonly id: string; readonly text: string; readonly sourcePath: string | null; readonly users: string[] }

function readAbout(text: string): { crates: Set<string>; licenses: AboutLicense[] } {
  let parsed: Record<string, unknown>
  try {
    parsed = record(JSON.parse(text), 'cargo-about output')
  } catch (error) {
    if (error instanceof LicenseBundleError) throw error
    throw new LicenseBundleError(`cargo-about output is not JSON: ${(error as Error).message}`)
  }
  const key = (value: unknown, what: string): string => {
    const krate = record(value, what)
    if (typeof krate.name !== 'string' || typeof krate.version !== 'string') throw new LicenseBundleError(`${what} has no crate name and version`)
    return `${krate.name}@${krate.version}`
  }
  if (!Array.isArray(parsed.crates) || !Array.isArray(parsed.licenses)) throw new LicenseBundleError('cargo-about output has no crates and licenses lists')
  const crates = new Set(parsed.crates.map((entry, index) => key(record(entry, `cargo-about crate ${index}`).package, `cargo-about crate ${index}`)))
  const licenses = parsed.licenses.map((entry, index): AboutLicense => {
    const license = record(entry, `cargo-about license ${index}`)
    if (typeof license.id !== 'string' || typeof license.text !== 'string' || !Array.isArray(license.used_by)) throw new LicenseBundleError(`cargo-about license ${index} lacks an id, text, or users`)
    const sourcePath = typeof license.source_path === 'string' ? license.source_path : null
    return { id: license.id, text: license.text, sourcePath, users: license.used_by.map((user, at) => key(record(user, `cargo-about license ${index} user ${at}`).crate, `cargo-about license ${index} user ${at}`)) }
  })
  return { crates, licenses }
}

/**
 * Build the bundle from collected inputs, reading license and notice files
 * from the crates' source directories.
 *
 * @throws LicenseBundleError listing every problem: a crate missing from either
 * side, a license text not found in the crate's sources or not verbatim, a
 * stale or broad clarification, or an unreadable notice.
 */
export function buildBundle(inputs: BundleInputs): Bundle {
  const config = readConfig(inputs.config)
  const about = readAbout(inputs.about)
  const problems: string[] = []

  const packages = new Map<string, Package>()
  const members = new Set(inputs.metadata.workspace_members)
  for (const pkg of inputs.metadata.packages) {
    const key = `${pkg.name}@${pkg.version}`
    if (packages.has(key)) {
      problems.push(`cargo metadata lists ${key} from two sources; the bundle cannot tell them apart`)
      continue
    }
    const { name, version, source, license } = pkg
    packages.set(key, { key, name, version, source, license, dir: dirname(pkg.manifest_path), workspace: members.has(pkg.id) })
  }

  const third = new Map<string, ThirdParty>()
  const workspace = new Map<string, { pkg: Package; platforms: Set<PlatformId> }>()
  const perPlatform = {} as Record<PlatformId, number>
  const treeIds = new Set(inputs.trees.map(tree => tree.id))
  for (const platform of PLATFORMS) if (!treeIds.has(platform.id)) problems.push(`no cargo tree for ${platform.id}`)
  for (const tree of inputs.trees) {
    const linkedEntries = parseTree(tree.linked)
    const linked = new Set(linkedEntries.map(entry => `${entry.name}@${entry.version}`))
    if (linkedEntries[0]?.name !== ROOT_PACKAGE) problems.push(`the ${tree.id} tree does not start at ${ROOT_PACKAGE}`)
    const hostSide = parseHostSide(inputs.hostTree, linked)
    const compiled = [...linkedEntries, ...hostSide.filter(entry => !linked.has(`${entry.name}@${entry.version}`))]
    let count = 0
    for (const entry of compiled) {
      const key = `${entry.name}@${entry.version}`
      const pkg = packages.get(key)
      if (pkg === undefined) {
        problems.push(`${key} is in the ${tree.id} tree but not in cargo metadata`)
        continue
      }
      if (pkg.workspace) {
        const seen = workspace.get(key) ?? { pkg, platforms: new Set<PlatformId>() }
        seen.platforms.add(tree.id)
        workspace.set(key, seen)
        continue
      }
      if (pkg.source !== CRATES_IO) {
        problems.push(`${key} comes from ${pkg.source ?? 'a path'}; only crates.io sources are supported`)
        continue
      }
      count++
      const seen = third.get(key) ?? { pkg, roles: new Map(), texts: new Map<string, string>(), notices: [], clarified: false }
      seen.roles.set(tree.id, linked.has(key) ? 'runtime' : 'build')
      third.set(key, seen)
    }
    perPlatform[tree.id] = count
  }

  for (const key of third.keys()) if (!about.crates.has(key)) problems.push(`${key} is compiled into bake-rs but cargo-about did not report it`)
  // cargo-about evaluates `cfg()` against all its targets at once, so it can
  // report a crate no single target compiles, such as a `target_env = "gnu"`
  // Windows import library. Such a crate is left out and listed as excluded;
  // one missing from the lock is an error.
  const excluded: string[] = []
  for (const key of about.crates) {
    if (third.has(key) || packages.get(key)?.workspace === true) continue
    if (packages.has(key)) excluded.push(key)
    else problems.push(`cargo-about reported ${key}, which is not in the locked dependency graph`)
  }
  excluded.sort(byLabel)

  const texts = new Map<string, Text>()
  const missing = new Set<string>()
  for (const license of about.licenses) {
    const users = license.users.filter(user => third.has(user))
    if (users.length === 0) continue
    if (license.sourcePath === null) {
      for (const user of users) missing.add(user)
      problems.push(`${users.join(', ')}: no ${license.id} license text was found in the source package; cargo-about would substitute the generic text, losing the crate's copyright. Add a reviewed, hashed clarification naming the crate's own file`)
      continue
    }
    // cargo-about reports a harvested file by absolute path inside one of the
    // crates sharing the text, and a clarified file relative to each clarified
    // crate's root. Either way, every crate the text is attached to must hold
    // it verbatim in its own source package.
    const sourcePath = license.sourcePath
    const owner = isAbsolute(sourcePath)
      ? users.map(user => third.get(user) as ThirdParty).find(entry => inside(entry.pkg.dir, sourcePath))
      : undefined
    if (isAbsolute(sourcePath) && owner === undefined) {
      for (const user of users) missing.add(user)
      problems.push(`${users.join(', ')}: the ${license.id} text was read from outside the crates using it`)
      continue
    }
    const preferred = owner === undefined ? sourcePath : relative(owner.pkg.dir, sourcePath)
    const found = new Map<string, string>()
    for (const user of users) {
      const entry = third.get(user) as ThirdParty
      try {
        const file = findText(entry.pkg.dir, preferred, license.text, owner === undefined)
        if (file === undefined) problems.push(`${user}: the ${license.id} text cargo-about attached is not verbatim in ${owner === undefined ? slash(preferred) : 'any file of its source package'}`)
        else found.set(user, file)
      } catch (error) {
        problems.push(error instanceof LicenseBundleError ? error.message : `${user}: ${(error as Error).message}`)
      }
    }
    if (found.size !== users.length) {
      for (const user of users) missing.add(user)
      continue
    }
    const first = [...found.keys()].sort(byLabel)[0] as string
    const digest = sha256(license.text)
    const id = `${license.id.replaceAll(' ', '_')}-${digest.slice(0, 12)}`
    const source = `${first.replace('@', ' ')}, ${found.get(first) as string}`
    const existing = texts.get(id)
    // The same bytes may be reported more than once; name the first source in sort order.
    if (existing === undefined) {
      texts.set(id, { id, license: license.id, sha256: digest, body: license.text, source, users: new Set<string>() })
    }
    else if (source < existing.source) texts.set(id, { ...existing, source })
    for (const user of users) {
      ;(texts.get(id) as Text).users.add(user)
      ;(third.get(user) as ThirdParty).texts.set(id, found.get(user) as string)
    }
  }

  for (const entry of third.values()) if (entry.texts.size === 0 && about.crates.has(entry.pkg.key) && !missing.has(entry.pkg.key)) problems.push(`${entry.pkg.key}: cargo-about attached no license text`)

  // cargo-about applies a clarification to every version of the crate it
  // names whose files match its checksums, so every compiled version must
  // match: a version that does not would fall back to the generic text.
  for (const [name, clarification] of config.clarify) {
    const matches = [...third.values()].filter(entry => entry.pkg.name === name)
    if (matches.length === 0) problems.push(`about.toml clarifies ${name}, which no release platform compiles; remove the stale clarification`)
    for (const entry of matches) clarify(entry, clarification)
  }
  function clarify(entry: ThirdParty, clarification: Clarification): void {
    entry.clarified = true
    if (clarification.license !== entry.pkg.license) problems.push(`about.toml clarifies ${entry.pkg.key} as ${JSON.stringify(clarification.license)}, but it declares ${JSON.stringify(entry.pkg.license)}; a clarification may locate texts, not change the license`)
    for (const file of clarification.files) {
      const path = resolve(entry.pkg.dir, file.path)
      if (!inside(entry.pkg.dir, path)) {
        problems.push(`about.toml clarifies ${entry.pkg.key} with ${file.path}, outside its package`)
        continue
      }
      const actual = existsSync(path) ? sha256(readFileSync(path)) : 'missing'
      if (actual !== file.checksum) problems.push(`about.toml clarifies ${entry.pkg.key} ${file.path} at ${file.checksum}, but the locked source has ${actual}; review the file and update the clarification`)
    }
  }

  for (const entry of third.values()) {
    let names: string[]
    try {
      names = readdirSync(entry.pkg.dir).sort()
    } catch {
      problems.push(`${entry.pkg.key}: its source directory is missing; run with --write to fetch the locked crates`)
      continue
    }
    for (const name of names.filter(item => ATTRIBUTION_FILE.test(item))) {
      const path = join(entry.pkg.dir, name)
      if (!lstatSync(path).isFile()) continue
      try {
        const body = decode(readFileSync(path), `${entry.pkg.key} ${name}`)
        entry.notices.push({ file: name, sha256: sha256(body), body })
      } catch (error) {
        problems.push((error as Error).message)
      }
    }
  }

  const workspaceNotices = new Map<string, Notice | null>()
  for (const { pkg } of workspace.values()) {
    if (pkg.license !== 'MIT') problems.push(`${pkg.key} is a Bake crate but declares ${JSON.stringify(pkg.license)}, not the repository's MIT`)
    const path = join(pkg.dir, 'NOTICE')
    if (!existsSync(path)) {
      workspaceNotices.set(pkg.key, null)
      continue
    }
    const file = slash(relative(inputs.root, path))
    try {
      const body = decode(readFileSync(path), file)
      workspaceNotices.set(pkg.key, { file, sha256: sha256(body), body })
    } catch (error) {
      problems.push((error as Error).message)
    }
  }

  if (problems.length > 0) throw new LicenseBundleError([...new Set(problems)].join('\n'))

  const byKey = (left: { pkg: Package }, right: { pkg: Package }): number => left.pkg.name.localeCompare(right.pkg.name, 'en') || compareVersions(left.pkg.version, right.pkg.version)
  const crates = [...third.values()].sort(byKey)
  const members_ = [...workspace.values()].sort(byKey)
  const textList = [...texts.values()].sort((left, right) => left.id.localeCompare(right.id, 'en'))
  const files: Record<string, string> = {}
  const header = {
    cargoLock: sha256(inputs.cargoLock),
    config: sha256(inputs.config),
  }
  const targetFiles: Record<string, unknown>[] = []
  for (const platform of PLATFORMS) {
    const path = `${platform.id}/${BUNDLE_FILE}`
    const own = crates.filter(entry => entry.roles.has(platform.id))
    const bake = members_
      .filter(entry => entry.platforms.has(platform.id))
      .map(entry => ({ pkg: entry.pkg, notice: workspaceNotices.get(entry.pkg.key) ?? null }))
    const text = renderPlatform(platform, own, textList, bake, header, inputs)
    files[path] = text
    targetFiles.push({ id: platform.id, triple: platform.triple, file: path, sha256: sha256(text), crates: perPlatform[platform.id] })
  }
  const inventory = {
    schema: SCHEMA,
    generator: { tool: `cargo-about ${inputs.cargoAbout}`, cargo: inputs.toolchain.cargo, toolchain: inputs.toolchain.channel },
    inputs: { cargoLock: { path: 'rust/Cargo.lock', sha256: header.cargoLock }, config: { path: 'rust/about.toml', sha256: header.config } },
    selection: { package: ROOT_PACKAGE, runtime: 'cargo tree --target <triple> -e normal,no-proc-macro', build: 'crates below build-dependency and proc-macro edges of the linked crates in cargo tree --target all -e normal,build --no-dedupe, for every build host', excluded: 'dev-dependencies' },
    platforms: targetFiles,
    workspace: members_.map(({ pkg, platforms }) => {
      const notice = workspaceNotices.get(pkg.key) ?? null
      return {
        name: pkg.name,
        version: pkg.version,
        license: pkg.license,
        platforms: PLATFORMS.map(platform => platform.id).filter(id => platforms.has(id)),
        notice: notice === null ? null : { file: notice.file, sha256: notice.sha256 },
      }
    }),
    crates: crates.map(entry => ({
      name: entry.pkg.name,
      version: entry.pkg.version,
      source: entry.pkg.source,
      license: entry.pkg.license,
      clarified: entry.clarified,
      platforms: Object.fromEntries(PLATFORMS.flatMap(({ id }) => entry.roles.has(id) ? [[id, entry.roles.get(id)]] : [])),
      texts: [...entry.texts].sort(([left], [right]) => left.localeCompare(right, 'en')).map(([id, file]) => ({ id, file })),
      notices: entry.notices.map(notice => ({ file: notice.file, sha256: notice.sha256 })),
    })),
    excluded,
    texts: textList.map(text => ({ id: text.id, license: text.license, sha256: text.sha256, source: text.source })),
  }
  files[INVENTORY_FILE] = `${JSON.stringify(inventory, null, 2)}\n`
  return {
    files,
    summary: {
      crates: crates.length,
      texts: textList.length,
      perPlatform,
      clarified: crates.filter(entry => entry.clarified).map(entry => entry.pkg.key),
      excluded,
    },
  }
}

/** Order versions numerically where they are numeric, so 0.10 follows 0.9. */
function compareVersions(left: string, right: string): number {
  const a = left.split(/[.+-]/u)
  const b = right.split(/[.+-]/u)
  for (let index = 0; index < Math.max(a.length, b.length); index++) {
    const x = a[index] ?? ''
    const y = b[index] ?? ''
    const order = /^\d+$/u.test(x) && /^\d+$/u.test(y) ? Number(x) - Number(y) : x.localeCompare(y, 'en')
    if (order !== 0) return order
  }
  return 0
}

function renderPlatform(
  platform: typeof PLATFORMS[number],
  crates: readonly ThirdParty[],
  allTexts: readonly Text[],
  members: readonly { pkg: Package; notice: Notice | null }[],
  header: { cargoLock: string; config: string },
  inputs: BundleInputs,
): string {
  const used = new Set(crates.flatMap(entry => [...entry.texts.keys()]))
  const texts = allTexts.filter(text => used.has(text.id))
  const keys = new Set(crates.map(entry => entry.pkg.key))
  const label = (key: string): string => key.replace('@', ' ')
  const lines = [
    `Third-party licenses for bake-rs, ${platform.id} (${platform.triple})`,
    '',
    'Bake is MIT-licensed (see LICENSE). This file reproduces, verbatim, the license',
    'texts and notice files found in the source packages of the Rust crates compiled',
    'into bake-rs for this platform, and the NOTICE files of Bake\'s own crates.',
    'Each crate remains under its own license.',
    '',
    `Generated by scripts/release/native-licenses.ts with cargo-about ${inputs.cargoAbout}`,
    `and Rust ${inputs.toolchain.channel} from rust/Cargo.lock (sha256 ${header.cargoLock})`,
    `and rust/about.toml (sha256 ${header.config}).`,
    `Selection: "runtime" crates are the dependencies of ${ROOT_PACKAGE} linked into the`,
    `binary for ${platform.triple}. "build" crates are build scripts, proc macros, and`,
    'their dependencies, which only run while it is built; they are listed for every',
    'build host, so this list can name some that this platform\'s build does not use.',
    '',
    RULE,
    `1. Crates (${crates.length})`,
    RULE,
    '',
    ...crates.map((entry) => {
      const notices = entry.notices.length === 0 ? '' : ` | notices: ${entry.notices.map(notice => notice.file).join(', ')}`
      return `${entry.pkg.name} ${entry.pkg.version} | ${entry.pkg.license} | ${entry.roles.get(platform.id)} | ${[...entry.texts.keys()].sort().join(', ')}${notices}`
    }),
    '',
    RULE,
    `2. License texts (${texts.length})`,
    RULE,
  ]
  for (const text of texts) {
    lines.push('', RULE, `[${text.id}] ${text.license}`, `Source: ${text.source}`, `Used by: ${[...text.users].filter(user => keys.has(user)).sort(byLabel).map(label).join(', ')}`, THIN_RULE, text.body.endsWith('\n') ? text.body.slice(0, -1) : text.body)
  }
  const thirdNotices = crates.flatMap(entry => entry.notices.map(notice => ({ title: `${entry.pkg.name} ${entry.pkg.version}, ${notice.file}`, notice })))
  const bakeNotices = members.filter(member => member.notice !== null).map(member => ({ title: `${member.pkg.name} (Bake), ${(member.notice as Notice).file}`, notice: member.notice as Notice }))
  lines.push('', RULE, `3. Notice files (${bakeNotices.length + thirdNotices.length})`, RULE)
  for (const { title, notice } of [...bakeNotices, ...thirdNotices]) {
    lines.push('', RULE, title, THIN_RULE, notice.body.endsWith('\n') ? notice.body.slice(0, -1) : notice.body)
  }
  return `${lines.join('\n')}\n`
}

function byLabel(left: string, right: string): number {
  const [a, av] = left.split('@') as [string, string]
  const [b, bv] = right.split('@') as [string, string]
  return a.localeCompare(b, 'en') || compareVersions(av, bv)
}

// ---------------------------------------------------------------- effects

/** Runs a command and returns its standard output; throws with its standard error on failure. */
export type Runner = (command: string, args: readonly string[], cwd: string) => string

/** The default runner, which inherits the environment so rustup selects `rust/rust-toolchain.toml`. */
export const run: Runner = (command, args, cwd) => {
  const result = spawnSync(command, args, { cwd, encoding: 'utf8', maxBuffer: 1 << 30 })
  if (result.error !== undefined) throw new LicenseBundleError(`${command} could not start: ${result.error.message}`)
  if (result.status !== 0) throw new LicenseBundleError(`${command} ${args.join(' ')} exited ${result.status ?? result.signal}:\n${result.stderr.trim()}`)
  return result.stdout
}

/**
 * Gather the inputs: the pinned toolchain and cargo-about, `cargo metadata`,
 * both trees for each platform, and cargo-about's JSON. Every call is locked
 * and offline.
 *
 * @param cargoAbout the cargo-about executable, `$BAKE_CARGO_ABOUT` or `cargo-about` on `PATH`
 * @throws LicenseBundleError when a tool is missing or not the pinned version, or a call fails
 */
export function collect(root: string, runner: Runner = run, cargoAbout = process.env.BAKE_CARGO_ABOUT ?? 'cargo-about'): BundleInputs {
  const rust = join(root, 'rust')
  const toolchainFile = record(Bun.TOML.parse(readFileSync(join(rust, 'rust-toolchain.toml'), 'utf8')), 'rust-toolchain.toml')
  const channel = record(toolchainFile.toolchain, 'rust-toolchain.toml [toolchain]').channel
  if (typeof channel !== 'string') throw new LicenseBundleError('rust/rust-toolchain.toml names no channel')
  const cargo = runner('cargo', ['--version'], rust).trim()
  if (!cargo.startsWith(`cargo ${channel} `)) throw new LicenseBundleError(`rust/ selects ${JSON.stringify(cargo)}, not the pinned Rust ${channel}; install it with rustup`)
  let aboutVersion: string
  try {
    aboutVersion = runner(cargoAbout, ['--version'], rust).trim()
  } catch (error) {
    throw new LicenseBundleError(`cargo-about ${CARGO_ABOUT_VERSION} is required (${(error as Error).message}). Install it with \`cargo install cargo-about --version =${CARGO_ABOUT_VERSION} --locked --features cli\`, or name it with BAKE_CARGO_ABOUT`)
  }
  if (aboutVersion !== `cargo-about ${CARGO_ABOUT_VERSION}`) throw new LicenseBundleError(`${cargoAbout} is ${JSON.stringify(aboutVersion)}; the bundle is generated with cargo-about ${CARGO_ABOUT_VERSION}`)
  const metadata = JSON.parse(runner('cargo', ['metadata', '--format-version', '1', '--locked', '--offline'], rust)) as CargoMetadata
  // Cargo otherwise picks the tree's characters from `term.unicode` or the terminal; parseHostSide reads UTF-8 only.
  const tree = ['tree', '--locked', '--offline', '--charset', 'utf8', '-p', ROOT_PACKAGE]
  const trees = PLATFORMS.map(platform => ({ id: platform.id, linked: runner('cargo', [...tree, '--target', platform.triple, '-e', 'normal,no-proc-macro', '--prefix', 'none', '-f', '{p}'], rust) }))
  const hostTree = runner('cargo', [...tree, '--target', 'all', '-e', 'normal,build', '--no-dedupe', '-f', '{p}'], rust)
  // cargo-about refuses redirected stdout under PowerShell. Its output-file
  // option also keeps shell encoding out of the collected license text.
  const scratch = mkdtempSync(join(tmpdir(), 'bake-license-collect-'))
  let about: string
  try {
    const output = join(scratch, 'licenses.json')
    runner(cargoAbout, ['about', 'generate', '--frozen', '--fail', '-c', 'about.toml', '-m', join('crates', ROOT_PACKAGE, 'Cargo.toml'), '--format', 'json', '--output-file', output], rust)
    about = readFileSync(output, 'utf8')
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
  return {
    root,
    cargoLock: readFileSync(join(rust, 'Cargo.lock')),
    config: readFileSync(join(rust, 'about.toml'), 'utf8'),
    toolchain: { channel, cargo },
    cargoAbout: CARGO_ABOUT_VERSION,
    metadata,
    trees,
    hostTree,
    about,
  }
}

function listFiles(dir: string, prefix = ''): string[] {
  return readdirSync(join(dir, prefix), { withFileTypes: true }).flatMap((entry) => {
    const path = prefix === '' ? entry.name : `${prefix}/${entry.name}`
    return entry.isDirectory() ? listFiles(dir, path) : [path]
  }).sort()
}

/**
 * Compare an output directory with the expected bundle without writing.
 *
 * @returns every difference: a missing, extra, or changed file, or an inventory that is not this schema's JSON
 */
export function checkArtifacts(out: string, files: Readonly<Record<string, string>>): string[] {
  if (!existsSync(out)) return [`${out} does not exist; run with --write`]
  const problems: string[] = []
  const inventory = join(out, INVENTORY_FILE)
  if (existsSync(inventory)) {
    try {
      if ((JSON.parse(readFileSync(inventory, 'utf8')) as { schema?: unknown }).schema !== SCHEMA) problems.push(`${INVENTORY_FILE} is not a ${SCHEMA} inventory`)
    } catch {
      problems.push(`${INVENTORY_FILE} is not JSON`)
    }
  }
  const actual = new Set(listFiles(out))
  for (const path of Object.keys(files).sort()) {
    if (!actual.has(path)) problems.push(`${path} is missing`)
    else if (readFileSync(join(out, path), 'utf8') !== files[path]) problems.push(`${path} is out of date`)
  }
  for (const path of actual) if (!(path in files)) problems.push(`${path} is not part of the bundle`)
  return problems
}

/** Every path a bundle directory may hold; directories are the platform ids. */
const LAYOUT = new Set<string>([INVENTORY_FILE, ...PLATFORMS.flatMap(platform => [platform.id, `${platform.id}/${BUNDLE_FILE}`])])

/**
 * Whether `dir` exists to be replaced: `false` when it is absent, `true` when
 * it is empty or holds nothing but an earlier bundle's files and directories.
 * A link or any other entry is refused.
 *
 * @throws LicenseBundleError naming what makes `dir` not a bundle
 */
function previousBundle(dir: string): boolean {
  const stat = lstatSync(dir, { throwIfNoEntry: false })
  if (stat === undefined) return false
  if (!stat.isDirectory()) throw new LicenseBundleError(`${dir} is not a directory; refusing to replace it`)
  const unexpected: string[] = []
  const walk = (prefix: string): void => {
    for (const entry of readdirSync(join(dir, prefix), { withFileTypes: true })) {
      const path = prefix === '' ? entry.name : `${prefix}/${entry.name}`
      const directory = PLATFORMS.some(platform => platform.id === path)
      if (!LAYOUT.has(path) || (directory ? !entry.isDirectory() : !entry.isFile())) unexpected.push(path)
      else if (directory) walk(path)
    }
  }
  walk('')
  if (unexpected.length > 0) throw new LicenseBundleError(`${dir} holds ${unexpected.sort().join(', ')}, which no license bundle writes; refusing to replace it`)
  if (readdirSync(dir).length === 0) return true
  let schema: unknown
  try {
    schema = (JSON.parse(readFileSync(join(dir, INVENTORY_FILE), 'utf8')) as { schema?: unknown }).schema
  } catch {
    schema = undefined
  }
  if (schema !== SCHEMA) throw new LicenseBundleError(`${dir} holds no ${SCHEMA} inventory; refusing to replace it`)
  return true
}

/** Seams for fault-injection tests; production uses the file system's own operations. */
export interface WriteSeams {
  readonly rename?: (from: string, to: string) => void
}

/**
 * Replace an output directory with the bundle. The directory must be absent,
 * empty, or an earlier bundle with nothing else in it, so a mistyped `--out`
 * cannot delete other files. The bundle is staged in a directory of its own
 * beside `out`, and the earlier bundle is moved aside and deleted only once
 * the new one is in place. On any failure after the move it is moved back;
 * if that fails too, it is left where it was moved and the error names it.
 */
export function writeArtifacts(out: string, files: Readonly<Record<string, string>>, seams: WriteSeams = {}): void {
  const rename = seams.rename ?? renameSync
  const replacing = previousBundle(out)
  mkdirSync(dirname(out), { recursive: true })
  const work = mkdtempSync(join(dirname(out), '.native-licenses-'))
  const previous = join(work, 'previous')
  let keepWork = false
  // The one way back once the earlier bundle has been moved aside.
  const restore = (cause: unknown): never => {
    try {
      rename(previous, out)
    } catch (error) {
      keepWork = true
      throw new LicenseBundleError(`${(cause as Error).message}\nThe earlier bundle could not be moved back to ${out} (${(error as Error).message}); it is kept at ${previous}`)
    }
    throw cause
  }
  try {
    const stage = join(work, 'bundle')
    for (const [path, text] of Object.entries(files)) {
      mkdirSync(dirname(join(stage, path)), { recursive: true })
      writeFileSync(join(stage, path), text, { flag: 'wx' })
    }
    if (replacing) {
      rename(out, previous)
      // Something may have been added between the check and the move.
      try {
        previousBundle(previous)
      } catch (error) {
        restore(error)
      }
    }
    try {
      rename(stage, out)
    } catch (error) {
      if (replacing) restore(error)
      throw error
    }
  } finally {
    if (!keepWork) rmSync(work, { recursive: true, force: true })
  }
}

/**
 * The real path of `path`, resolving the links of its longest existing
 * ancestor; a link to a missing path is refused, since what it names could
 * be created anywhere.
 */
function canonicalPath(path: string): string {
  const rest: string[] = []
  let current = resolve(path)
  for (;;) {
    if (lstatSync(current, { throwIfNoEntry: false }) !== undefined) {
      try {
        return join(realpathSync.native(current), ...rest.reverse())
      } catch {
        throw new LicenseBundleError(`${current} is a link to a path that does not exist`)
      }
    }
    const parent = dirname(current)
    if (parent === current) return resolve(path)
    rest.push(basename(current))
    current = parent
  }
}

/**
 * Resolve `--out` through any links: outside the repository, or inside it only
 * under `.artifacts/native/`, and never in the TypeScript release's
 * `distribution/` or `.artifacts/bake-release/`.
 *
 * @throws LicenseBundleError naming the refused directory
 */
export function outputDir(root: string, requested: string | undefined): string {
  const out = canonicalPath(resolve(root, requested ?? DEFAULT_OUT))
  const repo = canonicalPath(root)
  const within = (dir: string): boolean => out === dir || inside(dir, out)
  const refuse = (why: string): never => { throw new LicenseBundleError(`--out ${requested ?? DEFAULT_OUT} resolves to ${out}, ${why}`) }
  for (const route of ['distribution', '.artifacts/bake-release']) if (within(canonicalPath(join(root, route)))) refuse(`inside the TypeScript release's ${route}/`)
  if (out === repo || inside(out, repo)) refuse('which is or contains the repository')
  if (inside(repo, out) && !within(canonicalPath(join(root, '.artifacts/native')))) refuse('in the repository but not under .artifacts/native/')
  return out
}

const USAGE = 'Usage: bun scripts/release/native-licenses.ts [--check | --write] [--out <dir>]'

/** Command-line entry; returns the exit status. */
export function main(argv: readonly string[], root = resolve(import.meta.dir, '../..'), runner: Runner = run): number {
  let mode: 'check' | 'write' = 'check'
  let requested: string | undefined
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    if (arg === '--check' || arg === '--write') mode = arg === '--check' ? 'check' : 'write'
    else if (arg === '--out' && argv[index + 1] !== undefined) requested = argv[++index]
    else {
      console.error(USAGE)
      return 2
    }
  }
  try {
    const out = outputDir(root, requested)
    if (mode === 'write') runner('cargo', ['fetch', '--locked'], join(root, 'rust'))
    const bundle = buildBundle(collect(root, runner))
    const counts = PLATFORMS.map(platform => `${platform.id} ${bundle.summary.perPlatform[platform.id]}`).join(', ')
    if (bundle.summary.excluded.length > 0) console.log(`Excluded, as no release platform compiles them: ${bundle.summary.excluded.join(', ')}`)
    if (mode === 'write') {
      mkdirSync(dirname(out), { recursive: true })
      // A link swapped in while the parent was created would move the output elsewhere.
      if (outputDir(root, requested) !== out) throw new LicenseBundleError(`${out} changed while its parent was created`)
      writeArtifacts(out, bundle.files)
      console.log(`Wrote ${out}: ${bundle.summary.crates} crates (${counts}), ${bundle.summary.texts} license texts`)
      return 0
    }
    const problems = checkArtifacts(out, bundle.files)
    if (problems.length > 0) {
      console.error(`The native license bundle at ${out} does not match the locked dependencies:\n${problems.map(problem => `  ${problem}`).join('\n')}\nRun with --write to regenerate it.`)
      return 1
    }
    console.log(`${out} matches: ${bundle.summary.crates} crates (${counts}), ${bundle.summary.texts} license texts`)
    return 0
  } catch (error) {
    if (!(error instanceof LicenseBundleError)) throw error
    console.error(error.message)
    return 1
  }
}

if (import.meta.main) process.exitCode = main(process.argv.slice(2))
