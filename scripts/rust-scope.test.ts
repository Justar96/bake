import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'
import { describe, expect, it } from 'bun:test'
import {
  describeRustScope, isRustInput, parseRustWorkspace, RUST_EXTERNAL_INPUTS, RUST_SCOPE_RUNNERS, rustScope, rustScopeFilter,
  type RustPackage,
} from './rust-scope.ts'

const ROOT = resolve(import.meta.dirname, '..')
const RUST = join(ROOT, 'rust')

// A graph shaped like the workspace: view <- tui <- cli, and a leaf the others do not reach.
const graph: RustPackage[] = [
  { name: 'bake-tui-view', dir: 'rust/crates/bake-tui-view', dependencies: [] },
  { name: 'bake-process', dir: 'rust/crates/bake-process', dependencies: [] },
  { name: 'bake-coding-agent', dir: 'rust/crates/bake-coding-agent', dependencies: ['bake-process'] },
  { name: 'bake-tui', dir: 'rust/crates/bake-tui', dependencies: ['bake-coding-agent', 'bake-tui-view'] },
  // A dev-dependency edge counts like any other.
  { name: 'bake-cli', dir: 'rust/crates/bake-cli', dependencies: ['bake-process', 'bake-tui'] },
  { name: 'bake-update', dir: 'rust/crates/bake-update', dependencies: [] },
  { name: 'bake-plugin-sdk', dir: 'rust/crates/bake-plugin-sdk', dependencies: [] },
  { name: 'bake-plugin-fixture-hostile', dir: 'rust/plugins/fixtures/hostile', dependencies: ['bake-plugin-sdk'] },
]

const packagesOf = (files: readonly string[]): readonly string[] | string => {
  const scope = rustScope(files, graph)
  return scope.kind === 'packages' ? scope.packages : scope.kind
}

describe('Rust change scope', () => {
  it('selects a changed package and every transitive reverse dependent', () => {
    expect(packagesOf(['rust/crates/bake-tui-view/src/lib.rs'])).toEqual(['bake-cli', 'bake-tui', 'bake-tui-view'])
    expect(packagesOf(['rust/crates/bake-process/tests/x.rs'])).toEqual(['bake-cli', 'bake-coding-agent', 'bake-process', 'bake-tui'])
    expect(packagesOf(['rust/crates/bake-cli/docs/cli.md'])).toEqual(['bake-cli'])
    expect(packagesOf(['rust/plugins/fixtures/hostile/src/lib.rs'])).toEqual(['bake-cli', 'bake-plugin-fixture-hostile'])
  })

  it('names why each package was selected', () => {
    const scope = rustScope(['docs/a.md', 'rust/crates/bake-tui-view/src/lib.rs', 'rust/crates/bake-tui-view/src/b.rs'], graph)
    expect(scope.kind === 'packages' ? [...scope.changed] : []).toEqual([['bake-tui-view', 'rust/crates/bake-tui-view/src/lib.rs']])
    expect(describeRustScope(scope)).toEqual([
      '3 package(s): bake-cli, bake-tui, bake-tui-view',
      '  bake-tui-view: rust/crates/bake-tui-view/src/lib.rs',
      '  reverse dependents: bake-cli, bake-tui',
    ])
  })

  it('widens to the full suite for workspace-wide inputs and its own runners', () => {
    for (const wide of ['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml', 'rust/.config/nextest.toml',
      'rust/test-support/harness.rs', 'rust/about.toml', 'rust/README.md', 'rust/plugins/build-fixtures.sh',
      'rust/crates/bake-update/Cargo.toml', 'rust/plugins/fixtures/hostile/Cargo.toml', ...RUST_SCOPE_RUNNERS]) {
      const scope = rustScope(['rust/crates/bake-tui-view/src/lib.rs', wide], graph)
      expect(scope.kind).toBe('full')
      expect(scope.kind === 'full' ? scope.reason : '').toContain(wide)
    }
  })

  it('selects nothing for paths no Rust package reads', () => {
    for (const files of [[], ['docs/architecture.md'], ['packages/core/agent/src/index.ts', 'scripts/preflight.ts', 'package.json']]) {
      expect(rustScope(files, graph)).toEqual({ kind: 'none' })
      expect(files.some(isRustInput)).toBe(false)
    }
  })

  it('selects the readers of a non-Cargo input outside their package', () => {
    expect(packagesOf(['CHANGELOG.md'])).toEqual(['bake-cli', 'bake-tui'])
    expect(packagesOf(['distribution/host/native-release-key.pub'])).toEqual(['bake-update'])
    expect(packagesOf(['rust/crates/bake-plugin-sdk/wit/plugin.wit']))
      .toEqual(['bake-cli', 'bake-coding-agent', 'bake-plugin-fixture-hostile', 'bake-plugin-sdk', 'bake-tui'])
    expect(packagesOf(['distribution/host/install.sh'])).toBe('none')
    for (const input of RUST_EXTERNAL_INPUTS) expect(isRustInput(input.path)).toBe(true)
  })

  it('filters nextest by each changed package and its reverse dependents', () => {
    expect(rustScopeFilter(['bake-tui-view', 'bake-ai'])).toBe('rdeps(=bake-ai) | rdeps(=bake-tui-view)')
  })

  it('reads members and intra-workspace edges of every kind from cargo metadata', () => {
    const at = (dir: string) => join(ROOT, dir)
    const metadata = JSON.stringify({
      packages: [
        { name: 'a', manifest_path: at('rust/crates/a/Cargo.toml'), dependencies: [
          { name: 'b', kind: null, path: at('rust/crates/b') },
          { name: 'c', kind: 'dev', path: at('rust/crates/c') },
          { name: 'serde', kind: null },
        ] },
        { name: 'b', manifest_path: at('rust/crates/b/Cargo.toml'), dependencies: [{ name: 'c', kind: 'build', path: at('rust/crates/c') }] },
        { name: 'c', manifest_path: at('rust/crates/c/Cargo.toml'), dependencies: [] },
      ],
    })
    expect(parseRustWorkspace(metadata, ROOT)).toEqual([
      { name: 'a', dir: 'rust/crates/a', dependencies: ['b', 'c'] },
      { name: 'b', dir: 'rust/crates/b', dependencies: ['c'] },
      { name: 'c', dir: 'rust/crates/c', dependencies: [] },
    ])
    expect(() => parseRustWorkspace('{"packages":{}}', ROOT)).toThrow(/packages is not an array/u)
    expect(() => parseRustWorkspace(JSON.stringify({ packages: [{ name: 'x', manifest_path: '/elsewhere/Cargo.toml', dependencies: [] }] }), ROOT))
      .toThrow(/outside/u)
  })
})

