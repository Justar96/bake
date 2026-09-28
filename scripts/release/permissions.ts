/**
 * Release archives carry no group- or world-writable entry.
 *
 * Bun's isolated linker marks each package `bin` target mode 0777 whatever the
 * umask, so a staged install holds world-writable files that tar would ship as
 * they are. {@link restrictSharedWrite} clears those bits in the stage, and
 * {@link sharedWritableEntries} reads an archive back to prove none is left.
 */

import { chmodSync, lstatSync, readdirSync } from 'node:fs'
import { join } from 'node:path'

/** The group and other write bits, which `chmod go-w` clears. */
export const SHARED_WRITE = 0o022

/**
 * Clear group and other write permission on `root` and everything under it,
 * keeping every other bit, so executables stay executable. Symlinks are
 * neither changed nor followed: their own mode means nothing, and `chmod`
 * would change their target instead.
 * @param root - the staged release tree.
 * @param platform - Windows has no POSIX modes to change; its tar invents them.
 * @returns the paths changed, relative to `root` and `/`-separated.
 */
export function restrictSharedWrite(root: string, platform: NodeJS.Platform = process.platform): string[] {
  if (platform === 'win32') return []
  const changed: string[] = []
  const visit = (path: string, relative: string): void => {
    const stats = lstatSync(path)
    if (stats.isSymbolicLink()) return
    const mode = stats.mode & 0o7777
    if ((mode & SHARED_WRITE) !== 0) {
      chmodSync(path, mode & ~SHARED_WRITE)
      changed.push(relative)
    }
    if (!stats.isDirectory()) return
    for (const name of readdirSync(path).sort()) visit(join(path, name), relative === '.' ? name : `${relative}/${name}`)
  }
  visit(root, '.')
  return changed
}

/** One tar entry's header, as {@link tarEntries} reads it. */
export interface ArchiveEntry {
  readonly path: string
  /** The tar type flag: `0` a file, `1` a hard link, `2` a symlink, `5` a directory. */
  readonly type: string
  /** Permission bits, including setuid, setgid, and sticky. */
  readonly mode: number
}

const BLOCK = 512
const decoder = new TextDecoder()

/** Buffers a byte stream so whole tar blocks can be read or skipped from it. */
class BlockReader {
  private readonly chunks: Uint8Array[] = []
  private buffered = 0

  constructor(private readonly source: AsyncIterator<Uint8Array>) {}

  /** Buffer `size` bytes, or report that the stream ended first. */
  private async fill(size: number): Promise<boolean> {
    while (this.buffered < size) {
      const next = await this.source.next()
      if (next.done === true) return false
      this.chunks.push(next.value)
      this.buffered += next.value.byteLength
    }
    return true
  }

  /** The next `size` bytes, or `undefined` when the stream ends before them. */
  async read(size: number): Promise<Uint8Array | undefined> {
    if (!await this.fill(size)) return undefined
    const bytes = new Uint8Array(size)
    let offset = 0
    while (offset < size) {
      const chunk = this.head()
      const take = Math.min(chunk.byteLength, size - offset)
      bytes.set(chunk.subarray(0, take), offset)
      offset += take
      this.drop(take)
    }
    return bytes
  }

  /** Discard `size` bytes, such as a file's content, without copying them. */
  async skip(size: number): Promise<void> {
    let left = size
    while (left > 0) {
      if (this.buffered === 0 && !await this.fill(1)) throw new Error('The tar archive ends inside an entry')
      const take = Math.min(this.head().byteLength, left)
      this.drop(take)
      left -= take
    }
  }

  /** The first buffered chunk; callers fill the buffer before asking. */
  private head(): Uint8Array {
    const [chunk] = this.chunks
    if (chunk === undefined) throw new Error('The tar archive ends inside an entry')
    return chunk
  }

  private drop(size: number): void {
    const chunk = this.head()
    if (size >= chunk.byteLength) this.chunks.shift()
    else this.chunks[0] = chunk.subarray(size)
    this.buffered -= size
  }
}

/** A NUL-terminated header string. */
function text(bytes: Uint8Array, start: number, length: number): string {
  const field = bytes.subarray(start, start + length)
  const end = field.indexOf(0)
  return decoder.decode(end === -1 ? field : field.subarray(0, end))
}

