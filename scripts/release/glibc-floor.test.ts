/** The glibc floor check: version needs read from `readelf`, weak needs set apart, and the floor enforced. */
import { describe, expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import { join, resolve } from 'node:path'
import {
  checkGlibcFloor, compareGlibc, DEFAULT_GLIBC_FLOOR, glibcFloor, parseVersionNeeds, readVersionNeeds,
} from './glibc-floor.ts'

const ROOT = resolve(import.meta.dir, '../..')

/**
 * `readelf --version-info --wide` of a release `bake` built on Fedora with
 * glibc 2.43, cut to its three sections: the symbol versions, which name
 * versions but are not needs, and the needs, which include a `GCC_` one and
 * weak glibc ones above the hard floor.
 */
const FEDORA_READELF = `
Version symbols section '.gnu.version' contains 6 entries:
 Addr: 0x0000000000001234  Offset: 0x00001234  Link: 7 (.dynsym)
  000:   0 (*local*)       0 (*local*)       2 (GLIBC_2.34)   21 (GLIBC_2.39)
  004:  12 (GLIBC_2.2.5)   4 (GCC_3.0)

Version needs section '.gnu.version_r' contains 4 entries:
 Addr: 0x0000000000001790  Offset: 0x00001790  Link: 8 (.dynstr)
  000000: Version: 1  File: libgcc_s.so.1  Cnt: 3
  0x0040:   Name: GCC_3.0  Flags: none  Version: 4
  0x0050:   Name: GCC_4.2.0  Flags: none  Version: 30
  0x0010: Version: 1  File: libm.so.6  Cnt: 2
  0x0070:   Name: GLIBC_2.2.5  Flags: none  Version: 12
  0x0090:   Name: GLIBC_2.29  Flags: none  Version: 13
  0x0020: Version: 1  File: libc.so.6  Cnt: 7
  0x00a0:   Name: GLIBC_2.2.5  Flags: none  Version: 3
  0x0160:   Name: GLIBC_2.17  Flags: none  Version: 20
  0x0170:   Name: GLIBC_2.18  Flags: WEAK  Version: 28
  0x01c0:   Name: GLIBC_2.32  Flags: none  Version: 25
  0x01d0:   Name: GLIBC_2.33  Flags: none  Version: 6
  0x01e0:   Name: GLIBC_2.34  Flags: none  Version: 2
  0x01f0:   Name: GLIBC_2.39  Flags: WEAK  Version: 21
  0x0030: Version: 1  File: ld-linux-x86-64.so.2  Cnt: 1
  0x0200:   Name: GLIBC_2.3  Flags: none  Version: 7
`

describe('glibc versions', () => {
  test('order by their numbers, not their text', () => {
    const ordered = ['GLIBC_2.2.5', 'GLIBC_2.3', 'GLIBC_2.3.4', 'GLIBC_2.4', 'GLIBC_2.17', 'GLIBC_2.34', 'GLIBC_2.35', 'GLIBC_3.0']
    for (const [index, left] of ordered.entries()) {
      expect(compareGlibc(left, left)).toBe(0)
      for (const right of ordered.slice(index + 1)) {
        expect(compareGlibc(left, right)).toBe(-1)
        expect(compareGlibc(right, left)).toBe(1)
      }
    }
    expect(compareGlibc('GLIBC_2.35', 'GLIBC_2.35.0')).toBe(0)
    expect(() => compareGlibc('2.35', 'GLIBC_2.35')).toThrow(/not a glibc version/)
  })
})

describe('reading version needs', () => {
  test('takes the glibc needs alone, with their weak flags', () => {
    const needs = parseVersionNeeds(FEDORA_READELF)
    expect(needs.map(need => need.name)).toEqual([
      'GLIBC_2.2.5', 'GLIBC_2.29', 'GLIBC_2.2.5', 'GLIBC_2.17', 'GLIBC_2.18', 'GLIBC_2.32', 'GLIBC_2.33', 'GLIBC_2.34', 'GLIBC_2.39', 'GLIBC_2.3',
    ])
    expect(needs.filter(need => need.weak).map(need => need.name)).toEqual(['GLIBC_2.18', 'GLIBC_2.39'])
  })

  test('the floor is the highest need that is not weak', () => {
    expect(glibcFloor(parseVersionNeeds(FEDORA_READELF))).toEqual({ required: 'GLIBC_2.34', weak: 'GLIBC_2.39' })
    // A weak need at or below the floor adds nothing to report.
    expect(glibcFloor([{ name: 'GLIBC_2.34', weak: false }, { name: 'GLIBC_2.18', weak: true }])).toEqual({ required: 'GLIBC_2.34', weak: undefined })
    expect(glibcFloor([])).toEqual({ required: undefined, weak: undefined })
    expect(parseVersionNeeds('\nNo version information found in this file.\n')).toEqual([])
  })
})

describe('the floor check', () => {
  const fedora = glibcFloor(parseVersionNeeds(FEDORA_READELF))

  test('passes at or above the binary\'s floor and fails below it', () => {
    expect(DEFAULT_GLIBC_FLOOR).toBe('GLIBC_2.35')
    expect(checkGlibcFloor('bake', fedora, DEFAULT_GLIBC_FLOOR)).toEqual({
      ok: true,
      report: 'bake needs GLIBC_2.34, within the GLIBC_2.35 floor; weak references up to GLIBC_2.39 are used only where the host has them',
    })
    expect(checkGlibcFloor('bake', fedora, 'GLIBC_2.34').ok).toBe(true)
    expect(checkGlibcFloor('bake', fedora, 'GLIBC_2.33')).toEqual({
      ok: false,
      report: 'bake needs GLIBC_2.34, above the GLIBC_2.33 floor, so it does not start on a host with glibc 2.33; weak references up to GLIBC_2.39 are used only where the host has them',
    })
  })

  test('a weak need above the floor does not fail it', () => {
    expect(checkGlibcFloor('bake', { required: 'GLIBC_2.17', weak: 'GLIBC_2.39' }, 'GLIBC_2.28').ok).toBe(true)
  })

  test('refuses a floor that is not a glibc version', () => {
    expect(() => checkGlibcFloor('bake', fedora, '2.35')).toThrow(/--floor "2\.35" is not a glibc version/)
  })
})

/** A `bake` this checkout has built, preferring the release build, or undefined. */
function builtBake(): string | undefined {
  return ['release', 'debug'].map(profile => join(ROOT, 'rust/target', profile, 'bake')).find(path => existsSync(path))
}

const linuxBake = process.platform === 'linux' ? builtBake() : undefined
if (linuxBake === undefined) {
  console.info('glibc-floor: no Linux rust/target/{release,debug}/bake is built here, so the built-binary checks are skipped')
}

describe.skipIf(linuxBake === undefined)('a built bake', () => {
  const binary = linuxBake as string

  function run(args: readonly string[]): { code: number; stdout: string; stderr: string } {
    const child = Bun.spawnSync(['bun', join(import.meta.dir, 'glibc-floor.ts'), binary, ...args], { stdout: 'pipe', stderr: 'pipe', timeout: 30_000 })
    return { code: child.exitCode, stdout: child.stdout.toString(), stderr: child.stderr.toString() }
  }

  test('passes at its own floor and fails at a floor one release below it', () => {
    const { required } = glibcFloor(readVersionNeeds(binary))
    if (required === undefined) throw new Error(`${binary} needs no glibc symbol version`)
    const atFloor = run(['--floor', required])
    expect(atFloor).toMatchObject({ code: 0, stderr: '' })
    expect(atFloor.stdout).toStartWith(`${binary} needs ${required}, within the ${required} floor`)
    const [major, minor] = required.slice('GLIBC_'.length).split('.').map(Number) as [number, number]
    const below = `GLIBC_${major}.${minor - 1}`
    const refused = run(['--floor', below])
    expect(refused.code).toBe(1)
    expect(refused.stdout).toContain(`needs ${required}, above the ${below} floor`)
  })
})
