/** The native license bundle reproduces each platform's crate texts verbatim and refuses incomplete or stale inputs. */
import { afterEach, describe, expect, spyOn, test } from 'bun:test'
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import {
  BUNDLE_FILE,
  buildBundle,
  type BundleInputs,
  checkArtifacts,
  collect,
  INVENTORY_FILE,
  LicenseBundleError,
  main,
  outputDir,
  parseHostSide,
  type PlatformId,
  PLATFORMS,
  type Runner,
  writeArtifacts,
} from './native-licenses.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

const CRATES_IO = 'registry+https://github.com/rust-lang/crates.io-index'
const ALL = PLATFORMS.map(platform => platform.id)
const MIT = (holder: string): string => `MIT License\n\nCopyright (c) ${holder}\n\nPermission is hereby granted, free of charge, to any person obtaining a copy.\n`
const APACHE = '                              Apache License\n                        Version 2.0, January 2004\n\n   END OF TERMS AND CONDITIONS\n'

const CONFIG = `accepted = ["MIT", "Apache-2.0", "Unicode-3.0"]
targets = [${PLATFORMS.map(platform => JSON.stringify(platform.triple)).join(', ')}]
ignore-dev-dependencies = true
ignore-build-dependencies = false
private = { ignore = true }
`

/** A third-party crate in the fixture registry. */
interface Crate {
  readonly name: string
  readonly version: string
  readonly license: string
  /** Files at the crate root, by path. */
  readonly files: Readonly<Record<string, string>>
  /** The license cargo-about chooses and the file it reports, by default the crate's only MIT or Apache file. */
  readonly chosen?: readonly { readonly id: string; readonly file: string | null; readonly text?: string }[]
  readonly platforms?: readonly PlatformId[]
  readonly role?: 'runtime' | 'build'
}

const crates = {
  memchr: { name: 'memchr', version: '2.8.3', license: 'Unlicense OR MIT', files: { 'LICENSE-MIT': MIT('2015 Andrew Gallant'), COPYING: 'This project is dual-licensed under the Unlicense and MIT licenses.\n' } },
  winapi: { name: 'winapi', version: '0.3.9', license: 'MIT/Apache-2.0', files: { 'LICENSE-MIT': MIT('2015-2018 The winapi-rs Developers').replaceAll('\n', '\r\n') }, platforms: ['win32-x64'] },
  derive: { name: 'serde_derive', version: '1.0.229', license: 'MIT OR Apache-2.0', files: { 'LICENSE-MIT': MIT('David Tolnay') }, role: 'build' },
  apache: { name: 'sync_wrapper', version: '1.0.2', license: 'Apache-2.0', files: { LICENSE: APACHE, NOTICE: 'sync_wrapper\nCopyright 2020 Actyx AG\n' } },
} satisfies Record<string, Crate>

interface Fixture {
  readonly root: string
  readonly inputs: BundleInputs
  /** Where a crate's sources are. */
  readonly dir: (crate: Crate) => string
}

/** A repository root with a Bake workspace, a registry of crate sources, and the tool outputs `collect` would gather. */
interface FixtureOptions {
  readonly config?: string
  /** Edits cargo-about's output before it is serialized. */
  readonly about?: (about: { crates: unknown[]; licenses: unknown[] }) => void
  /** Edits `cargo metadata`'s packages. */
  readonly metadata?: (packages: Record<string, unknown>[]) => void
  readonly lock?: string
  readonly upstream?: string
  /** Files under `rust/licenses/`, by path. */
  readonly vendored?: Readonly<Record<string, string>>
}

