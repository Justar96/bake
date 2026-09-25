/**
 * Install a verified release beside the running one, then move `current`.
 *
 * Every step before the last leaves the running install as it was:
 *
 * 1. Take the install lock, recovering one whose holder has exited.
 * 2. Stream the archive into `<root>/.staging/`, checking its size and hash.
 * 3. Unpack it there, then start the unpacked command and check its version.
 * 4. Rename the unpacked tree into `versions/`, on the same filesystem.
 * 5. Move `current`: an atomic link rename on Unix, an atomic pointer-file
 *    rename on Windows.
 *
 * An interruption at any point before step 5 leaves `current` naming the old
 * release, and the next run clears the staging directory under the lock.
 *
 * @module @deepseek-ai/dsh-updater/install
 */

import { createHash, randomBytes } from 'node:crypto'
import { execFile, spawn } from 'node:child_process'
import { createWriteStream, existsSync, lstatSync, readFileSync } from 'node:fs'
import { mkdir, readdir, rename, rm, stat, symlink, utimes, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { archiveUrl, UpdateError, type ReleaseArtifact, type ReleaseManifest, type ReleaseSource } from './manifest.ts'
import { CURRENT_POINTER, currentOf, directoryFor, versionDirectory, type ManagedInstall } from './layout.ts'

/** A lock older than this is abandoned whatever its process id says: no install takes half an hour. */
const LOCK_STALE_MS = 30 * 60 * 1000
/** A version directory no command started from for this long, and not current or previous, may be removed. */
export const PRUNE_AFTER_MS = 7 * 24 * 60 * 60 * 1000
/** File each launch touches in its own version directory, so pruning can tell an idle release from a running one. */
export const LAUNCH_MARKER = '.last-launch'
/** Bound on starting the unpacked command to read its version. */
const SMOKE_TIMEOUT_MS = 60_000

/** Everything one install needs, injected so tests own every effect. */
export interface InstallOptions extends ReleaseSource {
  readonly layout: ManagedInstall
  readonly manifest: ReleaseManifest
  readonly artifact: ReleaseArtifact
  /** Node executable that starts the unpacked command; the running one by default. */
  readonly node?: string
  /** Time source, for the lock's age. */
  readonly now?: () => number
  /** Platform whose pointer the install moves; the running one by default. */
  readonly platform?: NodeJS.Platform
  /** Windows: the `bake.cmd` that launched this process, to bring onto the pointer form. */
  readonly launcher?: string | undefined
}

/** What an install changed. */
export interface InstallResult {
  /** The version directory `current` now names. */
  readonly directory: string
  /** The directory `current` named before, when it named one. */
  readonly previous: string | undefined
  /** Version directories removed because nothing had started them recently. */
  readonly pruned: readonly string[]
  /** Windows: the launcher is replaced once the `cmd.exe` running it exits. */
  readonly launcherPending: boolean
}

/**
 * Install `artifact` and make it current.
 * @param options - the verified release, where to put it, and the effects to use.
 * @returns what changed.
 * @throws {UpdateError} when a step fails; `current` is unchanged in that case.
 */
export async function installRelease(options: InstallOptions): Promise<InstallResult> {
  const { layout, manifest, artifact } = options
  const platform = options.platform ?? process.platform
  const release = await acquireLock(layout.root, options.now ?? Date.now)
  try {
    const staging = join(layout.root, '.staging')
    // Held under the lock, so nothing else is writing here: whatever is left
    // is an interrupted run's.
    await rm(staging, { recursive: true, force: true })
    const directory = directoryFor(manifest.version, artifact.sha256)
    const destination = join(layout.root, 'versions', directory)
    const node = options.node ?? process.execPath
    if (!await starts(node, destination, manifest.version)) {
      const work = join(staging, randomBytes(6).toString('hex'))
      await mkdir(work, { recursive: true })
      const archive = join(work, artifact.file)
      await downloadArchive(options, archive)
      const unpacked = join(work, 'unpacked')
      await mkdir(unpacked)
      await run(platform === 'win32' ? 'tar.exe' : 'tar', ['-xzf', archive, '-C', unpacked], options.signal)
      if (!await starts(node, unpacked, manifest.version)) {
        throw new UpdateError(`The downloaded Bake ${manifest.version} did not start; the current install is unchanged`)
      }
      // A directory that exists but did not start is a damaged copy: set it
      // aside under staging rather than install over it.
      if (existsSync(destination)) await rename(destination, join(work, 'damaged'))
      await rename(unpacked, destination)
      await rm(work, { recursive: true, force: true })
    }
    const previous = currentOf(layout.root) ?? versionOf(layout.running)
    await pointAt(layout.root, directory, platform)
    const launcherPending = platform === 'win32' && options.launcher !== undefined
      ? await migrateLauncher(options.launcher, layout.root)
      : false
    await rm(staging, { recursive: true, force: true })
    const pruned = await prune(layout.root, new Set([directory, previous, versionOf(layout.running)].filter(name => name !== undefined)),
      (options.now ?? Date.now)())
    return { directory, previous, pruned, launcherPending }
  } finally {
    await release()
  }
}

/**
 * Take `<root>/update.lock`, recovering an abandoned one.
 *
 * The lock records its holder's process id and start time. A contender takes
 * it over when that process no longer exists or the lock is older than any
 * install takes, so a crashed or killed update never blocks the next.
 *
 * @param root - the install root.
 * @param now - time source.
 * @returns a function that releases the lock.
 * @throws {UpdateError} while another live update holds it.
 */
export async function acquireLock(root: string, now: () => number = Date.now): Promise<() => Promise<void>> {
  const path = join(root, 'update.lock')
  const content = `${JSON.stringify({ pid: process.pid, startedAt: now() })}\n`
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      await writeFile(path, content, { flag: 'wx', mode: 0o600 })
      return async () => { await rm(path, { force: true }) }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST' || attempt > 0) throw error
      if (!abandoned(path, now())) throw new UpdateError('Another Bake update is running; try again when it finishes')
      await rm(path, { force: true })
    }
  }
  /* v8 ignore next -- the loop returns or throws */
  throw new UpdateError('Could not take the update lock')
}

