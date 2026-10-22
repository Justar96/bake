#!/usr/bin/env bun
/**
 * The oldest glibc a built Linux `bake` runs on:
 * `bun scripts/release/glibc-floor.ts <binary> [--floor GLIBC_2.34]`.
 *
 * The dynamic loader refuses to start a binary whose version needs
 * (`.gnu.version_r`) name a glibc symbol version the host's glibc lacks,
 * unless the need is weak: Rust's standard library binds newer functions
 * such as `pidfd_spawnp` weakly and falls back when they are missing. This
 * reads those needs with `readelf --dynamic --version-info`, reports the
 * highest non-weak `GLIBC_` version, which is the binary's real floor, and
 * the highest weak one, and fails when the floor is above `--floor`.
 *
 * It fails closed: readelf runs in the C locale, since its headings are
 * translated, and a dynamically linked binary whose glibc needs cannot be
 * read is an error, not a pass. Only a binary with no shared library to
 * load, a static one, needs no glibc version.
 *
 * The floor follows from the glibc the release is built against, not from
 * Bake's code, so a builder image change shows up here first. The default,
 * {@link DEFAULT_GLIBC_FLOOR}, is the floor the release owner chose (D50):
 * glibc 2.34, which RHEL 9 and its rebuilds and Amazon Linux 2023 ship.
 *
 * See distribution/README.md, "Native release".
 */

import { parseArgs } from 'node:util'

/** The highest glibc version a release binary may need unless `--floor` names another. */
export const DEFAULT_GLIBC_FLOOR = 'GLIBC_2.34'

/** One glibc symbol version a binary needs. */
export interface GlibcNeed {
  /** `GLIBC_<major>.<minor>[.<patch>]`. */
  readonly name: string
  /** Whether the loader starts the binary without it. */
  readonly weak: boolean
}

/** What a binary needs of glibc, highest first. */
export interface GlibcFloor {
  /** The highest version the binary cannot start without, or undefined when it needs none. */
  readonly required: string | undefined
  /** The highest weak version, when it is above {@link required}. */
  readonly weak: string | undefined
}

const GLIBC_VERSION = /^GLIBC_(\d+(?:\.\d+)*)$/u

/** Whether `value` names a glibc symbol version, such as `GLIBC_2.35`. */
export function isGlibcVersion(value: string): boolean {
  return GLIBC_VERSION.test(value)
}

/** Order two glibc symbol versions by their numbers: `GLIBC_2.4` before `GLIBC_2.17`. */
export function compareGlibc(left: string, right: string): number {
  const numbers = (value: string): number[] => {
    const match = GLIBC_VERSION.exec(value)
    if (match === null) throw new Error(`${JSON.stringify(value)} is not a glibc version such as ${DEFAULT_GLIBC_FLOOR}`)
    return (match[1] as string).split('.').map(Number)
  }
  const [a, b] = [numbers(left), numbers(right)]
  for (let index = 0; index < Math.max(a.length, b.length); index++) {
    const order = (a[index] ?? 0) - (b[index] ?? 0)
    if (order !== 0) return Math.sign(order)
  }
  return 0
}

/**
 * The glibc version needs in `readelf --dynamic --version-info` output, as
 * the C locale prints it: the `Name:` entries of its `.gnu.version_r`
 * section, from every glibc library the binary links (libc, libm, the
 * loader). The symbol-version and version-definition sections it also
 * prints are not needs and are skipped.
 *
 * A binary is dynamically linked when its dynamic section lists a `NEEDED`
 * shared library: those entries are what make the loader map libc at all,
 * whereas a static binary has no dynamic section and a static-pie one has
 * one without `NEEDED` entries. Program headers would not tell the two
 * apart as well: a static-pie binary carries `PT_DYNAMIC` too.
 *
 * @returns the needs, empty only for a binary that loads no shared library.
 * @throws when the output reports no dynamic section either way, which is
 * what translated or unexpected output looks like, or when a dynamically
 * linked binary shows no glibc version need.
 */
