/**
 * The native preview packager refuses a binary that does not report this
 * build, keeps an earlier archive through every refusal, and archives only
 * regular payload files with their modes. These tests run stand-in binaries,
 * shell scripts that print what each case needs, and raw license bundles in
 * the generator's output layout; `pack-rust.native.ts` runs the real
 * `bake-rs` and the real generator.
 */
import { afterEach, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import {
  chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync,
  symlinkSync, writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { BUNDLE_FILE, INVENTORY_FILE, PLATFORMS, SCHEMA } from './native-licenses.ts'
import {
  archiveName, canonicalPath, checkOutputDir, docsDigests, hostTarget, inside, isNativeVersion, isRustTarget, packRust, parseSelfCheck,
  payload, targetLicenses, verifyArchive, workspaceInputs, type Expected, type PackOptions, type RustTarget, type TargetLicenses,
} from './pack-rust.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A private directory whose path holds spaces and non-ASCII text. */
function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-rs pack ñ 測試-'))
  roots.push(root)
  return root
}

const sha = (text: string): string => createHash('sha256').update(text).digest('hex')
const DOCS = { cli: '# CLI\n\nRun it.\n', home: '# Home\n\nWhere it lives.\n' }
const VERSION = '0.4.0-dev'
const host = hostTarget()
const releaseHost = isRustTarget(host)
const target = host as RustTarget

/** The record the real binary prints, members in its order. */
function record(overrides: Record<string, unknown> = {}): string {
  const docs = Object.fromEntries(Object.entries(DOCS).map(([name, text]) => [name, sha(text)]))
  return `${JSON.stringify({ kind: 'bake-rs-self-check', schema: 1, version: VERSION, runtime: 'rust', target, docs, ...overrides })}\n`
}

const quote = (text: string): string => `'${text.replaceAll('\'', '\'\\\'\'')}'`

interface Script { version?: string; check?: string; code?: number; body?: string }

/** A stand-in `bake-rs`: only shell builtins, because the packager runs it with an empty PATH. */
function fakeBinary(path: string, script: Script = {}): string {
  const version = script.version ?? `bake-rs ${VERSION}\n`
  const check = script.body ?? `printf '%s' ${quote(script.check ?? record())}; exit ${script.code ?? 0}`
  writeFileSync(path, `#!/bin/sh\ncase "$1" in\n  --version) printf '%s' ${quote(version)} ;;\n  --self-check) ${check} ;;\n  *) exit 64 ;;\nesac\n`)
  chmodSync(path, 0o755)
  return path
}

const NOTICE = 'Pi, Copyright (c) 2025 Mario Zechner, MIT\n'
const LOCK = '# locked\nversion = 4\n'
const ABOUT = 'accepted = ["MIT"]\n'

/** Each platform's stand-in license text; the heading is the generator's. */
const licenseText = (id: string, triple: string): string =>
  `Third-party licenses for bake-rs, ${id} (${triple})\n\nmemchr 2.8.3 | Unlicense OR MIT | runtime | MIT-0123456789ab\n\nCopyright (c) 2015 Andrew Gallant (${id})\n`

/** The inventory fields the packager reads, as the generator writes them. */
interface FixtureInventory {
  schema: string
  generator: { tool: string; cargo: string; toolchain: string }
  inputs: Record<string, { path: string; sha256: string } | undefined>
  platforms: { id: string; triple: string; file: string; sha256: string; crates: number }[]
  workspace: { name: string; version: string; license: string; platforms: string[]; notice: { file: string; sha256: string } | null }[]
}

interface BundleEdit {
  /** Text replacements by path, made before the texts are hashed; `null` leaves a text out. */
  readonly texts?: Readonly<Record<string, string | null>>
  /** Changes the inventory once the platform hashes are in it. */
  readonly inventory?: (inventory: FixtureInventory) => void
}

/** A bundle in `native-licenses.ts`'s output layout for the fixture's `rust/Cargo.lock`, `rust/about.toml`, and bake-ai NOTICE. */
function licenseBundle(edit: BundleEdit = {}): Record<string, string> {
  const files: Record<string, string> = {}
  for (const { id, triple } of PLATFORMS) {
    const path = `${id}/${BUNDLE_FILE}`
    const text = edit.texts?.[path] === undefined ? licenseText(id, triple) : edit.texts[path]
    if (text !== null) files[path] = text
  }
  const ids = PLATFORMS.map(platform => platform.id)
  const inventory: FixtureInventory = {
    schema: SCHEMA,
    generator: { tool: 'cargo-about 0.9.2', cargo: 'cargo 1.99.0 (5f94df478 2026-08-27)', toolchain: '1.99.0' },
    inputs: { cargoLock: { path: 'rust/Cargo.lock', sha256: sha(LOCK) }, config: { path: 'rust/about.toml', sha256: sha(ABOUT) } },
    platforms: PLATFORMS.map(({ id, triple }) => ({ id, triple, file: `${id}/${BUNDLE_FILE}`, sha256: sha(files[`${id}/${BUNDLE_FILE}`] ?? ''), crates: 1 })),
    workspace: [
      { name: 'bake-ai', version: VERSION, license: 'MIT', platforms: ids, notice: { file: 'rust/crates/bake-ai/NOTICE', sha256: sha(NOTICE) } },
      { name: 'bake-cli', version: VERSION, license: 'MIT', platforms: ids, notice: null },
    ],
  }
  edit.inventory?.(inventory)
  files[INVENTORY_FILE] = `${JSON.stringify(inventory, null, 2)}\n`
  return files
}

/** This host's text path in a bundle, and another platform's. */
const OWN = `${target}/${BUNDLE_FILE}`
const OTHER = PLATFORMS.find(platform => platform.id !== target) as typeof PLATFORMS[number]

/** `bundle` with one file left out, or with bytes appended after hashing. */
const without = (bundle: Record<string, string>, path: string): Record<string, string> =>
  Object.fromEntries(Object.entries(bundle).filter(([name]) => name !== path))
const tampered = (bundle: Record<string, string>, path: string): Record<string, string> => ({ ...bundle, [path]: `${bundle[path] ?? ''}edited\n` })

/** A repository-shaped root: license, notices, docs, one crate NOTICE, the license inputs, and a stand-in binary. */
function fixture(script: Script = {}): { root: string; options: PackOptions; destination: string } {
  const root = scratch()
  writeFileSync(join(root, 'LICENSE'), 'MIT License\n')
  mkdirSync(join(root, 'rust'))
  writeFileSync(join(root, 'rust/Cargo.lock'), LOCK)
  writeFileSync(join(root, 'rust/about.toml'), ABOUT)
  writeFileSync(join(root, 'THIRD_PARTY_NOTICES.md'), '# Notices\n')
  mkdirSync(join(root, 'docs'))
  for (const [name, text] of Object.entries(DOCS)) writeFileSync(join(root, 'docs', `${name}.md`), text)
  writeFileSync(join(root, 'docs', 'README.txt'), 'not a topic\n')
  mkdirSync(join(root, 'crates/bake-ai'), { recursive: true })
  writeFileSync(join(root, 'crates/bake-ai/NOTICE'), NOTICE)
  mkdirSync(join(root, 'target'))
  const binary = fakeBinary(join(root, 'target/bake-rs'), script)
  const outputDir = join(root, '.artifacts/bake-rs-release', VERSION)
  const options: PackOptions = {
    binary, root, docs: join(root, 'docs'), notices: { 'bake-ai': join(root, 'crates/bake-ai/NOTICE') }, licenses: licenseBundle(), version: VERSION, target, outputDir, timeoutMs: 5_000,
  }
  return { root, options, destination: join(outputDir, archiveName(VERSION, target)) }
}

const EARLIER = 'an earlier archive that must survive\n'
/** License files for cases that never stage them. */
const LICENSES: TargetLicenses = { text: 'licenses\n', inventory: '{}\n', notices: {} }

/** Run a pack that must fail, over an existing archive that must be left as it was. */
async function refused(options: PackOptions, destination: string, message: RegExp): Promise<void> {
  mkdirSync(options.outputDir, { recursive: true })
  writeFileSync(destination, EARLIER)
  await expect(packRust(options)).rejects.toThrow(message)
  expect(readFileSync(destination, 'utf8')).toBe(EARLIER)
  expect(readdirSync(options.outputDir)).toEqual([archiveName(VERSION, target)])
}

describe('version and platform names', () => {
  test('every release platform is named and the archive is apart from the TypeScript one', () => {
    for (const name of ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']) expect(isRustTarget(name)).toBe(true)
    for (const name of ['win32-arm64', 'linux-ia32', 'freebsd-x64', '']) expect(isRustTarget(name)).toBe(false)
    expect(archiveName('0.4.0-dev', 'linux-x64')).toBe('bake-rs-v0.4.0-dev-linux-x64.tar.gz')
  })

  test('a native version is SemVer without build metadata', () => {
    for (const version of ['0.4.0', '0.4.0-dev', '1.2.3-rc.1']) expect(isNativeVersion(version)).toBe(true)
    for (const version of ['0.4', '00.4.0', '0.4.0+abc', '0.4.0-', 'v0.4.0', '0.4.0/../x', 4]) expect(isNativeVersion(version)).toBe(false)
  })

  test('the output guard knows what is inside a directory', () => {
    expect(inside('/a/b', '/a/b')).toBe(true)
    expect(inside('/a/b', '/a/b/c')).toBe(true)
    expect(inside('/a/b', '/a/bc')).toBe(false)
    expect(inside('/a/b', '/a')).toBe(false)
    expect(inside('/a/b', '/a/b/../c')).toBe(false)
    expect(inside('/a/b', '/a/b/..c')).toBe(true)
  })
})

describe('the self-check record', () => {
  const expected: Expected = { version: VERSION, target: 'linux-x64', docs: { cli: sha(DOCS.cli), home: sha(DOCS.home) } }
  const line = (overrides: Record<string, unknown> = {}): string =>
    `${JSON.stringify({ kind: 'bake-rs-self-check', schema: 1, version: VERSION, runtime: 'rust', target: 'linux-x64', docs: expected.docs, ...overrides })}\n`

  test('accepts the record of this build', () => {
    expect(parseSelfCheck(line(), expected).target).toBe('linux-x64')
  })

  test.each([
    ['empty output', '', /not one line/],
    ['a truncated record', line().slice(0, 40), /not one line/],
    ['a record without its newline', line().trimEnd(), /not one line/],
    ['a second line', `${line()}extra\n`, /not one line/],
    ['text that is not JSON', 'ok\n', /not JSON/],
    ['an array', '[]\n', /not a JSON object/],
    ['another kind', line({ kind: 'bake-self-check' }), /kind/],
    ['a newer schema', line({ schema: 2 }), /schema 1/],
    ['another runtime', line({ runtime: 'node' }), /runtime/],
    ['an extra member', line({ node: '24' }), /members/],
    ['another version', line({ version: '0.4.0' }), /version "0\.4\.0"; Cargo builds 0\.4\.0-dev/],
    ['a foreign platform', line({ target: 'darwin-arm64' }), /platform "darwin-arm64"; this build is for linux-x64/],
    ['docs that are not an object', line({ docs: [] }), /docs is not an object/],
    ['a missing topic', line({ docs: { cli: sha(DOCS.cli) } }), /lacks documentation topic 'home'/],
    ['an extra topic', line({ docs: { ...expected.docs, old: sha('x') } }), /topic 'old', which the sources lack/],
    ['a stale topic', line({ docs: { ...expected.docs, cli: sha('# Old\n') } }), /topic 'cli' differs/],
  ])('refuses %s', (_name, stdout, message) => {
    expect(() => parseSelfCheck(stdout, expected)).toThrow(message)
  })

  test('documentation digests come from the Markdown files only', () => {
    const { options } = fixture()
    expect(docsDigests(options.docs)).toEqual({ cli: sha(DOCS.cli), home: sha(DOCS.home) })
    const empty = scratch()
    expect(() => docsDigests(empty)).toThrow(/holds no documentation/)
  })
})

describe('workspace inputs', () => {
  test('notices follow the crates bake-cli links, not every workspace member', () => {
    const root = scratch()
    for (const crate of ['bake-cli', 'bake-ai', 'bake-mcp', 'bake-conformance']) {
      mkdirSync(join(root, crate))
      if (crate !== 'bake-cli') writeFileSync(join(root, crate, 'NOTICE'), 'notice\n')
    }
    const pkg = (name: string, dependencies: { name: string; kind: string | null; path?: string }[] = []) =>
      ({ name, version: '0.4.0-dev', manifest_path: join(root, name, 'Cargo.toml'), dependencies })
    const inputs = workspaceInputs({
      target_directory: join(root, 'target'),
      packages: [
        pkg('bake-cli', [
          { name: 'bake-ai', kind: null, path: join(root, 'bake-ai') },
          { name: 'serde_json', kind: null },
          { name: 'bake-conformance', kind: 'dev', path: join(root, 'bake-conformance') },
        ]),
        pkg('bake-ai'), pkg('bake-mcp'), pkg('bake-conformance'),
      ],
    })
    expect(inputs.version).toBe('0.4.0-dev')
    expect(inputs.docs).toBe(join(root, 'bake-cli', 'docs'))
    expect(inputs.notices).toEqual({ 'bake-ai': join(root, 'bake-ai', 'NOTICE') })
  })

  test('crate names cannot leave the notices directory', () => {
    expect(() => payload({ binary: 'b', root: 'r', notices: { '../x': 'n' } }, 'linux-x64', LICENSES)).toThrow(/cannot name a notice directory/)
  })
})

describe('the license bundle', () => {
  const check = (options: PackOptions, licenses: Readonly<Record<string, string>> = options.licenses): TargetLicenses =>
    targetLicenses(licenses, { target, version: VERSION, root: options.root, notices: options.notices })

  test.skipIf(!releaseHost)('yields this platform\'s text and the inventory, as generated', () => {
    const { options } = fixture()
    expect(check(options)).toEqual({ text: options.licenses[OWN] as string, inventory: options.licenses[INVENTORY_FILE] as string, notices: { 'bake-ai': sha(NOTICE) } })
  })

  const edited = (change: (inventory: FixtureInventory) => void): (() => Record<string, string>) =>
    () => licenseBundle({ inventory: change })
  test.skipIf(!releaseHost).each([
    ['no inventory', () => without(licenseBundle(), INVENTORY_FILE), /has no inventory\.json/],
    ['an inventory that is not JSON', () => ({ ...licenseBundle(), [INVENTORY_FILE]: '{ broken' }), /inventory\.json is not JSON/],
    ['another schema', edited((inventory) => { inventory.schema = 'bake-native-licenses/0' }), /not a bake-native-licenses\/1 inventory/],
    ['another cargo-about', edited((inventory) => { inventory.generator.tool = 'cargo-about 0.9.1' }), /generated by "cargo-about 0\.9\.1", not cargo-about 0\.9\.2/],
    ['no Cargo.lock input', edited((inventory) => { inventory.inputs.cargoLock = undefined }), /does not record rust\/Cargo\.lock/],
    ['an input outside the repository', edited((inventory) => { inventory.inputs.extra = { path: '../elsewhere', sha256: sha('') } }), /not a file of the repository/],
    ['this platform\'s text omitted', () => licenseBundle({ texts: { [OWN]: null } }), new RegExp(`lacks ${OWN}`)],
    ['this platform\'s text tampered with', () => tampered(licenseBundle(), OWN), new RegExp(`${OWN} that differs from its inventory hash`)],
    ['another platform\'s text tampered with', () => tampered(licenseBundle(), `${OTHER.id}/${BUNDLE_FILE}`), /differs from its inventory hash/],
    ['a file the inventory does not list', () => ({ ...licenseBundle(), [`${target}/notes.txt`]: 'x\n' }), new RegExp(`holds ${target}/notes\\.txt, which its inventory does not list`)],
    ['no entry for this platform', () => licenseBundle({
      texts: { [OWN]: null },
      inventory: (inventory) => { inventory.platforms = inventory.platforms.filter(entry => entry.id !== target) },
    }), new RegExp(`has no ${target} text`)],
    ['this platform under another triple', edited((inventory) => {
      for (const entry of inventory.platforms) if (entry.id === target) entry.triple = OTHER.triple
    }), new RegExp(`lists ${target} as "${OTHER.triple}"`)],
    ['another platform\'s text under this platform\'s name, rehashed', () => licenseBundle({ texts: { [OWN]: licenseText(OTHER.id, OTHER.triple) } }), /whose heading names another platform/],
    ['another bake-cli version', edited((inventory) => { (inventory.workspace[1] as FixtureInventory['workspace'][number]).version = '0.4.1' }), /describes bake-cli "0\.4\.1", not 0\.4\.0-dev/],
    ['a linked NOTICE left out', edited((inventory) => { (inventory.workspace[0] as FixtureInventory['workspace'][number]).notice = null }), /does not reproduce the NOTICE of bake-ai/],
    ['a NOTICE reproduced only for other platforms', edited((inventory) => { (inventory.workspace[0] as FixtureInventory['workspace'][number]).platforms = [OTHER.id] }), /does not reproduce the NOTICE of bake-ai/],
  ])('refuses %s', (_name, bundle, message) => {
    const { options } = fixture()
    expect(() => check(options, bundle())).toThrow(message)
  })

  test.skipIf(!releaseHost)('refuses a bundle the checkout has moved past', () => {
    const { root, options } = fixture()
    const licenses = licenseBundle()
    writeFileSync(join(root, 'rust/Cargo.lock'), `${LOCK}# a dependency changed\n`)
    expect(() => check(options, licenses)).toThrow(new RegExp(`generated from rust/Cargo\\.lock at sha256 ${sha(LOCK)}, but this checkout has [0-9a-f]{64}`))
    rmSync(join(root, 'rust/Cargo.lock'))
    expect(() => check(options, licenses)).toThrow(/but this checkout has missing/)
    writeFileSync(join(root, 'rust/Cargo.lock'), LOCK)
    writeFileSync(join(root, 'rust/about.toml'), `${ABOUT}accepted = ["MIT", "ISC"]\n`)
    expect(() => check(options, licenses)).toThrow(/generated from rust\/about\.toml/)
  })

  // Inputs beyond the lock and config, such as reviewed upstream texts, are checked by
  // whatever name the generator records them under, without knowing what they are.
  test.skipIf(!releaseHost)('refuses a change to any other recorded source input', () => {
    const { root, options } = fixture()
    const manifest = 'rust/licenses/upstream.toml'
    const vendored = 'rust/licenses/upstream/example-v1.0.0/LICENSE'
    const sources = {
      [manifest]: '[[text]]\nfile = "upstream/example-v1.0.0/LICENSE"\n',
      [vendored]: 'Apache License\nVersion 2.0, January 2004\n',
    }
    for (const [path, text] of Object.entries(sources)) {
      mkdirSync(join(root, path, '..'), { recursive: true })
      writeFileSync(join(root, path), text)
    }
    const licenses = licenseBundle({
      inventory: (inventory) => {
        inventory.inputs.upstream = { path: manifest, sha256: sha(sources[manifest] as string) }
        inventory.inputs.upstreamText = { path: vendored, sha256: sha(sources[vendored] as string) }
      },
    })
    expect(check(options, licenses).text).toBe(licenses[OWN] as string)
    writeFileSync(join(root, vendored), `${sources[vendored]}edited\n`)
    const stale = new RegExp(`generated from ${vendored.replaceAll('.', '\\.')} at sha256 ${sha(sources[vendored] as string)}, but this checkout has [0-9a-f]{64}`)
    expect(() => check(options, licenses)).toThrow(stale)
    writeFileSync(join(root, vendored), sources[vendored] as string)
    writeFileSync(join(root, manifest), `${sources[manifest]}# a pin changed\n`)
    expect(() => check(options, licenses)).toThrow(/generated from rust\/licenses\/upstream\.toml/)
    rmSync(join(root, manifest))
    expect(() => check(options, licenses)).toThrow(/upstream\.toml at sha256 [0-9a-f]{64}, but this checkout has missing/)
  })

  test.skipIf(!releaseHost)('refuses NOTICE files of other crates than the archive\'s, and names the hashes the archived copies must have', () => {
    const { options } = fixture()
    expect(() => check({ ...options, notices: {} })).toThrow(/reproduces a NOTICE of bake-ai, which the archive does not carry/)
    expect(check(options).notices).toEqual({ 'bake-ai': sha(NOTICE) })
  })
})

// The stand-in binaries are shell scripts and the checks read POSIX modes,
// so these run outside Windows; the native smoke covers the Windows binary.
describe.skipIf(process.platform === 'win32' || !releaseHost)('packing', () => {
  test('archives exactly the payload with its modes, and the archive unpacks and runs', async () => {
    const { options, destination } = fixture()
    const result = await packRust(options)
    expect(result.archive).toBe(realpathSync.native(destination))
    expect(result.record.target).toBe(target)
    expect([...result.listing].sort()).toEqual([
      '0644 LICENSE', '0644 THIRD_PARTY_NOTICES.md', '0755 bin/', '0755 bin/bake-rs',
      '0755 licenses/', '0644 licenses/inventory.json', `0755 licenses/${target}/`, `0644 licenses/${target}/THIRD-PARTY-LICENSES.txt`,
      '0755 notices/', '0755 notices/bake-ai/', '0644 notices/bake-ai/NOTICE',
    ].sort())
    expect(readdirSync(options.outputDir)).toEqual([archiveName(VERSION, target)])
    const unpacked = scratch()
    expect(Bun.spawnSync(['tar', '-xzf', destination, '-C', unpacked]).exitCode).toBe(0)
    expect(statSync(join(unpacked, 'bin/bake-rs')).mode & 0o777).toBe(0o755)
    expect(readFileSync(join(unpacked, 'notices/bake-ai/NOTICE'), 'utf8')).toContain('Mario Zechner')
    expect(readdirSync(unpacked).sort()).toEqual(['LICENSE', 'THIRD_PARTY_NOTICES.md', 'bin', 'licenses', 'notices'])
    // This platform's text and the inventory, byte for byte, and no other platform's text.
    expect(readdirSync(join(unpacked, 'licenses')).sort()).toEqual([target, 'inventory.json'].sort())
    expect(readFileSync(join(unpacked, 'licenses', target, 'THIRD-PARTY-LICENSES.txt'), 'utf8')).toBe(options.licenses[`${target}/${BUNDLE_FILE}`] as string)
    expect(readFileSync(join(unpacked, 'licenses/inventory.json'), 'utf8')).toBe(options.licenses[INVENTORY_FILE] as string)
  })

  test('a later pack replaces the earlier archive only once it passes', async () => {
    const { options, destination } = fixture()
    mkdirSync(options.outputDir, { recursive: true })
    writeFileSync(destination, EARLIER)
    await packRust(options)
    expect(readFileSync(destination, 'utf8')).not.toBe(EARLIER)
    expect(readdirSync(options.outputDir)).toEqual([archiveName(VERSION, target)])
  })

  test.skipIf(process.platform !== 'darwin')('source metadata adds no AppleDouble files or extended attributes to the package', async () => {
    const { options } = fixture()
    const attribute = 'com.bake.package-fixture'
    const source = join(options.root, 'LICENSE')
    expect(Bun.spawnSync(['/usr/bin/xattr', '-w', attribute, 'fixture', source]).exitCode).toBe(0)
    const result = await packRust(options)
    expect(result.listing.some(entry => entry.includes('._'))).toBe(false)
    const unpacked = scratch()
    expect(Bun.spawnSync(['tar', '-xzf', result.archive, '-C', unpacked]).exitCode).toBe(0)
    const attributes = Bun.spawnSync(['/usr/bin/xattr', join(unpacked, 'LICENSE')], { stdout: 'pipe', stderr: 'pipe' })
    expect(attributes.exitCode).toBe(0)
    expect(attributes.stdout.toString().split('\n')).not.toContain(attribute)
  })

  test.each([
    ['a refused self-check', { check: '', code: 1 }, /--self-check exited 1/],
    ['a truncated record', { check: record().slice(0, 30) }, /not one line/],
    ['a corrupted record', { check: record().replace('"schema":1', '"schema":1,,') }, /not JSON/],
    ['another version', { check: record({ version: '0.4.1' }) }, /reports version "0\.4\.1"/],
    ['a foreign platform', { check: record({ target: target === 'linux-x64' ? 'linux-arm64' : 'linux-x64' }) }, /reports platform/],
    ['stale documentation', { check: record({ docs: { cli: sha('# Old\n'), home: sha(DOCS.home) } }) }, /topic 'cli' differs/],
    ['another --version', { version: 'bake-rs 0.3.8\n' }, /--version exited 0 with "bake-rs 0\.3\.8\\n"/],
    ['a write into the home', { body: `printf x > "$HOME/leak"; printf '%s' ${quote(record())}` }, /wrote leak into its empty home/],
    ['a check that never ends', { body: 'while :; do :; done' }, /did not finish within 1000 ms/],
    ['endless output', { body: 'while :; do printf "%s" xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done' }, /more than 1048576 bytes/],
  ])('refuses %s and keeps the earlier archive', async (_name, script, message) => {
    const { options, destination } = fixture(script)
    await refused({ ...options, timeoutMs: 1_000 }, destination, message)
  })

  // A process the stand-in leaves behind holds both output pipes open after
  // the stand-in exits; the run must still end at its deadline, or at the
  // other stream's overflow, rather than wait for the descendant.
  test.each([
    ['a descendant that keeps the output open', `printf '%s' ${quote(record())}`, /did not finish within 1000 ms/],
    ['overflow while a descendant keeps the other stream open', 'while :; do printf "%s" xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx >&2; done', /more than 1048576 bytes/],
  ])('refuses %s without waiting for it', async (_name, then, message) => {
    const pidFile = join(scratch(), 'descendant.pid')
    const { options, destination } = fixture({ body: `/bin/sleep 30 & printf '%s' $! > ${quote(pidFile)}; ${then}` })
    const started = performance.now()
    try {
      await refused({ ...options, timeoutMs: 1_000 }, destination, message)
      expect(performance.now() - started).toBeLessThan(10_000)
    } finally {
      // The packager kills only its immediate child; the test owns the descendant.
      const pid = Number(readFileSync(pidFile, 'utf8'))
      if (Number.isInteger(pid) && pid > 0) {
        try { process.kill(pid, 'SIGKILL') } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
        }
        // The orphan's new parent reaps it; a zombie has already stopped.
        // Observe that stop before afterEach removes its temporary paths.
        const deadline = performance.now() + 10_000
        for (;;) {
          const observed = Bun.spawnSync(['/bin/ps', '-p', String(pid), '-o', 'stat='], { stdout: 'pipe', stderr: 'pipe' })
          const state = observed.stdout.toString().trim()
          if (observed.exitCode === 1 && state === '') break
          if (observed.exitCode !== 0) throw new Error(`Cannot observe fixture process ${pid}: ${observed.stderr.toString()}`)
          if (state.startsWith('Z')) break
          if (performance.now() >= deadline) throw new Error(`Fixture process ${pid} survived its termination request`)
          await Bun.sleep(10)
        }
      }
    }
  })

  // The bundle is checked before the binary runs, so a refused one leaves no trace either.
  test.each([
    ['an omitted license text', () => licenseBundle({ texts: { [OWN]: null } }), /lacks/],
    ['a tampered license text', () => tampered(licenseBundle(), OWN), /differs from its inventory hash/],
    ['a stale license bundle', () => licenseBundle({ inventory: (inventory) => { inventory.inputs.cargoLock = { path: 'rust/Cargo.lock', sha256: sha('# an earlier lock\n') } } }), /generated from rust\/Cargo\.lock/],
  ])('refuses %s before running the binary, and keeps the earlier archive', async (_name, licenses, message) => {
    const marker = join(scratch(), 'ran')
    const { options, destination } = fixture({ body: `printf x > ${quote(marker)}; exit 99` })
    await refused({ ...options, licenses: licenses() }, destination, message)
    expect(existsSync(marker)).toBe(false)
  })

  test('refuses a NOTICE that differs from the one the license text reproduces, before running the binary', async () => {
    const marker = join(scratch(), 'ran')
    const { root, options, destination } = fixture({ body: `printf x > ${quote(marker)}; exit 99` })
    writeFileSync(join(root, 'crates/bake-ai/NOTICE'), `${NOTICE}edited\n`)
    await refused(options, destination, /reproduces a NOTICE of bake-ai other than .*crates\/bake-ai\/NOTICE/)
    expect(existsSync(marker)).toBe(false)
  })

  test('refuses an output link into the TypeScript release before running anything, and changes nothing there', async () => {
    const marker = join(scratch(), 'ran')
    const { root, options } = fixture({ body: `printf x > ${quote(marker)}; exit 99` })
    const legacy = join(root, '.artifacts/bake-release')
    mkdirSync(legacy, { recursive: true })
    // An archive of the same name already there: it must survive untouched.
    writeFileSync(join(legacy, archiveName(VERSION, target)), EARLIER)
    mkdirSync(join(root, '.artifacts/bake-rs-release'), { recursive: true })
    linkDir(legacy, options.outputDir)
    await expect(packRust(options)).rejects.toThrow(/inside the TypeScript release's \.artifacts\/bake-release\//)
    expect(readdirSync(legacy)).toEqual([archiveName(VERSION, target)])
    expect(readFileSync(join(legacy, archiveName(VERSION, target)), 'utf8')).toBe(EARLIER)
    expect(existsSync(marker)).toBe(false)
  })

  test('refuses a binary that is a symbolic link', async () => {
    const { root, options, destination } = fixture()
    const link = join(root, 'target/linked-bake-rs')
    symlinkSync(options.binary, link)
    await refused({ ...options, binary: link }, destination, /is a symbolic link/)
  })

  test('refuses a notice that is a symbolic link, or missing, or empty', async () => {
    const { root, options, destination } = fixture()
    symlinkSync(join(root, 'LICENSE'), join(root, 'linked-NOTICE'))
    await refused({ ...options, notices: { 'bake-ai': join(root, 'linked-NOTICE') } }, destination, /is a symbolic link/)
    await refused({ ...options, notices: { 'bake-ai': join(root, 'missing-NOTICE') } }, destination, /ENOENT/)
    writeFileSync(join(root, 'empty-NOTICE'), '')
    await refused({ ...options, notices: { 'bake-ai': join(root, 'empty-NOTICE') } }, destination, /is empty/)
  })

  test('refuses another platform, an unsupported one, and a malformed version before building anything', async () => {
    const { options, destination } = fixture()
    const foreign = host === 'linux-x64' ? 'linux-arm64' : 'linux-x64'
    await refused({ ...options, target: foreign }, destination, new RegExp(`a ${foreign} archive must be built and checked on ${foreign}`))
    await refused({ ...options, target: 'freebsd-x64' }, destination, /not a Bake release platform/)
    await refused({ ...options, version: '0.4.0+local' }, destination, /not a native release version/)
  })
})

// Junctions on Windows, which need no privilege; symbolic links elsewhere.
const linkDir = (target: string, path: string): void => symlinkSync(target, path, process.platform === 'win32' ? 'junction' : 'dir')

describe('the output directory', () => {
  /** A repository-shaped root with both TypeScript release outputs holding an archive, and the native output root. */
  function repository(): { root: string; legacy: string; host: string; native: string } {
    const root = scratch()
    const legacy = join(root, '.artifacts/bake-release/0.3.8')
    const host = join(root, 'distribution/host/public')
    const native = join(root, '.artifacts/bake-rs-release')
    for (const dir of [legacy, host, native]) mkdirSync(dir, { recursive: true })
    writeFileSync(join(legacy, 'bake-v0.3.8-linux-x64.tar.gz'), 'TypeScript archive\n')
    writeFileSync(join(host, 'latest.json'), '{"version":"0.3.8"}\n')
    return { root, legacy, host, native }
  }

  test('accepts the native output root and directories outside the repository, resolved', () => {
    const { root, native } = repository()
    expect(checkOutputDir(root, join(native, VERSION))).toBe(join(canonicalPath(native), VERSION))
    const external = scratch()
    expect(checkOutputDir(root, join(external, 'new', 'dir'))).toBe(join(canonicalPath(external), 'new', 'dir'))
    // A link from the native root to a directory outside the repository is legitimate.
    linkDir(external, join(native, 'elsewhere'))
    expect(checkOutputDir(root, join(native, 'elsewhere', VERSION))).toBe(join(canonicalPath(external), VERSION))
  })

  test('refuses the TypeScript outputs and the rest of the repository', () => {
    const { root, legacy, host } = repository()
    expect(() => checkOutputDir(root, legacy)).toThrow(/inside the TypeScript release's \.artifacts\/bake-release\//)
    expect(() => checkOutputDir(root, join(host, 'releases'))).toThrow(/inside the TypeScript release's distribution\/host\/public\//)
    expect(() => checkOutputDir(root, join(root, 'out'))).toThrow(/not under \.artifacts\/bake-rs-release\//)
  })

  test('refuses a link under the native root, or outside the repository, that leads into a TypeScript output', () => {
    const { root, legacy, host, native } = repository()
    linkDir(legacy, join(native, VERSION))
    expect(() => checkOutputDir(root, join(native, VERSION))).toThrow(/inside the TypeScript release's \.artifacts\/bake-release\//)
    expect(() => checkOutputDir(root, join(native, VERSION, 'deeper'))).toThrow(/bake-release/)
    const external = join(scratch(), 'public link')
    linkDir(host, external)
    expect(() => checkOutputDir(root, external)).toThrow(/inside the TypeScript release's distribution\/host\/public\//)
  })

  test('refuses a native root that is itself a link into a TypeScript output', () => {
    const { root, legacy, native } = repository()
    rmSync(native, { recursive: true })
    linkDir(legacy, native)
    expect(() => checkOutputDir(root, join(native, VERSION))).toThrow(/bake-release/)
  })

  test('refuses a dangling link, which mkdir would follow', () => {
    const { root, native } = repository()
    linkDir(join(root, '.artifacts/bake-release/0.3.9'), join(native, VERSION))
    expect(() => checkOutputDir(root, join(native, VERSION, 'x'))).toThrow(/link to a path that does not exist/)
  })
})

/** Tar `tree`'s top-level names into a gzipped archive under `root`. */
function tar(root: string, tree: string, names: string[]): string {
  const archive = join(root, 'test.tar.gz')
  // These cases vary payload entries and modes, independently of macOS metadata.
  const metadata = process.platform === 'darwin' ? ['--no-mac-metadata', '--no-xattrs', '--no-acls', '--no-fflags'] : []
  expect(Bun.spawnSync(['tar', ...metadata, '-czf', archive, '-C', tree, ...names]).exitCode).toBe(0)
  return archive
}

describe.skipIf(process.platform === 'win32')('reading an archive back', () => {
  const files = payload({ binary: 'unused', root: 'unused', notices: { 'bake-ai': 'unused' } }, 'linux-x64', LICENSES)

  /** A staged tree matching `files`, with the given binary mode. */
  function tree(root: string, binaryMode = 0o755): string {
    const stage = join(root, 'stage')
    const dirs = ['bin', 'licenses', 'licenses/linux-x64', 'notices', 'notices/bake-ai']
    for (const dir of dirs) mkdirSync(join(stage, dir), { recursive: true })
    for (const file of files) writeFileSync(join(stage, file.path), `${file.path}\n`)
    for (const dir of ['', ...dirs]) chmodSync(join(stage, dir), 0o755)
    for (const file of files) chmodSync(join(stage, file.path), file.mode)
    chmodSync(join(stage, 'bin/bake-rs'), binaryMode)
    return stage
  }
  const TOP = ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'bin', 'licenses', 'notices']

  test('accepts the payload as staged', async () => {
    const root = scratch()
    expect((await verifyArchive(tar(root, tree(root), TOP), files)).length).toBe(11)
  })

  test('refuses a truncated archive', async () => {
    const root = scratch()
    const archive = tar(root, tree(root), TOP)
    const bytes = readFileSync(archive)
    writeFileSync(archive, bytes.subarray(0, Math.floor(bytes.length / 2)))
    await expect(verifyArchive(archive, files)).rejects.toThrow(/not a readable gzipped tar archive/)
  })

  test('refuses a corrupted archive', async () => {
    const root = scratch()
    const archive = tar(root, tree(root), TOP)
    const bytes = readFileSync(archive)
    for (let index = 20; index < bytes.length - 8; index += 7) bytes[index] = (bytes[index] ?? 0) ^ 0xff
    writeFileSync(archive, bytes)
    await expect(verifyArchive(archive, files)).rejects.toThrow(/not a readable gzipped tar archive/)
  })

  test('refuses an extra file, a missing file, a link, and a binary that is not executable', async () => {
    let root = scratch()
    let stage = tree(root)
    writeFileSync(join(stage, 'bin/node'), 'node\n')
    await expect(verifyArchive(tar(root, stage, TOP), files)).rejects.toThrow(/holds bin\/node, which is not part of the package/)

    root = scratch()
    stage = tree(root)
    await expect(verifyArchive(tar(root, stage, ['bin', 'licenses', 'notices', 'LICENSE']), files)).rejects.toThrow(/lacks THIRD_PARTY_NOTICES\.md$/)

    // A platform's license text left out, with its directory and the inventory still there.
    root = scratch()
    stage = tree(root)
    rmSync(join(stage, 'licenses/linux-x64/THIRD-PARTY-LICENSES.txt'))
    await expect(verifyArchive(tar(root, stage, TOP), files)).rejects.toThrow(/lacks licenses\/linux-x64\/THIRD-PARTY-LICENSES\.txt$/)

    root = scratch()
    stage = tree(root)
    rmSync(join(stage, 'LICENSE'))
    symlinkSync('/etc/hostname', join(stage, 'LICENSE'))
    expect(lstatSync(join(stage, 'LICENSE')).isSymbolicLink()).toBe(true)
    await expect(verifyArchive(tar(root, stage, TOP), files)).rejects.toThrow(/LICENSE has tar type "2"/)

    root = scratch()
    await expect(verifyArchive(tar(root, tree(root, 0o644), TOP), files)).rejects.toThrow(/bin\/bake-rs is not executable/)

    root = scratch()
    await expect(verifyArchive(tar(root, tree(root, 0o775), TOP), files)).rejects.toThrow(/bin\/bake-rs is writable by group or others/)
  })

  test('refuses a payload path twice', async () => {
    const root = scratch()
    await expect(verifyArchive(tar(root, tree(root), [...TOP, 'LICENSE']), files)).rejects.toThrow(/holds LICENSE twice/)
  })
})