function fixture(list: readonly Crate[], options: FixtureOptions = {}): Fixture {
  const root = mkdtempSync(join(tmpdir(), 'bake-native-licenses-'))
  roots.push(root)
  const write = (path: string, text: string): void => {
    mkdirSync(dirname(path), { recursive: true })
    writeFileSync(path, text)
  }
  const config = options.config ?? CONFIG
  write(join(root, 'rust/Cargo.lock'), options.lock ?? '# locked\n')
  for (const [path, text] of Object.entries(options.vendored ?? {})) write(join(root, 'rust/licenses', path), text)
  write(join(root, 'rust/about.toml'), config)
  write(join(root, 'rust/rust-toolchain.toml'), '[toolchain]\nchannel = "1.99.0"\n')
  const workspace = ['bake-cli', 'bake-agent'].map(name => ({ name, dir: join(root, 'rust/crates', name) }))
  write(join(workspace[0]?.dir as string, 'Cargo.toml'), '')
  write(join(workspace[1]?.dir as string, 'NOTICE'), 'Portions of bake-agent are adapted from Pi.\nCopyright (c) 2025 Mario Zechner\n')
  const dir = (crate: Crate): string => join(root, 'registry', `${crate.name}-${crate.version}`)
  for (const crate of list) for (const [path, text] of Object.entries(crate.files)) write(join(dir(crate), path), text)

  const packages: Record<string, unknown>[] = [
    ...workspace.map(({ name, dir: at }) => ({ name, version: '0.4.0-dev', id: `path+file://${at}#${name}@0.4.0-dev`, source: null, license: 'MIT', manifest_path: join(at, 'Cargo.toml') })),
    ...list.map(crate => ({ name: crate.name, version: crate.version, id: `${CRATES_IO}#${crate.name}@${crate.version}`, source: CRATES_IO, license: crate.license, manifest_path: join(dir(crate), 'Cargo.toml') })),
  ]
  options.metadata?.(packages)
  const members = packages.filter(pkg => pkg.source === null).map(pkg => pkg.id as string)

  const head = workspace.map(({ name, dir: at }) => `${name} v0.4.0-dev (${at})`)
  const line = (crate: Crate): string => `${crate.name} v${crate.version}`
  // Linked crates per target; build-host crates appear once, in the all-targets tree.
  const trees = PLATFORMS.map(platform => ({
    id: platform.id,
    linked: [...head, ...list.filter(crate => crate.role !== 'build' && (crate.platforms ?? ALL).includes(platform.id)).map(line)].join('\n'),
  }))
  const runtime = list.filter(crate => crate.role !== 'build')
  const build = list.filter(crate => crate.role === 'build')
  const hostTree = [
    head[0] as string,
    ...[head[1] as string, ...runtime.map(line)].map(text => `├── ${text}`),
    ...build.length === 0 ? [] : ['[build-dependencies]', ...build.map(crate => `└── ${line(crate)}`)],
  ].join('\n')

  const licenses = new Map<string, { id: string; text: string; source_path: string | null; used_by: unknown[] }>()
  for (const crate of list) {
    const chosen = crate.chosen ?? [{ id: crate.license === 'Apache-2.0' ? 'Apache-2.0' : 'MIT', file: crate.license === 'Apache-2.0' ? 'LICENSE' : 'LICENSE-MIT' }]
    for (const choice of chosen) {
      const text = choice.text ?? (choice.file === null ? MIT('<year> <copyright holders>') : crate.files[choice.file] as string)
      const key = `${choice.id}\0${text}`
      const source = choice.file === null ? null : join(dir(crate), choice.file)
      const entry = licenses.get(key) ?? { id: choice.id, text, source_path: source, used_by: [] }
      entry.used_by.push({ crate: { name: crate.name, version: crate.version }, path: null })
      licenses.set(key, entry)
    }
  }
  const about = {
    crates: list.map(crate => ({ package: { name: crate.name, version: crate.version }, license: crate.license })),
    licenses: [...licenses.values()],
  }
  options.about?.(about)
  const inputs: BundleInputs = {
    root,
    cargoLock: readFileSync(join(root, 'rust/Cargo.lock')),
    config,
    toolchain: { channel: '1.99.0', cargo: 'cargo 1.99.0 (5f94df478 2026-08-27)' },
    cargoAbout: '0.9.2',
    metadata: { packages: packages as unknown as BundleInputs['metadata']['packages'], workspace_members: members },
    trees,
    hostTree,
    about: JSON.stringify(about),
    upstream: options.upstream ?? null,
  }
  return { root, inputs, dir }
}

function failure(run: () => unknown): string {
  try {
    run()
  } catch (error) {
    if (error instanceof LicenseBundleError) return error.message
    throw error
  }
  throw new Error('expected a LicenseBundleError')
}

interface Inventory {
  schema: string
  platforms: { id: string; file: string; sha256: string; crates: number }[]
  crates: {
    name: string
    version: string
    platforms: Record<string, string>
    texts: { id: string; file: string }[]
    notices: { file: string }[]
    clarified: boolean
  }[]
  workspace: { name: string; notice: { file: string } | null }[]
}

const realpathOf = (path: string): string => realpathSync.native(path)
const inventory = (files: Readonly<Record<string, string>>): Inventory => JSON.parse(files[INVENTORY_FILE] as string) as Inventory
const bundle = (files: Readonly<Record<string, string>>, platform: PlatformId): string => files[`${platform}/${BUNDLE_FILE}`] as string