function abandoned(path: string, now: number): boolean {
  let holder: { pid?: unknown; startedAt?: unknown }
  try { holder = JSON.parse(readFileSync(path, 'utf8')) as typeof holder } catch { return true }
  if (typeof holder.pid !== 'number' || typeof holder.startedAt !== 'number') return true
  if (now - holder.startedAt > LOCK_STALE_MS) return true
  try {
    process.kill(holder.pid, 0)
    return false
  } catch (error) {
    // EPERM: the process exists, owned by someone else.
    return (error as NodeJS.ErrnoException).code === 'ESRCH'
  }
}

/** Stream the archive to `path`, refusing it once it outgrows its stated size or when its hash differs. */
async function downloadArchive(options: InstallOptions, path: string): Promise<void> {
  const { manifest, artifact } = options
  const url = archiveUrl(options.base, manifest.version, artifact.file)
  let response: Response
  try {
    response = await (options.fetch ?? fetch)(url, options.signal === undefined ? {} : { signal: options.signal })
  } catch (error) {
    if (options.signal?.aborted === true) throw error
    throw new UpdateError(`Could not download ${url}: ${error instanceof Error ? error.message : String(error)}`)
  }
  if (!response.ok || response.body === null) throw new UpdateError(`${url} answered ${response.status}`)
  const hash = createHash('sha256')
  const out = createWriteStream(path, { flags: 'wx', mode: 0o600 })
  const closed = new Promise<void>((resolve, reject) => { out.on('close', resolve); out.on('error', reject) })
  let size = 0
  try {
    for await (const chunk of response.body as unknown as AsyncIterable<Uint8Array>) {
      size += chunk.byteLength
      if (size > artifact.size) throw new UpdateError(`${artifact.file} is larger than the release manifest says`)
      hash.update(chunk)
      if (!out.write(chunk)) await new Promise<void>(resolve => out.once('drain', () => resolve()))
    }
  } finally {
    out.end()
    await closed
  }
  if (size !== artifact.size) throw new UpdateError(`${artifact.file} is ${size} bytes, not the ${artifact.size} the release manifest says`)
  if (hash.digest('hex') !== artifact.sha256) throw new UpdateError(`${artifact.file} did not match the release manifest's SHA-256`)
}

