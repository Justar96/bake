/**
 * `bun run release:pack:rust` end to end on the real `bake`: the debug
 * build the native preflight group has just made is packed into a path with
 * spaces and non-ASCII text, unpacked by the system `tar`, and run with a
 * PATH that holds neither Node nor Bun, in a home whose Bake settings and
 * credentials are deliberately broken and must stay untouched. The archive's
 * license text must equal, byte for byte, what `native-licenses.ts --write`
 * generates for this host, so these tests need cargo-about 0.9.2 on PATH or
 * in `BAKE_CARGO_ABOUT`, and fail without it. This is a package-layout check,
 * not release qualification: no model, provider, sandbox, install, or update
 * runs. Run with `bun run test:rust:package`.
 */
import { afterEach, expect, test } from 'bun:test'
import {
  closeSync, lstatSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync,
} from 'node:fs'
import { spawn, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { BUNDLE_FILE, INVENTORY_FILE } from './native-licenses.ts'
import { archiveName, docsDigests, hostTarget, isRustTarget, parseSelfCheck } from './pack-rust.ts'

const ROOT = resolve(import.meta.dir, '../..')
const DOCS = join(ROOT, 'rust/crates/bake-cli/docs')
const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function scratch(label: string): string {
  const root = mkdtempSync(join(tmpdir(), `bake ${label} ñ 測試-`))
  roots.push(root)
  return root
}

/** Every path under `root` with its bytes and modification time. */
function snapshot(root: string): Record<string, string> {
  const out: Record<string, string> = {}
  const walk = (dir: string): void => {
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name)
      const stats = lstatSync(path)
      out[path] = `${stats.mtimeMs} ${stats.isDirectory() ? 'dir' : readFileSync(path).toString('base64')}`
      if (stats.isDirectory()) walk(path)
    }
  }
  walk(root)
  return out
}

const EARLIER = 'an earlier archive that must survive a refused pack\n'

/** How long a killed tree, its output, or `taskkill` itself may take to settle before the run is reported as not settling. */
const SETTLE_MS = 10_000
/** How long a command given a `ready` pattern may take to print it before it is killed. */
const READY_MS = 60_000

/** Whether the POSIX process group `pgid` has a member; signal 0 delivers nothing, so asking is safe whoever holds the id. */
function groupAlive(pgid: number): boolean {
  try {
    process.kill(-pgid, 0)
    return true
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM'
  }
}

/**
 * Kill the tree rooted at `pid`, which must not have been reaped yet, so its
 * id still names this command: on POSIX the process group it leads, on
 * Windows the tree `taskkill /T /F` finds below it, as `taskkillProcessTree`
 * in `packages/subprocess/subprocess-local` does, but bounded and checked.
 * @returns why the kill may have failed, or undefined.
 */
function killTree(pid: number): string | undefined {
  if (process.platform === 'win32') {
    const result = spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true, timeout: SETTLE_MS })
    if (result.error !== undefined) return `taskkill failed: ${result.error.message}`
    if (result.signal !== null) return `taskkill was stopped by ${result.signal}`
    return result.status === 0 ? undefined : `taskkill exited ${result.status}`
  }
  try {
    process.kill(-pid, 'SIGKILL')
    return undefined
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ESRCH' ? undefined : `the group kill failed: ${(error as Error).message}`
  }
}

/** Whether `promise` settles within `ms`. */
async function within(promise: Promise<unknown>, ms: number): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined
  const late = new Promise<false>((done) => { timer = setTimeout(() => done(false), ms) })
  try {
    return await Promise.race([promise.then(() => true, () => true), late])
  } finally {
    clearTimeout(timer)
  }
}

interface Ran { readonly code: number | null; readonly signal: NodeJS.Signals | null; readonly stdout: string; readonly stderr: string }

interface OwnedOptions {
  readonly env?: Record<string, string | undefined>
  /** Starts the deadline only once standard output matches, within {@link READY_MS}. */
  readonly ready?: RegExp
}