describe('a generated bundle', () => {
  test('lists each platform\'s crates with their own texts, notices, and roles', () => {
    const { inputs } = fixture([crates.memchr, crates.winapi, crates.derive, crates.apache])
    const { files } = buildBundle(inputs)
    const linux = bundle(files, 'linux-x64')
    const windows = bundle(files, 'win32-x64')

    expect(linux).toContain('Copyright (c) 2015 Andrew Gallant')
    expect(linux).toContain('This project is dual-licensed under the Unlicense and MIT licenses.')
    expect(linux).toContain('Copyright 2020 Actyx AG')
    expect(linux).toContain('Portions of bake-agent are adapted from Pi.')
    expect(linux).toMatch(/^serde_derive 1\.0\.229 \| MIT OR Apache-2\.0 \| build \| MIT-/mu)
    expect(linux).toMatch(/^memchr 2\.8\.3 \| Unlicense OR MIT \| runtime \| MIT-[0-9a-f]{12} \| notices: COPYING$/mu)
    // A crate compiled for one target appears in that target's file alone, its CRLF text unchanged.
    expect(linux).not.toContain('winapi')
    expect(windows).toContain('winapi 0.3.9 | MIT/Apache-2.0 | runtime')
    expect(windows).toContain('Copyright (c) 2015-2018 The winapi-rs Developers\r\n')

    const record = inventory(files)
    expect(record.platforms.map(platform => [platform.id, platform.crates])).toEqual([['darwin-arm64', 3], ['darwin-x64', 3], ['linux-arm64', 3], ['linux-x64', 3], ['win32-x64', 4]])
    expect(record.crates.find(crate => crate.name === 'winapi')?.platforms).toEqual({ 'win32-x64': 'runtime' })
    expect(record.crates.find(crate => crate.name === 'sync_wrapper')?.texts.map(text => text.file)).toEqual(['LICENSE'])
    expect(record.workspace.map(member => [member.name, member.notice?.file ?? null])).toEqual([['bake-agent', 'rust/crates/bake-agent/NOTICE'], ['bake-cli', null]])
    expect(Object.keys(files).sort()).toEqual([INVENTORY_FILE, ...ALL.map(id => `${id}/${BUNDLE_FILE}`)].sort())
    // Output names files inside packages and the repository, never this machine's paths.
    for (const text of Object.values(files)) expect(text).not.toContain(inputs.root)
  })

  test('is byte-identical whatever order the tools list crates in', () => {
    const list = [crates.memchr, crates.winapi, crates.derive, crates.apache]
    const forward = buildBundle(fixture(list).inputs).files
    const shuffled = buildBundle(fixture([...list].reverse(), {
      about: (about) => { about.crates.reverse(); about.licenses.reverse() },
      metadata: packages => packages.reverse(),
    }).inputs).files
    expect(shuffled).toEqual(forward)
  })

  test('drops a crate removed from the lock, and the earlier output then fails the check', () => {
    const out = join(fixture([]).root, 'out')
    const before = buildBundle(fixture([crates.memchr, crates.apache]).inputs).files
    writeArtifacts(out, before)
    expect(checkArtifacts(out, before)).toEqual([])

    const after = buildBundle(fixture([crates.memchr]).inputs).files
    expect(bundle(after, 'linux-x64')).not.toContain('sync_wrapper')
    expect(bundle(after, 'linux-x64')).not.toContain('Actyx AG')
    expect(checkArtifacts(out, after)).toEqual([INVENTORY_FILE, ...ALL.map(id => `${id}/${BUNDLE_FILE}`)].sort().map(path => `${path} is out of date`))
  })
})

describe('a crate cargo-about over-reports', () => {
  test('is left out and listed when it is locked but no platform compiles it', () => {
    // cargo-about evaluates cfg() over all five targets at once; Cargo's per-target trees are the authority.
    const gnu = { name: 'windows_x86_64_gnu', version: '0.52.6' }
    const { inputs } = fixture([crates.winapi], {
      metadata: packages => packages.push({ ...gnu, id: `${CRATES_IO}#windows_x86_64_gnu@0.52.6`, source: CRATES_IO, license: 'MIT OR Apache-2.0', manifest_path: '/nowhere/Cargo.toml' }),
      about: (about) => {
        about.crates.push({ package: gnu, license: 'MIT OR Apache-2.0' })
        about.licenses.push({ id: 'MIT', text: MIT('Microsoft'), source_path: null, used_by: [{ crate: gnu, path: null }] })
      },
    })
    const { files } = buildBundle(inputs)
    expect((JSON.parse(files[INVENTORY_FILE] as string) as { excluded: string[] }).excluded).toEqual(['windows_x86_64_gnu@0.52.6'])
    expect(bundle(files, 'win32-x64')).not.toContain('windows_x86_64_gnu')
  })
})

describe('generation refuses', () => {
  test('a crate whose license text is not in its own source package', () => {
    const generic = { ...crates.memchr, files: { README: 'No license file.\n' }, chosen: [{ id: 'MIT', file: null }] }
    expect(failure(() => buildBundle(fixture([generic]).inputs))).toContain('memchr@2.8.3: no MIT license text was found in the source package')
  })

  test('a reported text that differs from the file it names', () => {
    const tampered = { ...crates.memchr, chosen: [{ id: 'MIT', file: 'LICENSE-MIT', text: MIT('someone else') }] }
    expect(failure(() => buildBundle(fixture([tampered]).inputs))).toContain('memchr@2.8.3: the MIT text cargo-about attached is not verbatim in any file of its source package')
  })

  test('a shared text attached to a crate whose package does not hold it', () => {
    const { inputs } = fixture([crates.memchr, crates.derive], {
      about: (about) => {
        const licenses = about.licenses as { used_by: unknown[] }[]
        licenses[0]?.used_by.push(...(licenses[1]?.used_by ?? []))
        licenses.splice(1, 1)
      },
    })
    expect(failure(() => buildBundle(inputs))).toContain('serde_derive@1.0.229: the MIT text cargo-about attached is not verbatim')
  })

  test('a compiled crate cargo-about did not report, or a reported crate outside the lock', () => {
    const missing = fixture([crates.memchr, crates.apache], { about: (about) => { about.crates.pop() } })
    expect(failure(() => buildBundle(missing.inputs))).toContain('sync_wrapper@1.0.2 is compiled into bake-rs but cargo-about did not report it')
    const extra = fixture([crates.memchr], { about: (about) => { about.crates.push({ package: { name: 'openssl', version: '0.10.0' }, license: 'Apache-2.0' }) } })
    expect(failure(() => buildBundle(extra.inputs))).toContain('cargo-about reported openssl@0.10.0, which is not in the locked dependency graph')
  })

  test('a crate that only a path or git source provides', () => {
    const { inputs } = fixture([crates.memchr], { metadata: (packages) => { (packages.at(-1) as Record<string, unknown>).source = 'git+https://example.com/memchr#abc' } })
    expect(failure(() => buildBundle(inputs))).toContain('memchr@2.8.3 comes from git+https://example.com/memchr#abc; only crates.io sources are supported')
  })
})

