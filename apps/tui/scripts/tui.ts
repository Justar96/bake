#!/usr/bin/env bun
/** Bun development commands; the agent and Ink integration tests execute on Node. */

import { resolve, join } from 'node:path'
import { availableParallelism, homedir } from 'node:os'
import { bundle, BUILT_ENV, diagnosticArguments, profileEnvironment, requireBuilt } from './build.ts'

const ROOT = resolve(import.meta.dir, '../../..')
const APP_LIB = join(ROOT, 'apps/tui/packages/app/lib')
const CLI = join(ROOT, 'apps/cli/lib/bin.js')
const APP_ENTRIES = ['index.js', 'startup.js', 'runner-loader.js', 'ui-loader.js', 'syntax-loader.js'] as const
const builtApp = (): string[] => APP_ENTRIES.map(entry => join(APP_LIB, entry))

/**
 * Processes one check target runs at once: its independent tsc programs,
 * layout scenes, or `bun test` workers. One core stays free for whatever runs
 * beside the target, such as the other targets preflight runs together; the
 * widest target gains nothing past eight.
 */
const WIDTH = Math.max(1, Math.min(8, availableParallelism() - 1))

/**
 * Run a command with the terminal attached, so an interactive app keeps its tty.
 *
 * @param command - argv to spawn.
 * @param options - overrides, such as a different working directory.
 * @returns the exit code.
 */
async function run(command: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): Promise<number> {
  const child = Bun.spawn(command, {
    cwd: options.cwd ?? ROOT, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit',
    env: { ...process.env, ...options.env },
  })
  return await child.exited
}

/** A command that failed; the dispatcher exits with its code once nothing else is left to run. */
class Failed extends Error {
  constructor(readonly code: number, readonly command: string, message = `${command} exited with ${code}`) {
    super(message)
  }
}

/**
 * Run a command and fail the dispatcher's command when it fails.
 *
 * @param command - argv to spawn.
 * @param options - overrides, such as a different working directory.
 * @throws {Failed} with the command's exit code.
 */
async function must(command: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): Promise<void> {
  const code = await run(command, options)
  if (code !== 0) throw new Failed(code, command.join(' '))
}

/** One of a target's commands that neither reads nor writes another's output. */
interface Task {
  /** How the report names the command. */
  label: string
  /** argv to spawn from the repository root. */
  command: string[]
  /** Discard stdout, for a command that reports on stderr and demonstrates on stdout. */
  quiet?: boolean
}

/** Output a task wrote, kept with the stream it was written to. */
interface Chunk {
  stream: NodeJS.WriteStream
  data: Uint8Array
}

/**
 * Run a task with its output held back until it exits.
 *
 * @param task - the command to spawn.
 * @returns its exit code and its output in arrival order; a spawn failure is exit code 127.
 */
async function held(task: Task): Promise<{ code: number; output: Chunk[] }> {
  const output: Chunk[] = []
  const report = (error: unknown) => { output.push({ stream: process.stderr, data: new TextEncoder().encode(`${String(error)}\n`) }) }
  // A read error is reported like output, so the child is still awaited.
  const hold = async (source: ReadableStream<Uint8Array>, stream: NodeJS.WriteStream) => {
    try {
      for await (const data of source) output.push({ stream, data })
    } catch (error) {
      report(error)
    }
  }
  let child
  try {
    child = Bun.spawn(task.command, { cwd: ROOT, stdin: 'ignore', stdout: task.quiet === true ? 'ignore' : 'pipe', stderr: 'pipe' })
  } catch (error) {
    report(error)
    return { code: 127, output }
  }
  await Promise.all([
    child.stdout instanceof ReadableStream ? hold(child.stdout, process.stdout) : undefined,
    hold(child.stderr, process.stderr),
  ])
  return { code: await child.exited, output }
}

/**
 * Run independent commands, up to {@link WIDTH} at once. Each command's output
 * is printed whole once it exits, in declaration order, so concurrent commands
 * never interleave. A failure does not stop the others: every command runs and
 * every failure is named.
 *
 * @param tasks - commands that neither read nor write each other's output.
 * @throws {Failed} with the first failure's exit code, in declaration order.
 */
async function concurrently(tasks: readonly Task[]): Promise<void> {
  const queue = [...tasks.keys()]
  const results = tasks.map(() => Promise.withResolvers<{ code: number; output: Chunk[] }>())
  const workers = Array.from({ length: Math.min(WIDTH, tasks.length) }, async () => {
    for (let index = queue.shift(); index !== undefined; index = queue.shift()) results[index]!.resolve(await held(tasks[index]!))
  })
  const failed: { label: string; code: number }[] = []
  for (const [index, task] of tasks.entries()) {
    const { code, output } = await results[index]!.promise
    for (const { stream, data } of output) await new Promise<void>(written => stream.write(data, () => written()))
    if (code === 0) continue
    failed.push({ label: task.label, code })
    console.error(`${task.label} exited with ${code}`)
  }
  await Promise.all(workers)
  const [first] = failed
  if (first === undefined) return
  throw failed.length === 1 ? new Failed(first.code, first.label)
    : new Failed(first.code, failed.map(item => item.label).join(', '), `${failed.length} of ${tasks.length} commands failed`)
}

