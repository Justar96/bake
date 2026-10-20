/**
 * Tell a native archive from a 0.3 TypeScript archive, so neither publication
 * route can be handed the other's bytes.
 *
 * `assemble-rust.ts` publishes native archives only, and `assemble.ts`
 * publishes TypeScript archives only: a 0.3 updater cannot launch,
 * launch-check, or roll back a native archive
 * ([D14](../../docs/roadmap/rust-0.4/scope-00/support.md#decision-register)).
 * A file name alone would not stop a rename from routing the bytes anyway, so
 * both read the archive's entries and require the layout of their own route.
 */

import { NATIVE_ARCHIVE_PREFIX } from './pack-rust.ts'
import { tarEntries } from './permissions.ts'

/**
 * The native executable, as `pack-rust.ts` archives it.
 *
 * The product's command is `bake`, and the archived executable is being
 * renamed from `bake-rs` to `bake`, so both names are native here; neither
 * identifies an archive on its own, because a 0.3 archive also holds a
 * `bin/bake` launcher script.
 */
export const NATIVE_EXECUTABLES = ['bin/bake-rs', 'bin/bake-rs.exe', 'bin/bake', 'bin/bake.exe'] as const

/** The native license bundle's inventory, which every native archive carries. */
export const NATIVE_INVENTORY = 'licenses/inventory.json'

/** Where a native archive keeps the linked Bake crates' NOTICE files. */
export const NATIVE_NOTICES_DIR = 'notices/'

/**
 * Files only a 0.3 TypeScript archive holds: the command the 0.3 updater
 * launch-checks with Node (`packages/boot/updater/src/verify.ts`'s
 * `RELEASE_COMMAND`), the Node dependency graph `pack.ts` installs, and its
 * root manifest.
 */
export const TYPESCRIPT_MARKERS = ['apps/cli/lib/bin.js', 'package.json'] as const

/** The directory of the Node runtime dependencies `pack.ts` installs into a 0.3 archive. */
export const TYPESCRIPT_MODULES_DIR = 'node_modules/'

/** Which publication route an archive's layout belongs to. */
export type ArchiveKind = 'native' | 'typescript' | 'unknown'

/** What an archive holds, as {@link readArchiveLayout} reads it. */
export interface ArchiveLayout {
  /** The archive's path, for messages. */
  readonly archive: string
  /** Entry paths, `/`-separated, with a leading `./` and trailing `/` removed. */
  readonly paths: readonly string[]
  /** Entries that are absolute, escape the archive root, or are neither a file nor a directory. */
  readonly refusedEntries: readonly string[]
  /** The {@link NATIVE_EXECUTABLES} it holds. */
  readonly nativeExecutables: readonly string[]
  /** The {@link TYPESCRIPT_MARKERS} and Node modules it holds. */
  readonly typescriptMarkers: readonly string[]
  readonly kind: ArchiveKind
}

/**
 * Read an archive's entry headers and classify its layout.
 * @param archive - path to a `.tar.gz` file.
 * @throws when it is not a readable gzipped tar archive.
 */
export async function readArchiveLayout(archive: string): Promise<ArchiveLayout> {
  const paths: string[] = []
  const refusedEntries: string[] = []
  const stream = Bun.file(archive).stream().pipeThrough(new DecompressionStream('gzip'))
  try {
    for await (const entry of tarEntries(stream)) {
      const path = entry.path.replace(/^\.\//u, '').replace(/\/+$/u, '')
      if (path === '' || path === '.') continue
      // A link or device entry can place bytes outside the unpacked release.
      if (entry.type !== '0' && entry.type !== '5') refusedEntries.push(`${path} (tar type ${entry.type})`)
      else if (path.startsWith('/') || /^[A-Za-z]:/u.test(path) || path.split('/').includes('..')) refusedEntries.push(path)
      paths.push(path)
    }
  } catch (error) {
    throw new Error(`${archive} is not a readable gzipped tar archive: ${error instanceof Error ? error.message : String(error)}`)
  }
  const nativeExecutables = NATIVE_EXECUTABLES.filter(name => paths.includes(name))
  const typescriptMarkers = [
    ...TYPESCRIPT_MARKERS.filter(name => paths.includes(name)),
    ...paths.some(path => path.startsWith(TYPESCRIPT_MODULES_DIR)) ? [TYPESCRIPT_MODULES_DIR] : [],
  ]
  // Native is proved by the whole native payload, not by an executable name a
  // 0.3 launcher script shares.
  const native = nativeExecutables.length > 0 && paths.includes(NATIVE_INVENTORY)
    && paths.some(path => path.startsWith(NATIVE_NOTICES_DIR))
  const kind: ArchiveKind = native && typescriptMarkers.length === 0 ? 'native'
    : !native && typescriptMarkers.length > 0 ? 'typescript' : 'unknown'
  return { archive, paths, refusedEntries, nativeExecutables, typescriptMarkers, kind }
}

/** Whether `file` is named as `pack-rust.ts` names a native archive. */
export function isNativeArchiveName(file: string): boolean {
  return file.startsWith(NATIVE_ARCHIVE_PREFIX)
}

/**
 * Check that `layout` is a native archive the native route may publish.
 * @throws naming the first reason it is not.
 */
export function assertNativeArchive(layout: ArchiveLayout): void {
  const [refused] = layout.refusedEntries
  if (refused !== undefined) throw new Error(`${layout.archive} holds ${refused}, which no release archive may carry`)
  const [marker] = layout.typescriptMarkers
  if (marker !== undefined) throw new Error(`${layout.archive} holds ${marker} of a TypeScript release; a native archive carries no Node runtime or TypeScript product code`)
  if (layout.kind !== 'native') {
    throw new Error(`${layout.archive} is not a native archive: it needs one of ${NATIVE_EXECUTABLES.join(', ')} with ${NATIVE_INVENTORY} and ${NATIVE_NOTICES_DIR}`)
  }
}

/**
 * Check that `file` may be published on the 0.3 route, which only a
 * TypeScript archive may: its updater reads that manifest.
 * @throws naming why the archive is not a TypeScript release archive.
 */
export function assertTypeScriptArchive(file: string, layout: ArchiveLayout): void {
  if (isNativeArchiveName(file)) throw new Error(`${file} is a native archive; publish it with release:assemble:rust, not on the 0.3 route`)
  if (layout.kind === 'native') {
    throw new Error(`${file} holds ${layout.nativeExecutables.join(' and ')} with ${NATIVE_INVENTORY}; a native archive cannot be published to 0.3 updaters`)
  }
  if (layout.kind !== 'typescript') {
    throw new Error(`${file} is not a TypeScript release archive: it holds none of ${[...TYPESCRIPT_MARKERS, TYPESCRIPT_MODULES_DIR].join(', ')}`)
  }
}