describe('upstream texts', () => {
  const COMMIT = 'f96eabaa11a06086e4fb779ed1c1688481067ab9'
  const UPSTREAM = `${APACHE}\n--- LLVM Exceptions to the Apache 2.0 License ----\n`
  const CHECKSUM = '0ec2ba2d4f3d'.padEnd(64, '0')
  const hash = (text: string): string => new Bun.CryptoHasher('sha256').update(text).digest('hex')
  const vcs = (sha1: string, path: string): string => JSON.stringify({ git: { sha1 }, path_in_vcs: path })
  // A published package with no license file, as cranelift-bitset 0.136.3 is; cargo-about reports only its generic text.
  const bitset: Crate = {
    name: 'cranelift-bitset',
    version: '0.136.3',
    license: 'Apache-2.0 WITH LLVM-exception',
    files: { '.cargo_vcs_info.json': vcs(COMMIT, 'cranelift/bitset'), 'Cargo.toml': '' },
    chosen: [{ id: 'Apache-2.0', file: null }],
  }
  const lock = (checksum = CHECKSUM): string => `version = 4\n\n[[package]]\nname = "cranelift-bitset"\nversion = "0.136.3"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${checksum}"\n`
  const entry = (fields: { sha256?: string; license?: string; crate?: string; pathInVcs?: string } = {}): string => [
    '[[text]]',
    'file = "upstream/wasmtime-v49.0.3/LICENSE"',
    `sha256 = "${fields.sha256 ?? hash(UPSTREAM)}"`,
    `license = "${fields.license ?? 'Apache-2.0 WITH LLVM-exception'}"`,
    'repository = "https://github.com/bytecodealliance/wasmtime"',
    `commit = "${COMMIT}"`,
    'path = "LICENSE"',
    `url = "https://raw.githubusercontent.com/bytecodealliance/wasmtime/${COMMIT}/LICENSE"`,
    `crates = [{ crate = "${fields.crate ?? 'cranelift-bitset@0.136.3'}", checksum = "${CHECKSUM}", path-in-vcs = "${fields.pathInVcs ?? 'cranelift/bitset'}" }]`,
    '',
  ].join('\n')
  const options = (overrides: Partial<FixtureOptions> = {}): FixtureOptions => ({ lock: lock(), upstream: entry(), vendored: { 'upstream/wasmtime-v49.0.3/LICENSE': UPSTREAM }, ...overrides })

  test('fill a crate whose published package omits the file, naming the commit it came from', () => {
    const { files } = buildBundle(fixture([bitset], options()).inputs)
    const text = bundle(files, 'linux-x64')
    expect(text).toContain('--- LLVM Exceptions to the Apache 2.0 License ----')
    expect(text).toContain(`Source: https://github.com/bytecodealliance/wasmtime at ${COMMIT}, LICENSE (the published crates omit it)`)
    expect(text).not.toContain('<copyright holders>')
    type Texts = { texts: { license: string; upstream?: { commit: string; file: string } }[] }
    const record = JSON.parse(files[INVENTORY_FILE] as string) as Texts & { inputs: Record<string, { path: string; sha256: string }> }
    expect(record.inputs.upstream).toEqual({ path: 'rust/licenses/upstream.toml', sha256: hash(entry()) })
    expect(record.inputs['rust/licenses/upstream/wasmtime-v49.0.3/LICENSE']).toEqual({ path: 'rust/licenses/upstream/wasmtime-v49.0.3/LICENSE', sha256: hash(UPSTREAM) })
    expect(record.texts[0]).toMatchObject({ license: 'Apache-2.0 WITH LLVM-exception', upstream: { commit: COMMIT, file: 'rust/licenses/upstream/wasmtime-v49.0.3/LICENSE' } })
  })

  test('are refused unless the vendored file, lock checksum, publishing commit, and declared license all match', () => {
    expect(failure(() => buildBundle(fixture([bitset], options({ vendored: { 'upstream/wasmtime-v49.0.3/LICENSE': `${UPSTREAM}edited\n` } })).inputs))).toContain('not the reviewed')
    expect(failure(() => buildBundle(fixture([bitset], options({ lock: lock('f'.repeat(64)) })).inputs))).toContain(`but Cargo.lock has ${'f'.repeat(64)}`)
    const otherCommit = { ...bitset, files: { ...bitset.files, '.cargo_vcs_info.json': vcs('0'.repeat(40), 'cranelift/bitset') } }
    expect(failure(() => buildBundle(fixture([otherCommit], options()).inputs))).toContain(`was published from ${'0'.repeat(40)}`)
    expect(failure(() => buildBundle(fixture([bitset], options({ upstream: entry({ pathInVcs: 'cranelift/entity' }) })).inputs))).toContain('not f96eabaa11a06086e4fb779ed1c1688481067ab9 at "cranelift/entity"')
    expect(failure(() => buildBundle(fixture([bitset], options({ upstream: entry({ license: 'Apache-2.0' }) })).inputs))).toContain('but it declares "Apache-2.0 WITH LLVM-exception"')
    expect(failure(() => buildBundle(fixture([bitset], options({ upstream: `${entry()}extra = 1\n` })).inputs))).toContain('needs file, sha256')
  })

  test('are refused once no compiled crate needs them', () => {
    expect(failure(() => buildBundle(fixture([crates.memchr], options()).inputs))).toContain('upstream.toml supplies cranelift-bitset@0.136.3, which no release platform compiles')
    const shipsLicense = { ...bitset, files: { ...bitset.files, LICENSE: UPSTREAM }, chosen: [{ id: 'Apache-2.0', file: 'LICENSE' }] }
    expect(failure(() => buildBundle(fixture([shipsLicense], options()).inputs))).toContain('whose own package now holds its license text')
  })

  test('cover only the crates they name', () => {
    const entity = { ...bitset, name: 'cranelift-entity', files: { ...bitset.files, '.cargo_vcs_info.json': vcs(COMMIT, 'cranelift/entity') } }
    expect(failure(() => buildBundle(fixture([bitset, entity], options()).inputs))).toContain('cranelift-entity@0.136.3: no Apache-2.0 license text was found')
  })
})

