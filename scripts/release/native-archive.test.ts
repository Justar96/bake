/** Each publication route reads an archive's layout, so a rename cannot route a native archive to 0.3 updaters or the reverse. */
import { afterEach, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { assertNativeArchive, assertTypeScriptArchive, readArchiveLayout } from './native-archive.ts'
import { archiveName } from './pack-rust.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-native-archive-'))
  roots.push(root)
  return root
}

/** Stage `files` and archive the staged tree as `name`. */
function archive(files: Record<string, string>, name = archiveName('0.4.0', 'linux-x64'), link?: { path: string; target: string }): string {
  const root = scratch()
  const stage = join(root, 'stage')
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(join(stage, path, '..'), { recursive: true })
    writeFileSync(join(stage, path), contents)
  }
  if (link !== undefined) symlinkSync(link.target, join(stage, link.path))
  const metadata = process.platform === 'darwin' ? ['--no-mac-metadata', '--no-xattrs', '--no-acls', '--no-fflags'] : []
  const path = join(root, name)
  const top = Object.keys(files).map(file => file.split('/')[0] as string)
  expect(Bun.spawnSync(['tar', ...metadata, '-czf', path, '-C', stage, ...new Set(top)]).exitCode).toBe(0)
  return path
}

/** A native archive's payload, apart from its executable. */
const NATIVE_PAYLOAD = {
  LICENSE: 'MIT\n',
  'THIRD_PARTY_NOTICES.md': 'notices\n',
  'notices/bake-ai/NOTICE': 'Pi, MIT\n',
  'licenses/inventory.json': '{"schema":"bake-native-licenses/1"}\n',
  'licenses/linux-x64/THIRD-PARTY-LICENSES.txt': 'texts\n',
}

/** A complete native archive, as `release:pack:rust` writes it today. */
const NATIVE = { ...NATIVE_PAYLOAD, 'bin/bake': 'native binary\n' }

const TYPESCRIPT = {
  'bin/bake': '#!/usr/bin/env node\n',
  'bin/bake.cmd': '@echo off\n',
  'package.json': '{"name":"bake"}\n',
  'apps/cli/lib/bin.js': 'console.log("bake")\n',
  'node_modules/bake-cli/package.json': '{"name":"bake-cli"}\n',
}

test('a native archive is native under either platform name for its executable', async () => {
  for (const binary of ['bin/bake', 'bin/bake.exe']) {
    const layout = await readArchiveLayout(archive({ ...NATIVE_PAYLOAD, [binary]: 'native binary\n' }))
    expect(layout.kind).toBe('native')
    expect(() => { assertNativeArchive(layout) }).not.toThrow()
  }
})

test('a 0.3 archive is a TypeScript archive, and its bin/bake is no native executable', async () => {
  const layout = await readArchiveLayout(archive(TYPESCRIPT, 'bake-v0.3.8-linux-x64.tar.gz'))
  expect(layout.kind).toBe('typescript')
  expect(layout.typescriptMarkers).toContain('apps/cli/lib/bin.js')
  expect(() => { assertTypeScriptArchive('bake-v0.3.8-linux-x64.tar.gz', layout) }).not.toThrow()
  expect(() => { assertNativeArchive(layout) }).toThrow(/holds apps\/cli\/lib\/bin\.js of a TypeScript release/)
})

test('the 0.3 route refuses a native archive renamed as its own', async () => {
  const layout = await readArchiveLayout(archive(NATIVE, 'bake-v0.4.0-linux-x64.tar.gz'))
  expect(() => { assertTypeScriptArchive('bake-v0.4.0-linux-x64.tar.gz', layout) })
    .toThrow(/holds bin\/bake with licenses\/inventory\.json; a native archive cannot be published to 0\.3 updaters/)
})

test('the native route refuses a 0.3 archive named as a native one', async () => {
  // Both routes name an archive bake-v<version>-<target>.tar.gz, so only the
  // layout tells them apart.
  expect(archiveName('0.4.0', 'linux-x64')).toBe('bake-v0.4.0-linux-x64.tar.gz')
  const layout = await readArchiveLayout(archive(TYPESCRIPT, archiveName('0.4.0', 'linux-x64')))
  expect(layout.kind).toBe('typescript')
  expect(() => { assertNativeArchive(layout) }).toThrow(/holds apps\/cli\/lib\/bin\.js of a TypeScript release/)
})

test('neither route takes an archive of another shape', async () => {
  const layout = await readArchiveLayout(archive({ 'README.md': 'nothing here\n' }))
  expect(layout.kind).toBe('unknown')
  expect(() => { assertNativeArchive(layout) }).toThrow(/is not a native archive/)
  expect(() => { assertTypeScriptArchive('bake-v0.3.8-linux-x64.tar.gz', layout) }).toThrow(/is not a TypeScript release archive/)
})

test('the native route refuses an archive holding a link', async () => {
  const layout = await readArchiveLayout(archive(NATIVE, archiveName('0.4.0', 'linux-x64'), { path: 'bin/bake-link', target: '../../../etc/passwd' }))
  expect(layout.refusedEntries).toEqual(['bin/bake-link (tar type 2)'])
  expect(() => { assertNativeArchive(layout) }).toThrow(/holds bin\/bake-link \(tar type 2\), which no release archive may carry/)
})

test('an unreadable archive is refused, not parsed', async () => {
  const root = scratch()
  const path = join(root, archiveName('0.4.0', 'linux-x64'))
  writeFileSync(path, 'not a gzip stream')
  await expect(readArchiveLayout(path)).rejects.toThrow(/is not a readable gzipped tar archive/)
})