/** A strict tsc run of one program, whose diagnostics fit a log. */
const tsc = (...args: string[]): string[] => ['bun', 'node_modules/typescript/bin/tsc', ...args, '--pretty', 'false']

/**
 * Bundle the plugin and the recorder to Node ESM.
 *
 * Tools do not work under the tsx source launch. `dsh-tools` keys its scheduler
 * with `Symbol('…')`, not `Symbol.for('…')`. The source launch then ends
 * up with two module instances of that package, so the lookup misses. Upstream
 * `headless` fails identically from source and works from `lib/`. The recorder
 * builds too, because fixtures worth having contain tool events.
 */
async function build(): Promise<void> {
  const entrypoints = ['apps/tui/packages/app/src/index.ts', 'apps/tui/packages/app/src/startup.ts',
                       'apps/tui/packages/app/src/runner-loader.ts', 'apps/tui/packages/app/src/ui-loader.ts',
                       'apps/tui/packages/app/src/syntax-loader.ts',
                       'apps/tui/packages/harness/record.ts']
  const built = await bundle(entrypoints.map(entry => join(ROOT, entry)), APP_LIB)
  console.log('built for Node with production React:')
  for (const output of built) console.log(`  ${output.path} (${output.size} bytes)`)
}

/** One selectable validation target. */
interface Check {
  /** The name `check <target>` selects. */
  name: string
  /** What it validates. */
  summary: string
  /** The target itself. */
  run: () => Promise<void>
}

const CHECKS: Check[] = [
  {
    name: 'peers',
    summary: 'React instance identity across Bake and Ink consumers',
    // Check the resolver used by the shipped Node application.
    run: () => must(['node', 'apps/tui/scripts/check-react-peers.mjs']),
  },
  {
    name: 'types',
    summary: 'strict types for application, tests, and Bun build/performance tooling',
    run: async () => {
      // The other programs read the packages' declarations through project
      // references, so the build that writes them finishes first. They emit
      // nothing, so they can then check at once.
      await must(tsc('-b', 'apps/tui/tsconfig.json'))
      await concurrently(['apps/tui/tsconfig.tests.json', 'apps/tui/tsconfig.tools.json', 'apps/tui/packages/app/performance/tsconfig.json']
        .map((project) => {
          const command = tsc('-p', project)
          return { label: command.join(' '), command }
        }))
    },
  },
  {
    name: 'unit',
    summary: 'pure modules and Bun tooling under bun test',
    // Filter by suffix, not by naming files. `bun test` matches `.spec.`
    // too, and those need Node. An explicit list silently omits every
    // pure test added later. Files run in parallel workers, each in a fresh
    // global (`--parallel` implies `--isolate`), so none sees another's leftovers.
    run: () => must(['bun', 'test', `--parallel=${WIDTH}`, '.test.ts'], { cwd: join(ROOT, 'apps/tui') }),
  },
  {
    name: 'spec',
    summary: 'component and integration specs under Node and vitest',
    // On Node. These mount real Cordis trees and Ink's real input channels, and
    // testing those on a runtime we cannot ship to is how drift starts.
    run: () => must(['node', 'node_modules/vitest/vitest.mjs', 'run', '--config', 'apps/tui/vitest.config.ts']),
  },
  {
    name: 'layout',
    summary: 'rendered layout invariants from DESIGN-LAYOUT.md',
    // Each scene renders through Ink and exits non-zero when a region overruns
    // the budget that keeps Ink off its screen-clearing path, when a frame
    // changes height while a turn runs, or when a rendered character rises
    // above 0x80, where a terminal and string-width can disagree about its width.
    // Scenes render in memory and share nothing, so they run at once.
    run: async () => {
      await concurrently(['frames', 'stability', 'realloop', 'overlays', 'stream', 'separation', 'chat', 'ascii']
        .map(scene => ({ label: `layout scene ${scene}`, command: ['bun', `apps/tui/prototype/${scene}.mjs`], quiet: true })))
      console.log('layout invariants: budgets, stability, and the ASCII vocabulary hold')
    },
  },
  {
    name: 'docs',
    summary: 'local Markdown links and anchors',
    run: () => must(['bun', 'apps/tui/scripts/check-docs.ts']),
  },
]

/**
 * Run the selected validation targets in declaration order. A failing target
 * does not stop the ones after it: every failure is reported, then the
 * command fails. Targets never overlap, so only one target's processes load
 * the machine at a time.
 *
 * @param names - the targets asked for; empty or `all` runs every one.
 * @throws {Failed} when any target failed.
 */