describe('clarifications', () => {
  const clarified = (checksum: string, license = 'Apache-2.0', name = 'sync_wrapper'): string => `${CONFIG}\n[${name}.clarify]\nlicense = "${license}"\n\n[[${name}.clarify.files]]\npath = "LICENSE"\nchecksum = "${checksum}"\n`
  const hash = (text: string): string => new Bun.CryptoHasher('sha256').update(text).digest('hex')

  test('are accepted for the one locked version whose file they hash, and marked in the inventory', () => {
    const { files } = buildBundle(fixture([crates.apache], { config: clarified(hash(APACHE)) }).inputs)
    expect(inventory(files).crates[0]?.clarified).toBe(true)
  })

  test('fail once the locked file no longer matches', () => {
    const changed = { ...crates.apache, files: { ...crates.apache.files, LICENSE: `${APACHE}\nAPPENDIX\n` } }
    expect(failure(() => buildBundle(fixture([changed], { config: clarified(hash(APACHE)) }).inputs))).toContain(`about.toml clarifies sync_wrapper@1.0.2 LICENSE at ${hash(APACHE)}, but the locked source has ${hash(`${APACHE}\nAPPENDIX\n`)}`)
  })

  test('fail for a crate no platform compiles', () => {
    expect(failure(() => buildBundle(fixture([crates.memchr], { config: clarified(hash(APACHE)) }).inputs))).toContain('about.toml clarifies sync_wrapper, which no release platform compiles; remove the stale clarification')
  })

  test('must match every locked version of the crate they name', () => {
    // cargo-about keys clarifications by name; a version whose file differs would fall back to the generic text.
    const same = { ...crates.apache, version: '0.1.2' }
    const both = inventory(buildBundle(fixture([crates.apache, same], { config: clarified(hash(APACHE)) }).inputs).files)
    expect(both.crates.map(crate => [crate.version, crate.clarified])).toEqual([['0.1.2', true], ['1.0.2', true]])
    const changed = { ...same, files: { ...same.files, LICENSE: `${APACHE}\nOLDER\n` } }
    expect(failure(() => buildBundle(fixture([crates.apache, changed], { config: clarified(hash(APACHE)) }).inputs))).toContain('about.toml clarifies sync_wrapper@0.1.2 LICENSE at')
  })

  test('cannot change the license a crate declares', () => {
    expect(failure(() => buildBundle(fixture([crates.apache], { config: clarified(hash(APACHE), 'MIT') }).inputs))).toContain('about.toml clarifies sync_wrapper@1.0.2 as "MIT", but it declares "Apache-2.0"')
  })

  test('and other broad settings are refused', () => {
    expect(failure(() => buildBundle(fixture([crates.memchr], { config: `${CONFIG}workarounds = ["ring"]\n` }).inputs))).toContain('`workarounds` is not a setting this bundle allows')
    expect(failure(() => buildBundle(fixture([crates.memchr], { config: `${CONFIG}\n[ring]\naccepted = ["OpenSSL"]\n` }).inputs))).toContain('`ring` is not a setting this bundle allows')
    expect(failure(() => buildBundle(fixture([crates.memchr], { config: CONFIG.replace('ignore-build-dependencies = false', 'ignore-build-dependencies = true') }).inputs))).toContain('must set `ignore-build-dependencies = false`')
    expect(failure(() => buildBundle(fixture([crates.memchr], { config: CONFIG.replace('"x86_64-pc-windows-msvc"', '"x86_64-pc-windows-gnu"') }).inputs))).toContain('`targets` must be exactly')
  })
})