/** Whether the command in `release` starts and reports `version`. */
async function starts(node: string, release: string, version: string): Promise<boolean> {
  const bin = join(release, 'apps/cli/lib/bin.js')
  if (!existsSync(bin)) return false
  try {
    const output = await new Promise<string>((resolve, reject) => {
      execFile(node, [bin, '--version'], { timeout: SMOKE_TIMEOUT_MS, windowsHide: true }, (error, stdout) => {
        if (error === null) resolve(stdout)
        else reject(error)
      })
    })
    return output.trim().split(/\s+/).includes(version)
  } catch {
    return false
  }
}

function run(command: string, args: readonly string[], signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['ignore', 'ignore', 'pipe'], windowsHide: true, ...signal === undefined ? {} : { signal } })
    let stderr = ''
    child.stderr.on('data', (chunk: Buffer) => { stderr += chunk.toString() })
    child.on('error', error => reject(signal?.aborted === true ? error : new UpdateError(`Could not run ${command}: ${error.message}`)))
    child.on('close', code => code === 0 ? resolve() : reject(new UpdateError(`${command} exited ${code}: ${stderr.trim()}`)))
  })
}

/**
 * Make `current` name `directory`, atomically.
 *
 * Unix renames a fresh absolute link over `current`; Windows renames a fresh
 * pointer file over `current.txt`. Either way a reader sees the old target or
 * the new one, never neither.
 */
export async function pointAt(root: string, directory: string, platform: NodeJS.Platform = process.platform): Promise<void> {
  const suffix = `${process.pid}-${randomBytes(4).toString('hex')}`
  if (platform === 'win32') {
    const temp = join(root, `.${CURRENT_POINTER}-${suffix}`)
    await writeFile(temp, `${directory}\r\n`, { flag: 'wx' })
    try { await renameRetrying(temp, join(root, CURRENT_POINTER)) } catch (error) { await rm(temp, { force: true }); throw error }
    return
  }
  const current = join(root, 'current')
  if (existsSync(current) && !lstatSync(current).isSymbolicLink()) {
    throw new UpdateError(`${current} exists and is not a Bake-managed link`)
  }
  const temp = join(root, `.current-${suffix}`)
  await symlink(join(root, 'versions', directory), temp)
  try { await rename(temp, current) } catch (error) { await rm(temp, { force: true }); throw error }
}

/** Windows reports a file another process holds open as busy for a moment; retry briefly. */
async function renameRetrying(from: string, to: string): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try { await rename(from, to); return } catch (error) {
      const code = (error as NodeJS.ErrnoException).code
      if (attempt >= 8 || (code !== 'EACCES' && code !== 'EBUSY' && code !== 'EPERM')) throw error
      await new Promise(resolve => setTimeout(resolve, 20 * 2 ** Math.min(attempt, 3)))
    }
  }
}

/**
 * The Windows launcher: route the arguments as the Unix `bin/bake` does, to
 * the release `current.txt` names.
 *
 * The install root is written in once, at install, and the release is read
 * from the pointer on every run, so an update never has to rewrite this file.
 * That matters because `cmd.exe` reads a batch file a line at a time, by
 * offset, while it runs: rewritten under a running command, it would go on
 * executing the new file from the old file's offset.
 *
 * @param root - the install root.
 * @returns the file's content, CRLF-terminated.
 */