/** A header number: octal digits, or GNU base-256 when the high bit is set. */
function number(bytes: Uint8Array, start: number, length: number): number {
  const field = bytes.subarray(start, start + length)
  const [first = 0] = field
  if ((first & 0x80) !== 0) return field.subarray(1).reduce((value, byte) => value * 256 + byte, first & 0x7f)
  const digits = text(bytes, start, length).trim()
  return digits === '' ? 0 : Number.parseInt(digits, 8)
}

/** Whether a header's checksum matches: the byte sum with its checksum field read as spaces. */
function checksumMatches(header: Uint8Array): boolean {
  let sum = 0
  for (const [index, byte] of header.entries()) sum += index >= 148 && index < 156 ? 0x20 : byte
  return sum === number(header, 148, 8)
}

/** The `path` record of a pax extended header, when it has one. */
function paxPath(data: Uint8Array): string | undefined {
  let path: string | undefined
  let offset = 0
  // Each record is "<length> <key>=<value>\n", its length counting every byte of it.
  while (offset < data.byteLength) {
    const space = data.indexOf(0x20, offset)
    if (space === -1) break
    const length = Number.parseInt(decoder.decode(data.subarray(offset, space)), 10)
    if (!Number.isInteger(length) || length <= 0) break
    const record = decoder.decode(data.subarray(space + 1, offset + length - 1))
    const equals = record.indexOf('=')
    if (equals !== -1 && record.slice(0, equals) === 'path') path = record.slice(equals + 1)
    offset += length
  }
  return path
}

/**
 * Read the entry headers of an uncompressed tar stream, in archive order.
 *
 * Understands ustar prefixes, pax `path` records, and GNU long names, which
 * GNU tar, bsdtar, and Windows `tar.exe` use for paths over 100 bytes.
 * @param source - the tar bytes.
 * @throws when a header's checksum does not match, or the stream ends inside an entry.
 */
export async function* tarEntries(source: AsyncIterable<Uint8Array>): AsyncGenerator<ArchiveEntry> {
  const reader = new BlockReader(source[Symbol.asyncIterator]())
  let longName: string | undefined
  let extendedPath: string | undefined
  for (;;) {
    const header = await reader.read(BLOCK)
    // End of archive: a zero block, or the end of the stream after the last entry.
    if (header === undefined || header.every(byte => byte === 0)) return
    if (!checksumMatches(header)) throw new Error('Not a tar archive: a header checksum does not match')
    const size = number(header, 124, 12)
    const padded = Math.ceil(size / BLOCK) * BLOCK
    const typeflag = header[156] ?? 0
    const flag = typeflag === 0 ? '0' : String.fromCharCode(typeflag)
    // Metadata for the next entry: GNU long names and pax records; global pax and long link names are ignored.
    if (flag === 'L' || flag === 'x' || flag === 'K' || flag === 'g') {
      const data = await reader.read(padded)
      if (data === undefined) throw new Error('The tar archive ends inside an entry')
      if (flag === 'L') longName = text(data, 0, size)
      else if (flag === 'x') extendedPath = paxPath(data.subarray(0, size)) ?? extendedPath
      continue
    }
    // Only POSIX ustar ("ustar\0") has a prefix field; GNU's "ustar " header keeps other data there.
    const prefix = text(header, 257, 6) === 'ustar' ? text(header, 345, 155) : ''
    const name = prefix === '' ? text(header, 0, 100) : `${prefix}/${text(header, 0, 100)}`
    yield { path: extendedPath ?? longName ?? name, type: flag, mode: number(header, 100, 8) & 0o7777 }
    longName = undefined
    extendedPath = undefined
    await reader.skip(padded)
  }
}

/**
 * List the entries of a gzipped tar archive that group or other users could
 * write once unpacked. Symlinks are exempt: their mode is always 0777 and
 * never consulted.
 * @param archive - path to a `.tar.gz` file.
 * @returns the offending entries, in archive order.
 */
export async function sharedWritableEntries(archive: string): Promise<ArchiveEntry[]> {
  const tar = Bun.file(archive).stream().pipeThrough(new DecompressionStream('gzip'))
  const writable: ArchiveEntry[] = []
  for await (const entry of tarEntries(tar)) {
    if (entry.type !== '2' && (entry.mode & SHARED_WRITE) !== 0) writable.push(entry)
  }
  return writable
}