describe('build-host crates', () => {
  // Captured from `cargo tree --target all -e normal,build --no-dedupe -f {p}` (cargo 1.99.0, Linux) over
  // a fixture whose proc macro and build dependency each take cfg(windows), cfg(target_os = "linux"), and
  // cfg(target_os = "macos") dependencies. `--target x86_64-pc-windows-msvc -e normal,build` on the same
  // Linux host listed lintool and tgtlin and omitted wintool and tgtwin: Cargo evaluates build-host cfg()
  // for the host it runs on, so a single host's per-target tree misses what a Windows runner builds.
  const ALL_TARGETS = [
    'app v0.1.0 (/work/app)',
    '├── mactool v0.1.0 (/work/mactool)',
    '└── pmacro v0.1.0 (proc-macro) (/work/pmacro)',
    '    ├── lintool v0.1.0 (/work/lintool)',
    '    ├── mactool v0.1.0 (/work/mactool)',
    '    └── wintool v0.1.0 (/work/wintool)',
    '[build-dependencies]',
    '├── helper v0.1.0 (/work/helper)',
    '│   ├── lintool v0.1.0 (/work/lintool)',
    '│   ├── mactool v0.1.0 (/work/mactool)',
    '│   └── wintool v0.1.0 (/work/wintool)',
    '├── tgtlin v0.1.0 (/work/tgtlin)',
    '└── tgtwin v0.1.0 (/work/tgtwin)',
  ].join('\n')

  test('are every crate under a proc macro or build dependency, for every host', () => {
    expect(parseHostSide(ALL_TARGETS, new Set(['app@0.1.0'])).map(entry => entry.name).sort()).toEqual(['helper', 'lintool', 'mactool', 'pmacro', 'tgtlin', 'tgtwin', 'wintool'])
  })

  test('leave out the build of a crate the platform does not link', () => {
    // wasm-bindgen is a wasm32-only dependency; its proc macro belongs to no release platform's build.
    const tree = ['bake-cli v0.4.0-dev (/r)', '├── tokio v1.53.1', '└── wasm-bindgen v0.2.129', '    └── wasm-bindgen-macro v0.2.129 (proc-macro)'].join('\n')
    expect(parseHostSide(tree, new Set(['bake-cli@0.4.0-dev', 'tokio@1.53.1']))).toEqual([])
    expect(parseHostSide(tree, new Set(['bake-cli@0.4.0-dev', 'wasm-bindgen@0.2.129'])).map(entry => entry.name)).toEqual(['wasm-bindgen-macro'])
  })

  test('follow a nested build-dependencies heading to its own parent only', () => {
    // The shape of num-traits' autocfg build dependency in bake-cli's tree.
    const nested = ['bake-cli v0.4.0-dev (/r)', '├── image v0.25.10', '│   ├── num-traits v0.2.19', '│   │   [build-dependencies]', '│   │   └── autocfg v1.5.1', '│   └── png v0.18.1', '└── tokio v1.53.1'].join('\n')
    const linked = new Set(['bake-cli@0.4.0-dev', 'image@0.25.10', 'num-traits@0.2.19', 'png@0.18.1', 'tokio@1.53.1'])
    expect(parseHostSide(nested, linked).map(entry => entry.name)).toEqual(['autocfg'])
    expect(() => parseHostSide('bake-cli v0.4.0-dev (/r)\n├── a v1\n│   │   └── b v1', linked)).toThrow(LicenseBundleError)
  })

  test('are listed on every platform, linked crates only where they link', () => {
    const windowsHostOnly = { ...crates.derive, platforms: ['win32-x64'] as PlatformId[] }
    const { files } = buildBundle(fixture([crates.winapi, windowsHostOnly]).inputs)
    expect(bundle(files, 'darwin-arm64')).toContain('serde_derive 1.0.229 | MIT OR Apache-2.0 | build')
    expect(bundle(files, 'darwin-arm64')).not.toContain('winapi')
  })
})