export function windowsLauncher(root: string): string {
  const literal = root.replaceAll('%', '%%')
  return [
    '@echo off',
    'setlocal',
    'if not defined DSH_HOME set "DSH_HOME=%USERPROFILE%\\.bake"',
    `set "BAKE_RELEASE_ROOT=${literal}"`,
    'set "BAKE_CURRENT="',
    `set /p BAKE_CURRENT=<"%BAKE_RELEASE_ROOT%\\${CURRENT_POINTER}"`,
    'if not defined BAKE_CURRENT (echo Bake is not installed correctly; run the installer again. 1>&2 & exit /b 1)',
    'set "BAKE_CLI=%BAKE_RELEASE_ROOT%\\versions\\%BAKE_CURRENT%\\apps\\cli\\lib\\bin.js"',
    'set "BAKE_LAUNCHER=%~f0"',
    'if /I "%~1"=="tui" goto raw',
    'if /I "%~1"=="headless" goto raw',
    'if /I "%~1"=="plugin" goto raw',
    'if /I "%~1"=="update" goto raw',
    'if /I "%~1"=="--profile" goto raw',
    'node "%BAKE_CLI%" --profile tui %*',
    'exit /b %ERRORLEVEL%',
    ':raw',
    'node "%BAKE_CLI%" %*',
    'exit /b %ERRORLEVEL%',
    '',
  ].join('\r\n')
}

/**
 * Bring an older `bake.cmd`, which named one release directly, onto the
 * pointer form.
 *
 * That file is the one running this process, so it cannot be replaced now
 * (see {@link windowsLauncher}). The new text waits beside it, and a detached
 * helper moves it into place once the `cmd.exe` running it has exited.
 *
 * @returns whether a replacement is pending.
 */
async function migrateLauncher(launcher: string, root: string): Promise<boolean> {
  const wanted = windowsLauncher(root)
  let existing = ''
  try { existing = readFileSync(launcher, 'utf8') } catch { return false }
  if (existing === wanted) return false
  const next = `${launcher}.new`
  await writeFile(next, wanted)
  const quote = (value: string): string => `'${value.replaceAll('\'', '\'\'')}'`
  const script = `Wait-Process -Id ${process.ppid} -ErrorAction SilentlyContinue; Move-Item -Force -LiteralPath ${quote(next)} -Destination ${quote(launcher)}`
  const helper = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-Command', script],
    { detached: true, stdio: 'ignore', windowsHide: true })
  helper.on('error', () => {})
  helper.unref()
  return true
}

/**
 * Remove version directories no one has started for {@link PRUNE_AFTER_MS}.
 *
 * `keep` always survives: the new current, the one it replaced, and the one
 * this process runs from. A release another terminal still runs from was
 * started recently, so its launch marker keeps it too; deleting it would
 * break that session's next lazy import.
 */
async function prune(root: string, keep: ReadonlySet<string>, now: number): Promise<string[]> {
  const versions = join(root, 'versions')
  const removed: string[] = []
  for (const name of await readdir(versions).catch(() => [] as string[])) {
    if (keep.has(name) || versionDirectory(name) === undefined) continue
    const directory = join(versions, name)
    const used = await stat(join(directory, LAUNCH_MARKER)).catch(() => stat(directory)).then(found => found.mtimeMs, () => now)
    if (now - used < PRUNE_AFTER_MS) continue
    try {
      await rm(directory, { recursive: true, force: true })
      removed.push(name)
    } catch {
      // Windows refuses to remove files a process holds open; that release is
      // in use, so it stays until the next update.
    }
  }
  return removed
}

/**
 * Record that a command started from `release`, for {@link prune}.
 *
 * Best effort: a read-only install or any other failure is ignored, since it
 * only makes a release look idle for longer.
 *
 * @param release - the running version directory.
 */
export async function markLaunched(release: string): Promise<void> {
  const marker = join(release, LAUNCH_MARKER)
  const now = new Date()
  try { await utimes(marker, now, now) } catch {
    try { await writeFile(marker, '') } catch { /* see above */ }
  }
}

function versionOf(release: string): string | undefined {
  const name = release.split(/[\\/]/).at(-1) ?? ''
  return versionDirectory(name) === undefined ? undefined : name
}
