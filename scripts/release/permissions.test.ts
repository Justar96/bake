/** A release archive gives no other user write access to anything it unpacks. */
import { afterEach, describe, expect, test } from 'bun:test'
import { chmodSync, lstatSync, mkdirSync, mkdtempSync, readlinkSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { restrictSharedWrite, sharedWritableEntries, tarEntries } from './permissions.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function scratch(): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-release-permissions-'))
  roots.push(root)
  return root
}

/** Past tar's 100-byte name field, so each format stores it its own way. */
const LONG = `node_modules/${'a'.repeat(60)}/${'b'.repeat(60)}/cli.js`

/** A staged tree with what Bun's isolated linker leaves: a 0777 package bin, beside ordinary files. */
function stage(root: string): string {
  const tree = join(root, 'stage')
  mkdirSync(join(tree, 'bin'), { recursive: true })
  mkdirSync(join(tree, LONG, '..'), { recursive: true })
  writeFileSync(join(tree, 'bin/bake'), '#!/bin/sh\n')
  writeFileSync(join(tree, 'package.json'), '{}\n')
  writeFileSync(join(tree, 'notes.md'), 'notes\n')
  writeFileSync(join(tree, LONG), '#!/usr/bin/env node\n')
  symlinkSync('../bin/bake', join(tree, 'node_modules/bake'))
  // Every mode explicit, so the result does not depend on the umask.
  for (const directory of ['node_modules', join(LONG, '../..'), join(LONG, '..')]) chmodSync(join(tree, directory), 0o755)
  chmodSync(tree, 0o700)
  chmodSync(join(tree, 'bin'), 0o775)
  chmodSync(join(tree, 'bin/bake'), 0o755)
  chmodSync(join(tree, 'package.json'), 0o644)
  chmodSync(join(tree, 'notes.md'), 0o666)
  chmodSync(join(tree, LONG), 0o777)
  return tree
}

async function archive(tree: string, file: string, format?: string): Promise<string> {
  const child = Bun.spawn(['tar', ...format === undefined ? [] : [`--format=${format}`], '-czf', file, '-C', tree, '.'],
    { stdout: 'ignore', stderr: 'pipe' })
  const [code, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()])
  if (code !== 0) throw new Error(`tar exited ${code}: ${stderr}`)
  return file
}

const mode = (path: string): number => statSync(path).mode & 0o7777

describe.skipIf(process.platform === 'win32')('staged release permissions', () => {
  test('clear group and other write, keep execute bits, and leave symlinks alone', () => {
    const root = scratch()
    const tree = stage(root)
    // A link out of the stage must not carry the chmod to its target.
    const outside = join(root, 'outside.js')
    writeFileSync(outside, '')
    chmodSync(outside, 0o777)
    symlinkSync(outside, join(tree, 'escape'))

    expect(restrictSharedWrite(tree)).toEqual(['bin', LONG, 'notes.md'])
    expect(mode(join(tree, 'bin'))).toBe(0o755)
    expect(mode(join(tree, 'bin/bake'))).toBe(0o755)
    expect(mode(join(tree, LONG))).toBe(0o755)
    expect(mode(join(tree, 'notes.md'))).toBe(0o644)
    expect(mode(join(tree, 'package.json'))).toBe(0o644)
    expect(mode(tree)).toBe(0o700)
    expect(mode(outside)).toBe(0o777)
    expect(lstatSync(join(tree, 'node_modules/bake')).isSymbolicLink()).toBe(true)
    expect(readlinkSync(join(tree, 'node_modules/bake'))).toBe('../bin/bake')
    expect(restrictSharedWrite(tree)).toEqual([])
  })

  test('change nothing on Windows, whose tar invents the modes it records', () => {
    const tree = stage(scratch())
    expect(restrictSharedWrite(tree, 'win32')).toEqual([])
    expect(mode(join(tree, LONG))).toBe(0o777)
  })

  // The host tar's default (GNU long names or restricted pax), full pax, and a ustar prefix field.
  for (const format of [undefined, 'pax', 'ustar']) {
    test(`an archive check finds exactly the writable entries (${format ?? 'default'} format)`, async () => {
      const root = scratch()
      const tree = stage(root)
      const before = await sharedWritableEntries(await archive(tree, join(root, 'before.tar.gz'), format))
      expect(before.map(entry => [entry.path.replace(/^\.\//, ''), entry.type, entry.mode]).sort()).toEqual([
        ['bin/', '5', 0o775], [LONG, '0', 0o777], ['notes.md', '0', 0o666],
      ])

      restrictSharedWrite(tree)
      const after = join(root, 'after.tar.gz')
      expect(await sharedWritableEntries(await archive(tree, after, format))).toEqual([])
      const entries = new Map<string, { type: string; mode: number }>()
      for await (const entry of tarEntries(Bun.file(after).stream().pipeThrough(new DecompressionStream('gzip')))) {
        entries.set(entry.path.replace(/^\.\//, ''), entry)
      }
      expect(entries.get('bin/bake')).toMatchObject({ type: '0', mode: 0o755 })
      expect(entries.get(LONG)?.mode).toBe(0o755)
      expect(entries.get('node_modules/bake')?.type).toBe('2')
    })
  }
})

test('refuses bytes that are not a tar archive', async () => {
  const bytes = new Uint8Array(1024).fill(0x41)
  const entries = async (): Promise<void> => {
    for await (const _ of tarEntries((async function* () { yield bytes })())) { /* drain */ }
  }
  await expect(entries()).rejects.toThrow('Not a tar archive')
})