describe('the output directory', () => {
  test('check reports missing, extra, and malformed files', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const out = join(root, 'out')
    expect(checkArtifacts(out, files)).toEqual([`${out} does not exist; run with --write`])
    writeArtifacts(out, files)
    writeFileSync(join(out, 'stray.txt'), 'x')
    writeFileSync(join(out, INVENTORY_FILE), '{')
    rmSync(join(out, 'win32-x64', BUNDLE_FILE))
    expect(checkArtifacts(out, files)).toEqual([`${INVENTORY_FILE} is not JSON`, `${INVENTORY_FILE} is out of date`, `win32-x64/${BUNDLE_FILE} is missing`, 'stray.txt is not part of the bundle'])
  })

  test('write replaces an earlier bundle but not a directory of other files', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const other = join(root, 'other')
    mkdirSync(other)
    writeFileSync(join(other, 'keep.txt'), 'mine')
    expect(failure(() => writeArtifacts(other, files))).toContain('holds keep.txt, which no license bundle writes')
    expect(readFileSync(join(other, 'keep.txt'), 'utf8')).toBe('mine')
    const out = join(root, 'out')
    writeArtifacts(out, files)
    writeArtifacts(out, files)
    expect(checkArtifacts(out, files)).toEqual([])
    expect(readdirSync(root).filter(name => name.startsWith('.native-licenses-'))).toEqual([])
  })

  test('write keeps an earlier bundle, and anything added to it, when it refuses or fails', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const out = join(root, 'out')
    writeArtifacts(out, files)
    const before = readFileSync(join(out, 'linux-x64', BUNDLE_FILE))
    writeFileSync(join(out, 'linux-x64', 'notes.txt'), 'unrelated')
    expect(failure(() => writeArtifacts(out, files))).toContain('holds linux-x64/notes.txt, which no license bundle writes')
    expect(readFileSync(join(out, 'linux-x64', 'notes.txt'), 'utf8')).toBe('unrelated')
    rmSync(join(out, 'linux-x64', 'notes.txt'))
    // A file that cannot be staged fails before the earlier bundle is touched, and leaves no stage behind.
    expect(() => writeArtifacts(out, { ...files, 'bad\0name': 'x' })).toThrow()
    expect(readFileSync(join(out, 'linux-x64', BUNDLE_FILE))).toEqual(before)
    expect(checkArtifacts(out, files)).toEqual([])
    expect(readdirSync(root).filter(name => name.startsWith('.native-licenses-'))).toEqual([])
  })

  test('write moves the earlier bundle back when installing the new one fails', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const out = join(root, 'out')
    writeArtifacts(out, files)
    const before = readFileSync(join(out, 'linux-x64', BUNDLE_FILE))
    const moves: string[] = []
    const rename = (from: string, to: string): void => {
      moves.push(`${basename(from)} -> ${basename(to)}`)
      if (basename(from) === 'bundle') throw new Error('injected install failure')
      renameSync(from, to)
    }
    expect(() => writeArtifacts(out, files, { rename })).toThrow('injected install failure')
    expect(moves).toEqual(['out -> previous', 'bundle -> out', 'previous -> out'])
    expect(readFileSync(join(out, 'linux-x64', BUNDLE_FILE))).toEqual(before)
    expect(checkArtifacts(out, files)).toEqual([])
    expect(readdirSync(root).filter(name => name.startsWith('.native-licenses-'))).toEqual([])
  })

  test('write keeps and names the earlier bundle when it cannot be moved back', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const out = join(root, 'out')
    writeArtifacts(out, files)
    const before = readFileSync(join(out, 'linux-x64', BUNDLE_FILE))
    const rename = (from: string, to: string): void => {
      if (basename(to) === 'previous') {
        renameSync(from, to)
        // A file written into the bundle between the check and the move fails revalidation.
        writeFileSync(join(to, 'late.txt'), 'late')
        return
      }
      if (basename(from) === 'previous') {
        // Another writer recreated the output in the meantime.
        mkdirSync(out)
        writeFileSync(join(out, 'concurrent.txt'), 'other')
        throw new Error('injected restore failure')
      }
      renameSync(from, to)
    }
    const message = failure(() => writeArtifacts(out, files, { rename }))
    expect(message).toContain('holds late.txt, which no license bundle writes')
    expect(message).toContain('could not be moved back')
    const kept = /it is kept at (.+)$/mu.exec(message)?.[1] as string
    expect(basename(kept)).toBe('previous')
    expect(readFileSync(join(kept, 'linux-x64', BUNDLE_FILE))).toEqual(before)
    expect(readFileSync(join(kept, 'late.txt'), 'utf8')).toBe('late')
    expect(readdirSync(out)).toEqual(['concurrent.txt'])
    expect(readFileSync(join(out, 'concurrent.txt'), 'utf8')).toBe('other')
  })

  test.skipIf(process.platform === 'win32')('write refuses a link where the bundle or its directories should be', () => {
    const { root, inputs } = fixture([crates.memchr])
    const { files } = buildBundle(inputs)
    const target = join(root, 'elsewhere')
    mkdirSync(target)
    writeFileSync(join(target, 'keep.txt'), 'mine')
    const out = join(root, 'out')
    mkdirSync(out)
    symlinkSync(target, join(out, 'linux-x64'))
    expect(failure(() => writeArtifacts(out, files))).toContain('holds linux-x64, which no license bundle writes')
    expect(readFileSync(join(target, 'keep.txt'), 'utf8')).toBe('mine')
  })

  test('is never a TypeScript release route or the repository', () => {
    const root = fixture([]).root
    expect(outputDir(root, undefined)).toBe(join(realpathOf(root), '.artifacts/native/licenses'))
    for (const route of ['distribution/host/public', '.artifacts/bake-release/0.4.0', '.', '..', 'rust/out']) expect(() => outputDir(root, route)).toThrow(LicenseBundleError)
  })

  test.skipIf(process.platform === 'win32')('is resolved through links, refusing one into a release route or dangling', () => {
    const root = fixture([]).root
    mkdirSync(join(root, 'distribution/host/public'), { recursive: true })
    mkdirSync(join(root, '.artifacts/native'), { recursive: true })
    const outside = mkdtempSync(join(tmpdir(), 'bake-native-licenses-out-'))
    roots.push(outside)
    symlinkSync(join(root, 'distribution/host/public'), join(root, '.artifacts/native/public'))
    symlinkSync(join(root, 'distribution'), join(outside, 'dist'))
    symlinkSync(root, join(outside, 'repo'))
    symlinkSync(join(outside, 'missing'), join(outside, 'dangling'))
    expect(failure(() => outputDir(root, '.artifacts/native/public/licenses'))).toContain("inside the TypeScript release's distribution/")
    expect(failure(() => outputDir(root, join(outside, 'dist/licenses')))).toContain("inside the TypeScript release's distribution/")
    expect(failure(() => outputDir(root, join(outside, 'repo/rust/licenses')))).toContain('in the repository but not under .artifacts/native/')
    expect(failure(() => outputDir(root, join(outside, 'dangling/licenses')))).toContain('is a link to a path that does not exist')
    expect(outputDir(root, join(outside, 'repo/.artifacts/native/x'))).toBe(join(realpathOf(root), '.artifacts/native/x'))
  })
})