async function check(names: string[]): Promise<void> {
  if (names.length > 1 && (names.includes('--list') || names.includes('all'))) {
    console.error('check all and check --list must be used without other targets')
    process.exit(2)
  }
  if (names[0] === '--list') {
    for (const target of CHECKS) console.log(`${target.name.padEnd(8)} ${target.summary}`)
    return
  }
  const wanted = names.length === 0 || names[0] === 'all' ? CHECKS.map(target => target.name) : names
  const unknown = wanted.filter(name => !CHECKS.some(target => target.name === name))
  if (unknown.length > 0) {
    console.error(`unknown target: ${unknown.join(', ')}`)
    console.error(`known: ${CHECKS.map(target => target.name).join(' ')}`)
    process.exit(2)
  }
  const started = performance.now()
  const failed: string[] = []
  for (const name of wanted) {
    const target = CHECKS.find(item => item.name === name)!
    const step = performance.now()
    const took = () => `${((performance.now() - step) / 1000).toFixed(1)}s`
    console.log(`--- ${target.name}: ${target.summary}`)
    try {
      await target.run()
      console.log(`--- ${target.name} passed in ${took()}`)
    } catch (error) {
      if (!(error instanceof Failed)) throw error
      failed.push(target.name)
      console.log(`--- ${target.name} FAILED in ${took()} (${error.message})`)
    }
  }
  const total = `${((performance.now() - started) / 1000).toFixed(1)}s`
  if (failed.length > 0) {
    console.log(`check: ${failed.length} of ${wanted.length} target(s) failed in ${total}: ${failed.join(', ')}`)
    throw new Failed(1, `check ${failed.join(' ')}`)
  }
  console.log(`check: ${wanted.length} target(s) passed in ${total}`)
}

/**
 * Drive the built profile through a real terminal.
 *
 * @param args - forwarded to the driver, except `--no-build`.
 */
async function e2e(args: string[]): Promise<void> {
  const forwarded = args.filter(argument => argument !== '--no-build')
  if (!args.includes('--list') && !args.includes('--help')) {
    requireBuilt([CLI])
    if (!args.includes('--no-build')) await build()
    requireBuilt(builtApp())
  }
  await must(['bun', 'apps/tui/scripts/pty-smoke.ts', ...forwarded], { env: BUILT_ENV })
}

/**
 * Start the built CLI as the release launcher does: in Bake's home, with
 * production React and Node's diagnostic flags.
 *
 * @param args - arguments for the CLI.
 */
async function launch(args: string[]): Promise<void> {
  const env = profileEnvironment(homedir(), process.env)
  await must(['node', ...diagnosticArguments(env.DSH_HOME), CLI, ...args], { env })
}

/** Print what this dispatcher can run. */
function usage(): void {
  console.log(`usage: tui/scripts/tui.ts <command> [arguments]

run the app
  dev [args]         component loop under Bun: no harness, no agent, no key
                     (--replay watches rows arrive)
  app [args]         run the built Node TUI; run bun run build first
  dsh [args]         built profile/plugin CLI using Bake's home (~/.bake by default)
  build              bundle the plugin and recorder for Node with production React

verify
  check [targets]    static and unit gate; \`check --list\` names the targets
  spec [args]        component and integration specs (vitest: -t NAME, --watch)
  unit [args]        pure modules and Bun tooling under bun test (--watch)
  e2e [args]         the built profile through a real terminal
                     (--list, --only NAME, --trace, --live, --no-build)
  perf [args]        built-profile latency and memory diagnostic; --mode development for a baseline
  verify             every gate CI runs, the whole runtime suite included (same as bun run verify)

fixtures
  record "<task>"    record a session fixture through the headless profile (needs a key)

Arguments after the command reach the underlying tool unchanged.`)
}

const [command = 'help', ...args] = Bun.argv.slice(2)

try {
  switch (command) {
    case 'dev':
      await must(['bun', '--hot', 'apps/tui/packages/harness/dev.tsx', ...args], { env: { NODE_ENV: 'development' } })
      break
    case 'app':
      requireBuilt([CLI, ...builtApp()])
      await launch(['--profile', 'tui', ...args])
      break
    case 'dsh':
      requireBuilt([CLI])
      await launch(args)
      break
    case 'build':
      await build()
      break
    case 'check':
      await check(args)
      break
    case 'spec': {
      // A bare `vitest` watches, which silently changes what `spec` means between
      // a terminal and a pipe; ask for watching explicitly instead.
      const mode = args.includes('--watch') ? 'watch' : 'run'
      await must(['node', 'node_modules/vitest/vitest.mjs', mode, '--config', 'apps/tui/vitest.config.ts',
                  ...args.filter(argument => argument !== '--watch')])
      break
    }
    case 'unit':
      await must(['bun', 'test', '.test.ts', ...args], { cwd: join(ROOT, 'apps/tui') })
      break
    case 'e2e':
      await e2e(args)
      break
    case 'perf':
      await must(['bun', 'apps/tui/packages/app/performance/terminal.perf.ts', ...args])
      break
    case 'verify':
      await must(['bun', 'run', 'verify', ...args])
      break
    case 'record':
      requireBuilt([CLI])
      await build()
      requireBuilt([join(APP_LIB, 'record.js')])
      await launch(['--profile', 'headless', '--patch', './apps/tui/packages/harness/record.patch.yml', ...args])
      break
    case 'help':
    case '--help':
    case '-h':
      usage()
      break
    default:
      console.error(`unknown command: ${command}\n`)
      usage()
      process.exit(2)
  }
} catch (error) {
  if (!(error instanceof Failed)) throw error
  process.exit(error.code)
}