/**
 * Run `argv` from the repository root and return only once it has exited and
 * both output streams have closed. The packager runs Cargo and cargo-about,
 * whose processes inherit its pipes; a closed pipe is the evidence, whoever
 * the process was, that nothing still holds it.
 *
 * Its tree is signalled only at the deadline, while its exit has not been
 * observed: on POSIX the process
 * group it leads gets SIGKILL, on Windows `taskkill /T /F`. After an exit
 * nothing is signalled: a group whose leader is reaped can lose its id
 * once it empties. A POSIX group that still has members then, or output still
 * held open after {@link SETTLE_MS}, fails the run instead. On Windows a
 * process left by a command that already exited is found only by the output
 * it holds, which is waited for until the deadline and then closed; one that
 * holds no output is not detected. The process API does not expose a retained
 * process-group identity; the exit check and signal are synchronous, but a
 * child reaped before its exit callback remains a PID-reuse race.
 * @throws when the command is not ready in time, passes its deadline, leaves
 *   processes or open output, or cannot be killed; the message names every
 *   problem and the exit status or signal seen.
 */
async function owned(argv: readonly string[], timeoutMs: number, options: OwnedOptions = {}): Promise<Ran> {
  const [command, ...args] = argv
  if (command === undefined) throw new Error('No command to run')
  const posix = process.platform !== 'win32'
  const child = spawn(command, args, { cwd: ROOT, env: options.env ?? process.env, stdio: ['ignore', 'pipe', 'pipe'], detached: posix, windowsHide: true })
  const chunks = { stdout: [] as Buffer[], stderr: [] as Buffer[] }
  const text = (stream: 'stdout' | 'stderr'): string => Buffer.concat(chunks[stream]).toString('utf8')
  let readied: () => void = () => {}
  const ready = new Promise<void>((done) => { readied = done })
  child.stdout.on('data', (chunk: Buffer) => {
    chunks.stdout.push(chunk)
    if (options.ready === undefined || options.ready.test(text('stdout'))) readied()
  })
  if (options.ready === undefined) readied()
  child.stderr.on('data', (chunk: Buffer) => chunks.stderr.push(chunk))
  const closed = Promise.all([once(child.stdout, 'close'), once(child.stderr, 'close')])
  let status: { code: number | null; signal: NodeJS.Signals | null } = { code: null, signal: null }
  let observed = false
  const problems: string[] = []
  // A failed spawn closes both streams and never exits.
  const exited = new Promise<void>((done) => {
    child.once('error', (error) => {
      problems.push(`could not start: ${error.message}`)
      done()
    })
    child.once('exit', (code, signal) => {
      observed = true
      status = { code, signal }
      done()
    })
  })
  let killed = false
  const kill = (why: string): void => {
    problems.push(why)
    killed = true
    // Synchronous with the check: no exit can be observed between them.
    if (!observed && child.pid !== undefined) {
      const failed = killTree(child.pid)
      if (failed !== undefined) problems.push(failed)
    }
  }
  try {
    if (!(await within(Promise.race([ready, exited]), READY_MS)) && !observed) kill(`did not print ${String(options.ready)} within ${READY_MS} ms; its process tree was killed`)
    let deadline = performance.now() + timeoutMs
    if (problems.length === 0 && !(await within(exited, timeoutMs)) && !observed) {
      kill(`did not finish within ${timeoutMs} ms${options.ready === undefined ? '' : ' of printing that it was ready'}; its process tree was killed`)
      deadline = performance.now()
    }
    if (!(await within(exited, SETTLE_MS))) problems.push(`did not exit within ${SETTLE_MS} ms`)
    const lingering = (): boolean => posix && child.pid !== undefined && groupAlive(child.pid)
    const left = !killed && lingering()
    if (left) problems.push('exited leaving processes in its group, which were not signalled since the exit frees its id')
    // Killed processes close their pipes as they go; on Windows an orphan may still hold them.
    const wait = left ? 0 : posix ? SETTLE_MS : Math.max(deadline - performance.now(), SETTLE_MS)
    if (!(await within(closed, wait))) problems.push('kept its output open after exiting, so a process it started may still run')
    if (killed) {
      // Only asked with signal 0: SIGKILL went to every member at once.
      const settle = performance.now() + SETTLE_MS
      while (lingering() && performance.now() < settle) await Bun.sleep(20)
      if (lingering()) problems.push(`its process group still had members ${SETTLE_MS} ms after the kill`)
    }
  } finally {
    child.stdout.destroy()
    child.stderr.destroy()
  }
  if (problems.length > 0) {
    const seen = status.signal ?? (status.code === null ? 'no exit' : `exit ${status.code}`)
    throw new Error(`${argv.join(' ')} ${problems.join('; ')} (${seen}):\n${text('stderr')}`)
  }
  return { ...status, stdout: text('stdout'), stderr: text('stderr') }
}