describe('the command', () => {
  /** A runner answering the fixture's tool calls and recording them. */
  function runner(inputs: BundleInputs, versions: { cargo?: string; about?: string } = {}): { run: Runner; calls: string[] } {
    const calls: string[] = []
    const run: Runner = (command, args) => {
      calls.push([command, ...args].join(' '))
      if (args[0] === '--version') return `${command === 'cargo' ? versions.cargo ?? inputs.toolchain.cargo : versions.about ?? 'cargo-about 0.9.2'}\n`
      if (args[0] === 'fetch') return ''
      if (args[0] === 'metadata') return JSON.stringify(inputs.metadata)
      if (args[0] === 'tree') {
        const triple = args[args.indexOf('--target') + 1]
        if (triple === 'all') return inputs.hostTree
        return inputs.trees.find(entry => PLATFORMS.find(platform => platform.id === entry.id)?.triple === triple)?.linked ?? ''
      }
      if (args[0] === 'about') {
        const output = args[args.indexOf('--output-file') + 1]
        if (args.indexOf('--output-file') < 0 || output === undefined) throw new Error('cargo-about requires --output-file under PowerShell')
        writeFileSync(output, inputs.about)
        return 'diagnostics are not license JSON'
      }
      throw new Error(`unexpected ${command} ${args.join(' ')}`)
    }
    return { run, calls }
  }

  function quiet<T>(run: () => T): T {
    const log = spyOn(console, 'log').mockImplementation(() => {})
    const error = spyOn(console, 'error').mockImplementation(() => {})
    try {
      return run()
    } finally {
      log.mockRestore()
      error.mockRestore()
    }
  }

  test('checks read-only and offline, and writes only after fetching the lock', () => {
    const { root, inputs } = fixture([crates.memchr, crates.winapi])
    const out = join(root, '.artifacts/native/licenses')
    const check = runner(inputs)
    expect(quiet(() => main(['--check'], root, check.run))).toBe(1)
    expect(existsSync(out)).toBe(false)
    expect(check.calls.some(call => call.startsWith('cargo fetch'))).toBe(false)
    expect(check.calls.filter(call => call.startsWith('cargo ') && call !== 'cargo --version').every(call => call.includes('--locked') && call.includes('--offline'))).toBe(true)
    expect(check.calls.find(call => call.includes('about generate'))).toContain('--frozen')
    // A user's `term.unicode = false` would otherwise print ASCII trees the parser refuses.
    expect(check.calls.filter(call => call.startsWith('cargo tree')).every(call => call.includes('--charset utf8'))).toBe(true)

    const write = runner(inputs)
    expect(quiet(() => main(['--write'], root, write.run))).toBe(0)
    expect(write.calls[0]).toBe('cargo fetch --locked')
    expect(quiet(() => main([], root, runner(inputs).run))).toBe(0)
  })

  test('refuses another cargo-about or Rust than the pinned ones', () => {
    const { root, inputs } = fixture([crates.memchr])
    const errors: string[] = []
    const error = spyOn(console, 'error').mockImplementation((message: string) => { errors.push(message) })
    try {
      expect(main(['--check'], root, runner(inputs, { about: 'cargo-about 0.8.4' }).run)).toBe(1)
      expect(main(['--check'], root, runner(inputs, { cargo: 'cargo 1.98.0 (aaaaaaaaa 2026-07-01)' }).run)).toBe(1)
      expect(main(['--bogus'], root, runner(inputs).run)).toBe(2)
    } finally {
      error.mockRestore()
    }
    expect(errors[0]).toContain('is "cargo-about 0.8.4"; the bundle is generated with cargo-about 0.9.2')
    expect(errors[1]).toContain('not the pinned Rust 1.99.0')
  })

  test('collects the output file and removes its private directory after success or failure', () => {
    const { root, inputs } = fixture([crates.memchr])
    const normal = runner(inputs)
    let output: string | undefined
    const capture: Runner = (command, args, cwd) => {
      if (args[0] === 'about') output = args[args.indexOf('--output-file') + 1]
      return normal.run(command, args, cwd)
    }
    expect(collect(root, capture).about).toBe(inputs.about)
    expect(output).toBeDefined()
    expect(existsSync(dirname(output!))).toBe(false)

    const interrupted: Runner = (command, args, cwd) => {
      if (args[0] === 'about') {
        output = args[args.indexOf('--output-file') + 1]
        normal.run(command, args, cwd)
        throw new Error('collector failed after writing')
      }
      return normal.run(command, args, cwd)
    }
    expect(() => collect(root, interrupted)).toThrow('collector failed after writing')
    expect(existsSync(dirname(output!))).toBe(false)
  })
})