/** Members named by `rust/Cargo.toml`, read as text so this test needs no Rust toolchain. */
function workspaceMembers(): { name: string; dir: string }[] {
  const manifest = readFileSync(join(RUST, 'Cargo.toml'), 'utf8')
  const block = /^members\s*=\s*\[([^\]]*)\]/mu.exec(manifest)?.[1] ?? ''
  return [...block.matchAll(/"([^"]+)"/gu)].map(([, dir]) => {
    const own = readFileSync(join(RUST, dir ?? '', 'Cargo.toml'), 'utf8')
    const name = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/msu.exec(own)?.[1]
    if (name === undefined) throw new Error(`no package name in rust/${dir}/Cargo.toml`)
    return { name, dir: `rust/${dir}` }
  })
}

function rustSources(directory: string): string[] {
  return readdirSync(directory, { recursive: true, encoding: 'utf8' })
    .map(path => path.replaceAll('\\', '/'))
    .filter(path => path.endsWith('.rs') && !path.split('/').includes('target'))
    .map(path => join(directory, path))
}

interface Reference { readonly consumer: string; readonly source: string; readonly literal: string; readonly target: string }

/** String literals in Rust source with their offsets, skipping comments and character literals. */
function stringLiterals(code: string): { value: string; index: number }[] {
  const found: { value: string; index: number }[] = []
  let at = 0
  while (at < code.length) {
    const char = code[at]
    if (code.startsWith('//', at)) {
      const end = code.indexOf('\n', at)
      at = end === -1 ? code.length : end + 1
    } else if (code.startsWith('/*', at)) {
      const end = code.indexOf('*/', at + 2)
      at = end === -1 ? code.length : end + 2
    } else if (char === '\'') {
      // A character literal ('x', '\'', '"'), or a lifetime, which has no closing quote.
      if (code[at + 1] === '\\') at = code.indexOf('\'', at + 3) + 1 || code.length
      else {
        const next = code.codePointAt(at + 1) ?? 0
        const width = next > 0xffff ? 2 : 1
        at += code[at + 1 + width] === '\'' ? 2 + width : 1
      }
    } else if (/[rb]/u.test(char ?? '') && /^(?:br|r)#*"/u.test(code.slice(at, at + 260)) && !/\w/u.test(code[at - 1] ?? '')) {
      const opening = /^(?:br|r)(#*)"/u.exec(code.slice(at, at + 260))
      const hashes = opening?.[1] ?? ''
      const start = at + (opening?.[0].length ?? 0)
      const end = code.indexOf(`"${hashes}`, start)
      const stop = end === -1 ? code.length : end
      found.push({ value: code.slice(start, stop), index: at })
      at = stop + 1 + hashes.length
    } else if (char === '"') {
      let end = at + 1
      while (end < code.length && code[end] !== '"') end += code[end] === '\\' ? 2 : 1
      found.push({ value: code.slice(at + 1, end), index: at })
      at = end + 1
    } else {
      at += 1
    }
  }
  return found
}

const ANCHORED = /(?:include_str!|include_bytes!)\s*\(\s*$|#\[path\s*=\s*$/u

/**
 * String literals in a member's sources that name an existing path outside that member:
 * `include_str!`, `include_bytes!`, and `#[path]` resolve from the source file; other `../`
 * literals from the package directory, where Cargo runs tests and `CARGO_MANIFEST_DIR` points;
 * and slash-separated literals from `rust/`, the base the tests' `rust_dir()` helpers join onto.
 */
function externalReferences(): Reference[] {
  const members = workspaceMembers()
  const memberAt = (path: string) => members.find(member => path === member.dir || path.startsWith(`${member.dir}/`))
  const references: Reference[] = []
  for (const member of members) {
    const own = join(ROOT, member.dir)
    for (const source of rustSources(own)) {
      const code = readFileSync(source, 'utf8')
      for (const { value: literal, index } of stringLiterals(code)) {
        const before = code.slice(Math.max(0, index - 40), index)
        const candidates = ANCHORED.test(before) ? [resolve(dirname(source), literal)]
          : literal.includes('../') ? [resolve(own, literal)]
            : /^[\w.-]+\/[\w./-]+$/u.test(literal) ? [resolve(RUST, literal)] : []
        for (const absolute of candidates) {
          const target = relative(ROOT, absolute).replaceAll('\\', '/')
          // The member's own files, and its ancestors (`..`, `../..`) as join bases, are not inputs.
          if (target === '' || target.startsWith('../') || !existsSync(absolute)) continue
          if (target === member.dir || target.startsWith(`${member.dir}/`) || member.dir.startsWith(`${target}/`)) continue
          // A slash-separated literal only counts when it lands in another member.
          if (!ANCHORED.test(before) && !literal.includes('../') && memberAt(target) === undefined) continue
          references.push({ consumer: member.name, source: relative(ROOT, source).replaceAll('\\', '/'), literal, target })
        }
      }
    }
  }
  return references
}

describe('Rust external input table', () => {
  const members = workspaceMembers()
  const references = externalReferences()

  it('finds the cross-package reads it is meant to guard', () => {
    const targets = new Set(references.map(reference => `${reference.consumer} ${reference.target}`))
    for (const known of ['bake-tui CHANGELOG.md', 'bake-update distribution/host/native-release-key.pub',
      'bake-cli rust/crates/bake-plugin-sdk/wit/plugin.wit', 'bake-coding-agent rust/crates/bake-plugin-sdk/wit',
      'bake-cli rust/plugins/fixtures/hostile/hostile.wasm', 'bake-cli rust/test-support/harness.rs']) {
      expect(targets).toContain(known)
    }
  })

  it('names every path a Rust package reads outside itself, or widens it to the full suite', () => {
    const memberAt = (path: string) => members.find(member => path.startsWith(`${member.dir}/`) || path === member.dir)
    const missing = references.filter((reference) => {
      // Under rust/ but in no member: such a change already runs every test.
      if (reference.target.startsWith('rust/') && memberAt(reference.target) === undefined) return false
      return !RUST_EXTERNAL_INPUTS.some(input => input.consumers.includes(reference.consumer)
        && (input.path.endsWith('/') ? `${reference.target}/`.startsWith(input.path) : reference.target === input.path))
    }).map(reference => `${reference.source}: "${reference.literal}" -> ${reference.target} (add it to RUST_EXTERNAL_INPUTS for ${reference.consumer})`)
    expect(missing).toEqual([])
  })

  it('keeps every table entry on a real path, read by real members', () => {
    const names = new Set(members.map(member => member.name))
    for (const input of RUST_EXTERNAL_INPUTS) {
      expect(existsSync(join(ROOT, input.path))).toBe(true)
      for (const consumer of input.consumers) expect(names).toContain(consumer)
      expect(references.some(reference => input.consumers.includes(reference.consumer)
        && `${reference.target}/`.startsWith(input.path.endsWith('/') ? input.path : `${input.path}/`))).toBe(true)
    }
  })
})

describe('Rust string literal scan', () => {
  it('skips comments and character literals and reads raw strings', () => {
    const code = [
      'let q = \'"\'; let e = \'\\\'\'; fn f<\'a>(x: &\'a str) {}',
      '// include_str!("../commented.md")',
      '/* "../block" */ let s = include_str!("../real.md");',
      'let r = r#"raw "quoted" ../x"#; let b = b"bytes";',
    ].join('\n')
    expect(stringLiterals(code).map(literal => literal.value)).toEqual(['../real.md', 'raw "quoted" ../x', 'bytes'])
  })
})