/** Run a Bun script from the repository root with {@link owned}; a kill is reported, not read as an exit status. */
async function script(
  args: string[], timeout: number, env: Record<string, string | undefined> = process.env,
): Promise<{ code: number; stdout: string; stderr: string }> {
  const { code, signal, stdout, stderr } = await owned([process.execPath, ...args], timeout, { env })
  if (signal !== null || code === null) throw new Error(`bun ${args.join(' ')} was stopped by ${signal ?? 'an unknown cause'}:\n${stderr}`)
  return { code, stdout, stderr }
}

function cargoVersion(): string {
  const metadata = Bun.spawnSync(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], { cwd: join(ROOT, 'rust'), stdout: 'pipe', stderr: 'inherit' })
  expect(metadata.exitCode).toBe(0)
  const cli = (JSON.parse(metadata.stdout.toString()) as { packages: { name: string; version: string }[] }).packages.find(pkg => pkg.name === 'bake-cli')
  if (cli === undefined) throw new Error('cargo metadata lists no bake-cli')
  return cli.version
}

test.skipIf(!isRustTarget(hostTarget()))('the packed bake unpacks and runs without Node, Bun, or a usable Bake home', async () => {
  const target = hostTarget()
  if (!isRustTarget(target)) return
  const version = cargoVersion()
  const out = join(scratch('out'), 'out dir')
  // An earlier archive of the same name, which a passing pack replaces.
  mkdirSync(out)
  writeFileSync(join(out, archiveName(version, target)), EARLIER)
  const { code, stdout, stderr } = await script(['scripts/release/pack-rust.ts', '--profile', 'dev', '--out', out], 540_000)
  if (code !== 0) throw new Error(`release:pack:rust exited ${code}:\n${stderr}`)
  const archive = join(realpathSync.native(out), archiveName(version, target))
  expect(stdout.trimEnd().split('\n').at(-1)).toBe(archive)
  expect(readdirSync(out)).toEqual([archiveName(version, target)])
  expect(readFileSync(archive, 'utf8')).not.toBe(EARLIER)

  // The generator's own command, run apart from the packager, is the reference for the archived bytes.
  const reference = join(scratch('licenses'), 'licenses')
  const generated = await script(['scripts/release/native-licenses.ts', '--write', '--out', reference], 120_000)
  if (generated.code !== 0) throw new Error(`native-licenses --write exited ${generated.code}:\n${generated.stderr}`)

  const unpacked = join(scratch('unpacked'), 'unpacked dir')
  mkdirSync(unpacked)
  const archiveInput = openSync(archive, 'r')
  try {
    // -p restores the archived modes instead of applying this shell's umask.
    const untar = Bun.spawnSync(['tar', '-xpzf', '-'], { cwd: unpacked, stdin: archiveInput, stderr: 'pipe' })
    expect(untar.exitCode).toBe(0)
  } finally {
    closeSync(archiveInput)
  }
  const exe = target === 'win32-x64' ? 'bake.exe' : 'bake'
  expect(readdirSync(unpacked).sort()).toEqual(['LICENSE', 'THIRD_PARTY_NOTICES.md', 'bin', 'licenses', 'notices'])
  expect(readdirSync(join(unpacked, 'bin'))).toEqual([exe])
  // This host's license text and the inventory, exactly as generated, and no other platform's text.
  expect(readdirSync(join(unpacked, 'licenses')).sort()).toEqual([INVENTORY_FILE, target].sort())
  expect(readdirSync(join(unpacked, 'licenses', target))).toEqual([BUNDLE_FILE])
  const text = readFileSync(join(unpacked, 'licenses', target, BUNDLE_FILE))
  expect(text.equals(readFileSync(join(reference, target, BUNDLE_FILE)))).toBe(true)
  expect(readFileSync(join(unpacked, 'licenses', INVENTORY_FILE)).equals(readFileSync(join(reference, INVENTORY_FILE)))).toBe(true)
  const inventory = JSON.parse(readFileSync(join(unpacked, 'licenses', INVENTORY_FILE), 'utf8')) as { platforms: { id: string; sha256: string }[] }
  expect(inventory.platforms.find(entry => entry.id === target)?.sha256).toBe(createHash('sha256').update(text).digest('hex'))
  expect(text.toString('utf8').split('\n')[0]).toStartWith(`Third-party licenses for bake, ${target} (`)
  expect(text.toString('utf8')).toContain('Mario Zechner')
  // Every NOTICE is a Bake crate's, and those that port Pi keep its notice.
  const notices = readdirSync(join(unpacked, 'notices')).sort()
  expect(notices).toContain('bake-coding-agent')
  expect(notices).not.toContain('bake-conformance')
  expect(readFileSync(join(unpacked, 'notices/bake-coding-agent/NOTICE'), 'utf8')).toContain('Mario Zechner')
  if (process.platform !== 'win32') expect(statSync(join(unpacked, 'bin', exe)).mode & 0o777).toBe(0o755)

  // An empty PATH: neither Node nor Bun can be found, so nothing below can lean on them.
  const path = scratch('path')
  expect(Bun.which('node', { PATH: path })).toBeNull()
  expect(Bun.which('bun', { PATH: path })).toBeNull()
  const home = scratch('home')
  for (const dir of ['.bake', '.dsh']) {
    mkdirSync(join(home, dir))
    writeFileSync(join(home, dir, 'settings.json'), '{ broken')
    writeFileSync(join(home, dir, 'auth.json'), '["broken"')
    writeFileSync(join(home, dir, 'settings.yaml'), 'providers: [broken')
    writeFileSync(join(home, dir, '.credentials.yaml'), 'key: *broken')
  }
  const before = snapshot(home)
  const env: Record<string, string> = { PATH: path, HOME: home, USERPROFILE: home, BAKE_HOME: join(home, '.bake'), DSH_HOME: join(home, '.dsh') }
  if (process.platform === 'win32') for (const name of ['SystemRoot', 'windir']) if (process.env[name] !== undefined) env[name] = process.env[name] as string
  const run = (...args: string[]): { code: number; stdout: string; stderr: string } => {
    const child = Bun.spawnSync([join(unpacked, 'bin', exe), ...args], { cwd: unpacked, env, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', timeout: 30_000 })
    expect(child.signalCode ?? null).toBeNull()
    return { code: child.exitCode, stdout: child.stdout.toString(), stderr: child.stderr.toString() }
  }

  expect(run('--version')).toEqual({ code: 0, stdout: `bake ${version}\n`, stderr: '' })
  const check = run('--self-check')
  expect(check.code).toBe(0)
  expect(check.stderr).toBe('')
  expect(parseSelfCheck(check.stdout, { version, target, docs: docsDigests(DOCS) }).runtime).toBe('rust')
  const topics = run('docs')
  expect(topics.code).toBe(0)
  for (const name of Object.keys(docsDigests(DOCS))) {
    expect(topics.stdout).toContain(`  ${name}`)
    expect(run('docs', name)).toEqual({ code: 0, stdout: readFileSync(join(DOCS, `${name}.md`), 'utf8'), stderr: '' })
  }
  const refused = run('--self-check', 'extra')
  expect(refused.code).toBe(2)
  expect(refused.stdout).toBe('')
  expect(snapshot(home)).toEqual(before)
  expect(readdirSync(path)).toEqual([])
  // Above the 600 s lane: the pack's 540 s and the reference generation's 120 s
  // deadlines run in turn, and the binary checks after them take seconds.
}, 720_000)

test.skipIf(!isRustTarget(hostTarget()))('a failed license generation refuses the pack before building and keeps the earlier archive', async () => {
  const target = hostTarget()
  if (!isRustTarget(target)) return
  const version = cargoVersion()
  const out = join(scratch('out'), 'out dir')
  mkdirSync(out)
  writeFileSync(join(out, archiveName(version, target)), EARLIER)
  // No collector where BAKE_CARGO_ABOUT points: generation must fail, not be skipped.
  const absent = join(scratch('tools'), process.platform === 'win32' ? 'cargo-about.exe' : 'cargo-about')
  const { code, stderr } = await script(['scripts/release/pack-rust.ts', '--profile', 'dev', '--out', out], 300_000, { ...process.env, BAKE_CARGO_ABOUT: absent })
  expect(code).toBe(1)
  expect(stderr).toContain('cargo-about 0.9.2 is required')
  expect(stderr).not.toMatch(/Finished `dev` profile/)
  expect(readdirSync(out)).toEqual([archiveName(version, target)])
  expect(readFileSync(join(out, archiveName(version, target)), 'utf8')).toBe(EARLIER)
})

test('a TypeScript release output directory is refused before building', async () => {
  const { code, stderr } = await script(['scripts/release/pack-rust.ts', '--out', join(ROOT, 'distribution/host/public')], 60_000)
  expect(code).toBe(1)
  expect(stderr).toContain("inside the TypeScript release's distribution/host/public/")
  // Cargo reports every build it runs, even one with nothing to do.
  expect(stderr).not.toContain('Finished')
})

/**
 * A descendant for the {@link owned} controls, identified without its pid: it
 * listens on loopback and answers each connection with a nonce only it knows,
 * then exits when asked to stop or after 30 s on its own. The command that
 * starts it, after `startMs` standing in for a slow start, prints `ready`
 * only once it listens, then hangs or exits.
 */
function descendantProgram(then: 'hang' | 'exit', portFile: string, nonce: string, startMs: number): string {
  const descendant = [
    'setTimeout(() => process.exit(0), 30000)',
    'const server = require(\'node:net\').createServer((socket) => {',
    `  socket.write(${JSON.stringify(`${nonce}\n`)})`,
    '  let line = ""',
    '  socket.on(\'data\', (chunk) => {',
    '    line += String(chunk)',
    '    if (line.length > 128) { socket.destroy(); return }',
    '    if (!line.endsWith("\\n")) return',
    `    if (line !== ${JSON.stringify(`ping ${nonce}\n`)} && line !== ${JSON.stringify(`stop ${nonce}\n`)}) { socket.destroy(); return }`,
    `    socket.end('done\\n', () => { if (line === ${JSON.stringify(`stop ${nonce}\n`)}) process.exit(0) })`,
    '  })',
    '})',
    'server.listen(0, \'127.0.0.1\', () => {',
    '  const fs = require(\'node:fs\')',
    `  fs.writeFileSync(${JSON.stringify(`${portFile}.partial`)}, String(server.address().port))`,
    `  fs.renameSync(${JSON.stringify(`${portFile}.partial`)}, ${JSON.stringify(portFile)})`,
    '})',
  ].join('\n')
  return [
    `setTimeout(() => require('node:child_process').spawn(process.execPath, ['-e', ${JSON.stringify(descendant)}], { stdio: 'inherit' }), ${startMs})`,
    'const wait = setInterval(() => {',
    `  if (!require('node:fs').existsSync(${JSON.stringify(portFile)})) return`,
    '  clearInterval(wait)',
    `  process.stdout.write('ready\\n', () => { ${then === 'hang' ? 'setInterval(() => {}, 1000)' : 'process.exit(0)'} })`,
    '}, 10)',
  ].join('\n')
}

/** Ask the descendant on `port`: `ours` when it answers with `nonce`, `gone` when nothing there does. */
async function ask(port: number, nonce: string, request: 'ping' | 'stop'): Promise<'ours' | 'gone'> {
  const { connect } = await import('node:net')
  return await new Promise((done, fail) => {
    const socket = connect(port, '127.0.0.1')
    let reply = ''
    let identified = false
    const timer = setTimeout(() => {
      socket.destroy()
      fail(new Error(`The descendant on port ${port} neither answered nor refused within ${SETTLE_MS} ms`))
    }, SETTLE_MS)
    socket.on('data', (chunk) => {
      reply += String(chunk)
      if (reply.length > 128) { socket.destroy(); return }
      if (!identified && reply.includes('\n')) {
        if (!reply.startsWith(`${nonce}\n`)) { socket.destroy(); return }
        identified = true
        // Authenticate this connection before sending a stop request.
        socket.write(`${request} ${nonce}\n`)
      }
    })
    socket.on('error', () => {})
    socket.on('close', () => {
      clearTimeout(timer)
      done(identified ? 'ours' : 'gone')
    })
  })
}

/**
 * Run a control with a descendant from {@link descendantProgram}, then stop
 * that descendant through its own channel if it still answers. Cleanup that
 * cannot prove it stopped fails the test.
 */
async function withDescendant(
  then: 'hang' | 'exit', startMs: number, control: (argv: string[], gone: () => Promise<boolean>) => Promise<void>,
): Promise<void> {
  const portFile = join(scratch('descendant'), 'port')
  const nonce = createHash('sha256').update(`${portFile}${performance.now()}${Math.random()}`).digest('hex')
  const port = (): number | undefined => {
    try {
      return Number(readFileSync(portFile, 'utf8'))
    } catch {
      return undefined
    }
  }
  const gone = async (): Promise<boolean> => {
    const at = port()
    if (at === undefined) throw new Error('The descendant never reported its port')
    return await ask(at, nonce, 'ping') === 'gone'
  }
  let failure: unknown
  try {
    await control([process.execPath, '-e', descendantProgram(then, portFile, nonce, startMs)], gone)
  } catch (error) {
    failure = error
  }
  const at = port()
  let stopped = at !== undefined && await ask(at, nonce, 'stop') === 'gone'
  const deadline = performance.now() + SETTLE_MS
  while (at !== undefined && !stopped && performance.now() < deadline) {
    await Bun.sleep(20)
    stopped = await ask(at, nonce, 'ping') === 'gone'
  }
  if (failure !== undefined) throw failure
  if (!stopped) throw new Error(at === undefined ? 'The descendant never reported its port; it stops itself within 30 s' : `The descendant on port ${at} did not stop within ${SETTLE_MS} ms`)
}

// The start takes longer than the deadline, so a deadline counted from the
// spawn would kill the command before its descendant exists.
test('a command that outlives its deadline is killed with the descendant holding its output, the deadline counted from readiness', async () => {
  await withDescendant('hang', 3_000, async (argv, gone) => {
    const started = performance.now()
    await expect(owned(argv, 2_000, { ready: /^ready$/mu }))
      .rejects.toThrow(/did not finish within 2000 ms of printing that it was ready; its process tree was killed \((?:SIGKILL|exit \d+)\)/)
    expect(performance.now() - started).toBeLessThan(READY_MS + 2_000 + 2 * SETTLE_MS)
    expect(await gone()).toBe(true)
  })
}, READY_MS + 4 * SETTLE_MS)

const UNSIGNALLED = new RegExp([
  'exited leaving processes in its group, which were not signalled since the exit frees its id;',
  ' kept its output open .*\\(exit 0\\)',
].join(''), 'su')

// Windows has no process group in which to find a descendant whose parent
// has exited; this control is about the POSIX group check after an exit.
test.skipIf(process.platform === 'win32')('a command that exits leaving a descendant in its group fails, keeping its exit status, without signalling the group', async () => {
  await withDescendant('exit', 0, async (argv, gone) => {
    await expect(owned(argv, 30_000, { ready: /^ready$/mu }))
      .rejects.toThrow(UNSIGNALLED)
    // Unsignalled, it still answers; the fixture's own channel stops it.
    expect(await gone()).toBe(false)
  })
}, 4 * SETTLE_MS)
