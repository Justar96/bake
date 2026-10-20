/**
 * What a Bake 0.3 update does to a `bake` command the native installer took
 * over ([D45](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)).
 *
 * `addDshAlias` in `packages/boot/updater/src/install.ts` is the only part of
 * the frozen 0.3 updater that writes into the command directory, and
 * `installRelease` calls it on every update and rollback. These checks run
 * that shipped function against a directory where the native install owns
 * `bake`: it must leave that command alone, because the 0.3 updater only ever
 * adds a missing `dsh` beside a `bake` that still leads to its own release.
 * The frozen TypeScript source is not changed.
 */
import { afterEach, expect, test } from 'bun:test'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { addDshAlias } from '../../packages/boot/updater/src/install.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** A 0.3 install, a native install, and the command directory both reach. */
interface Coexisting {
  readonly legacyRoot: string
  readonly nativeRoot: string
  readonly binDir: string
  readonly legacyTarget: string
  readonly nativeTarget: string
}

function coexisting(): Coexisting {
  const directory = mkdtempSync(join(tmpdir(), 'bake-legacy-update-'))
  roots.push(directory)
  const legacyRoot = join(directory, 'share/bake')
  const legacyRelease = join(legacyRoot, 'versions/0.3.8-abcdef012345')
  mkdirSync(join(legacyRelease, 'bin'), { recursive: true })
  writeFileSync(join(legacyRelease, 'bin/bake'), '#!/bin/sh\nprintf \'bake 0.3.8\\n\'\n')
  chmodSync(join(legacyRelease, 'bin/bake'), 0o755)
  symlinkSync(legacyRelease, join(legacyRoot, 'current'))
  const nativeRoot = join(directory, 'share/bake-rs')
  const nativeRelease = join(nativeRoot, 'releases/0.4.0')
  mkdirSync(join(nativeRelease, 'bin'), { recursive: true })
  writeFileSync(join(nativeRelease, 'bin/bake'), '#!/bin/sh\nprintf \'bake 0.4.0\\n\'\n')
  chmodSync(join(nativeRelease, 'bin/bake'), 0o755)
  symlinkSync('releases/0.4.0', join(nativeRoot, 'current'))
  const binDir = join(directory, 'bin')
  mkdirSync(binDir, { recursive: true })
  return {
    legacyRoot, nativeRoot, binDir,
    legacyTarget: join(legacyRoot, 'current/bin/bake'),
    nativeTarget: join(nativeRoot, 'current/bin/bake'),
  }
}

test('a 0.3 update leaves a bake the native install took over, and adds no alias for it', async () => {
  const world = coexisting()
  symlinkSync(world.nativeTarget, join(world.binDir, 'bake'))
  symlinkSync(world.legacyTarget, join(world.binDir, 'dsh'))

  await addDshAlias('linux', world.legacyRoot, undefined, world.binDir)

  expect(readlinkSync(join(world.binDir, 'bake'))).toBe(world.nativeTarget)
  expect(readlinkSync(join(world.binDir, 'dsh'))).toBe(world.legacyTarget)
})

test('a 0.3 update adds no dsh beside a taken-over bake', async () => {
  const world = coexisting()
  symlinkSync(world.nativeTarget, join(world.binDir, 'bake'))

  await addDshAlias('linux', world.legacyRoot, undefined, world.binDir)

  // The alias is added only when `bake` still leads to the 0.3 release, so a
  // native takeover ends 0.3's claim on both names.
  expect(existsSync(join(world.binDir, 'dsh'))).toBe(false)
  expect(readlinkSync(join(world.binDir, 'bake'))).toBe(world.nativeTarget)
})

test('a 0.3 update still adds dsh beside its own bake, which the native install never made', async () => {
  const world = coexisting()
  symlinkSync(world.legacyTarget, join(world.binDir, 'bake'))

  await addDshAlias('linux', world.legacyRoot, undefined, world.binDir)

  expect(readlinkSync(join(world.binDir, 'dsh'))).toBe(world.legacyTarget)
})

test('a 0.3 update never rewrites a Windows bake.cmd, whoever wrote it', async () => {
  const world = coexisting()
  const shim = '@echo off\r\nset "BAKE_NATIVE_ROOT=C:\\native"\r\n'
  writeFileSync(join(world.binDir, 'bake.cmd'), shim)
  const launcher = join(world.binDir, 'dsh.cmd')

  await addDshAlias('win32', world.legacyRoot, launcher, undefined)

  // 0.3 writes each name only when it is missing (`wx`), so the native shim stays.
  expect(readFileSync(join(world.binDir, 'bake.cmd'), 'utf8')).toBe(shim)
  expect(readFileSync(launcher, 'utf8')).toContain('BAKE_RELEASE_ROOT')
})
