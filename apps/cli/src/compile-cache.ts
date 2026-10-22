/**
 * Node's module compile cache for the launcher process.
 *
 * A profile boot loads more than a thousand modules. With the cache on, V8
 * reuses their compiled code from disk instead of compiling them again on
 * every launch. The cache only speeds up modules loaded after it is enabled,
 * so the `bin` entry enables it before it imports anything else.
 * @module bake-cli/compile-cache
 */

import { lstatSync, mkdirSync } from 'node:fs'
import module from 'node:module'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

/** The part of `node:module` the cache needs. Older Node versions lack these functions. */
export interface CompileCacheApi {
  enableCompileCache?(directory?: string): { directory?: string | undefined }
  flushCompileCache?(): void
}

/**
 * Choose the cache directory: Node's own default, `<tmpdir>/node-compile-cache`.
 *
 * Node keys entries by its version, the V8 cache format, the user id, and each
 * file's path and content, so a changed source file or an upgraded Node never
 * reads a stale entry. Node never deletes entries. Those left behind by replaced
 * releases stay only until the system cleans its temporary files, where a
 * directory under the Bake home would keep them forever.
 *
 * On POSIX the temporary directory is usually shared between users, and V8
 * trusts cached code. The directory is therefore used only when this user owns
 * it and no other user can write to it, so no one else can create the entries
 * Node reads back. It is created owner-only when absent. Windows keeps the
 * temporary directory per user and has no uids to compare.
 * @param base - the temporary directory.
 * @param uid - this process's user id, or `undefined` where the platform has none.
 * @returns the directory, or `undefined` when another user could write to it.
 * @throws when the directory cannot be inspected.
 */
export function compileCacheDirectory(base: string = tmpdir(), uid: number | undefined = process.getuid?.()): string | undefined {
  const directory = join(base, 'node-compile-cache')
  if (uid === undefined) return directory
  try {
    mkdirSync(directory, { mode: 0o700 })
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') return undefined
  }
  const info = lstatSync(directory)
  // Group write stays allowed: a user-private group (umask 002) is the common case.
  return info.isDirectory() && info.uid === uid && (info.mode & 0o002) === 0 ? directory : undefined
}

/**
 * Turn on Node's module compile cache for this process, as the first thing it does.
 *
 * `NODE_DISABLE_COMPILE_CACHE` leaves the cache off without touching the disk.
 * A caller's own `NODE_COMPILE_CACHE` directory, which Node applied at startup,
 * is kept. Otherwise the cache goes to {@link compileCacheDirectory}. Child
 * processes do not inherit the setting. The cache only affects speed, so an
 * unavailable directory, a Node without the API, or any failure leaves it off
 * and never fails startup.
 * @param env - the process environment.
 * @param api - `node:module`; tests substitute it.
 * @param locate - chooses the directory; tests substitute it.
 * @returns whether the cache is on.
 */
export function enableCompileCache(
  env: NodeJS.ProcessEnv = process.env,
  api: CompileCacheApi = module,
  locate: () => string | undefined = compileCacheDirectory,
): boolean {
  if (env.NODE_DISABLE_COMPILE_CACHE !== undefined || typeof api.enableCompileCache !== 'function') return false
  try {
    const directory = env.NODE_COMPILE_CACHE === undefined ? locate() : undefined
    if (directory === undefined && env.NODE_COMPILE_CACHE === undefined) return false
    // Node reports a directory only when the cache is on, whether now or already.
    return api.enableCompileCache(directory).directory !== undefined
  } catch {
    return false
  }
}

/**
 * Write the entries compiled so far. Node writes new entries again at exit,
 * but a process killed by a signal skips that.
 * @param api - `node:module`; tests substitute it.
 */
export function flushCompileCache(api: CompileCacheApi = module): void {
  try {
    api.flushCompileCache?.()
  } catch {
    // An unwritten cache only costs the next launch its compile time.
  }
}