export function parseVersionNeeds(output: string): GlibcNeed[] {
  const lines = output.split('\n')
  if (!lines.some(line => /^(?:Dynamic section at offset |There is no dynamic section in this file\.)/u.test(line))) {
    throw new Error('readelf printed no dynamic section report, so whether the binary loads glibc is unknown')
  }
  const linked = lines.filter(line => /\(NEEDED\)\s+Shared library: \[/u.test(line)).length
  const needs: GlibcNeed[] = []
  let inNeeds = false
  for (const line of lines) {
    if (/^Version needs section /u.test(line)) {
      inNeeds = true
      continue
    }
    // Another section's heading starts in the first column.
    if (/^\S/u.test(line)) inNeeds = false
    if (!inNeeds) continue
    const need = /\bName: (\S+)\s+Flags: (.*?)\s+Version: \d+/u.exec(line)
    if (need === null || !isGlibcVersion(need[1] as string)) continue
    needs.push({ name: need[1] as string, weak: /\bWEAK\b/u.test(need[2] as string) })
  }
  if (linked > 0 && needs.length === 0) {
    throw new Error(`the binary loads ${String(linked)} shared ${linked === 1 ? 'library' : 'libraries'} but readelf shows no glibc version need, so its glibc floor is unknown`)
  }
  return needs
}

/** The highest required and weak glibc versions among `needs`. */
export function glibcFloor(needs: readonly GlibcNeed[]): GlibcFloor {
  const highest = (names: string[]): string | undefined => names.sort(compareGlibc).at(-1)
  const required = highest(needs.filter(need => !need.weak).map(need => need.name))
  const weak = highest(needs.filter(need => need.weak).map(need => need.name))
  return { required, weak: weak !== undefined && (required === undefined || compareGlibc(weak, required) > 0) ? weak : undefined }
}

/**
 * The report line for `floor` against the configured `limit`, and whether it passes.
 * @throws when `limit` is not a glibc version.
 */
export function checkGlibcFloor(binary: string, floor: GlibcFloor, limit: string): { ok: boolean; report: string } {
  if (!isGlibcVersion(limit)) throw new Error(`--floor ${JSON.stringify(limit)} is not a glibc version such as ${DEFAULT_GLIBC_FLOOR}`)
  const weak = floor.weak === undefined ? '' : `; weak references up to ${floor.weak} are used only where the host has them`
  if (floor.required === undefined && floor.weak === undefined) return { ok: true, report: `${binary} is statically linked and needs no glibc symbol version` }
  if (floor.required === undefined) return { ok: true, report: `${binary} needs no glibc symbol version it cannot start without${weak}` }
  const ok = compareGlibc(floor.required, limit) <= 0
  const verdict = ok ? `within the ${limit} floor` : `above the ${limit} floor, so it does not start on a host with glibc ${limit.slice('GLIBC_'.length)}`
  return { ok, report: `${binary} needs ${floor.required}, ${verdict}${weak}` }
}

/**
 * The environment readelf runs in: `env` in the C locale, so its headings
 * are the English ones {@link parseVersionNeeds} reads. `LANGUAGE` is
 * removed because gettext prefers it to `LC_ALL` outside the C locale.
 */
export function readelfEnvironment(env: Readonly<Record<string, string | undefined>>): Record<string, string> {
  const result: Record<string, string> = {}
  for (const [name, value] of Object.entries(env)) {
    if (value !== undefined && name !== 'LANGUAGE') result[name] = value
  }
  return { ...result, LC_ALL: 'C', LANG: 'C' }
}

/**
 * Read `binary`'s version needs with `readelf`.
 * @throws when readelf is missing, cannot read the file as ELF, or prints
 * output {@link parseVersionNeeds} refuses.
 */
export function readVersionNeeds(binary: string): GlibcNeed[] {
  let result: ReturnType<typeof Bun.spawnSync>
  try {
    result = Bun.spawnSync(['readelf', '--dynamic', '--version-info', '--wide', binary], {
      stdout: 'pipe', stderr: 'pipe', env: readelfEnvironment(process.env),
    })
  } catch {
    throw new Error('readelf, from binutils, is needed to read the binary\'s glibc version needs')
  }
  const stderr = result.stderr?.toString().trim() ?? ''
  if (result.exitCode !== 0 || stderr !== '') throw new Error(`readelf could not read ${binary}: ${stderr || `exit ${String(result.exitCode)}`}`)
  try {
    return parseVersionNeeds(result.stdout?.toString() ?? '')
  } catch (error) {
    throw new Error(`${binary}: ${error instanceof Error ? error.message : String(error)}`)
  }
}

function main(): number {
  const { values, positionals } = parseArgs({
    options: { floor: { type: 'string', default: DEFAULT_GLIBC_FLOOR } },
    allowPositionals: true, strict: true,
  })
  const [binary, ...extra] = positionals
  if (binary === undefined || extra.length > 0) throw new Error('Usage: bun scripts/release/glibc-floor.ts <binary> [--floor GLIBC_2.34]')
  const { ok, report } = checkGlibcFloor(binary, glibcFloor(readVersionNeeds(binary)), values.floor)
  console.log(report)
  return ok ? 0 : 1
}

if (import.meta.main) {
  try {
    process.exit(main())
  } catch (error) {
    console.error(`glibc-floor: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(1)
  }
}
