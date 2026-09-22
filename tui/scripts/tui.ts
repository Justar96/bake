#!/usr/bin/env bun
/**
 * One entry point for TUI development: `tui/scripts/tui.ts help` lists
 * everything this fork can run, so the commands live next to the code instead
 * of only in AGENTS.md.
 *
 * The toolchain is Bun — this dispatcher, the bundler, the pure tests, the
 * component harness, and the terminal driver. Two things still run on Node, and
 * both are deliberate: the product itself, because `app-boot` reaches V8
 * current-context symbols JavaScriptCore does not have
 * ([PLAN.md](../PLAN.md#22-bun-cannot-run-the-harness)), and the checks whose
 * subject is that Node process — the integration specs and the resolver
 * identity gate.
 *
 * @module tui-dispatcher
 */

import { resolve, join } from 'node:path'
import { bundle, BUILT_ENV } from './build.ts'

const ROOT = resolve(import.meta.dir, '../..')
const SOURCE_PATCH = './tui/packages/app/cordis.patch.yml'
const BUILT_PATCH = './tui/packages/app/cordis.built.patch.yml'
const APP_LIB = join(ROOT, 'tui/packages/app/lib')

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

/**
 * Run a command and stop the dispatcher when it fails.
 *
 * @param command - argv to spawn.
 * @param options - overrides, such as a different working directory.
 */
async function must(command: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): Promise<void> {
  const code = await run(command, options)
  if (code !== 0) process.exit(code)
}

/**
 * Bundle the plugin and the recorder to Node ESM.
 *
 * Tools do not work under the tsx source launch: `dsh-tools` keys its scheduler
 * with `Symbol('…')` rather than `Symbol.for('…')`, and the source launch ends
 * up with two module instances of that package, so the lookup misses. Upstream
 * `headless` fails identically from source and works from `lib/`. The recorder
 * builds too, because fixtures worth having contain tool events.
 */
async function build(): Promise<void> {
  const entrypoints = ['tui/packages/app/src/index.ts', 'tui/packages/app/src/startup.ts',
                       'tui/packages/harness/record.ts']
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
    summary: 'React instance identity across upstream DOM and Ink consumers',
    // On Node because the guarantee is about the resolver the product and the
    // upstream DOM tests actually run under.
    run: () => must(['node', 'tui/scripts/check-react-peers.mjs']),
  },
  {
    name: 'types',
    summary: 'strict types for application, tests, and Bun build/performance tooling',
    run: async () => {
      await must(['bun', 'node_modules/typescript/bin/tsc', '-b', 'tui/tsconfig.json', '--pretty', 'false'])
      await must(['bun', 'node_modules/typescript/bin/tsc', '-p', 'tui/tsconfig.tests.json', '--pretty', 'false'])
      await must(['bun', 'node_modules/typescript/bin/tsc', '-p', 'tui/tsconfig.tools.json', '--pretty', 'false'])
      await must(['bun', 'node_modules/typescript/bin/tsc', '-p', 'tui/packages/app/performance/tsconfig.json', '--pretty', 'false'])
    },
  },
  {
    name: 'unit',
    summary: 'pure modules and Bun tooling under bun test',
    // Filter by suffix rather than naming files: `bun test` matches `.spec.`
    // too, and those need Node, while an explicit list silently omits every
    // pure test added later.
    run: () => must(['bun', 'test', '.test.ts'], { cwd: join(ROOT, 'tui') }),
  },
  {
    name: 'spec',
    summary: 'component and integration specs under Node and vitest',
    // On Node: these mount real Cordis trees and Ink's real input channels, and
    // testing those on a runtime we cannot ship to is how drift starts.
    run: () => must(['node', 'node_modules/vitest/vitest.mjs', 'run', '--config', 'tui/vitest.config.ts']),
  },
  {
    name: 'layout',
    summary: 'rendered layout invariants from DESIGN-LAYOUT.md',
    // Each scene renders through Ink and exits non-zero when a region overruns
    // the budget that keeps Ink off its screen-clearing path, when a frame
    // changes height while a turn runs, or when a rendered character rises
    // above 0x80, where a terminal and string-width can disagree about its width.
    run: async () => {
      for (const scene of ['frames', 'stability', 'realloop', 'overlays', 'stream', 'separation', 'chat', 'ascii']) {
        const child = Bun.spawn(['bun', `tui/prototype/${scene}.mjs`], { cwd: ROOT, stdout: 'ignore', stderr: 'inherit' })
        if (await child.exited !== 0) process.exit(1)
      }
      console.log('layout invariants: budgets, stability, and the ASCII vocabulary hold')
    },
  },
  {
    name: 'docs',
    summary: 'local Markdown links and anchors',
    run: () => must(['bun', 'tui/scripts/check-docs.ts']),
  },
]

/**
 * Run the selected validation targets in declaration order.
 *
 * @param names - the targets asked for; empty or `all` runs every one.
 */
async function check(names: string[]): Promise<void> {
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
  for (const name of wanted) {
    const target = CHECKS.find(item => item.name === name)!
    const step = performance.now()
    console.log(`--- ${target.name}: ${target.summary}`)
    await target.run()
    console.log(`--- ${target.name} passed in ${((performance.now() - step) / 1000).toFixed(1)}s`)
  }
  console.log(`check: ${wanted.length} target(s) passed in ${((performance.now() - started) / 1000).toFixed(1)}s`)
}

/**
 * Drive the built profile through a real terminal.
 *
 * @param args - forwarded to the driver, except `--no-build`.
 */
async function e2e(args: string[]): Promise<void> {
  const forwarded = args.filter(argument => argument !== '--no-build')
  if (!args.includes('--no-build') && !args.includes('--list') && !args.includes('--help')) await build()
  await must(['bun', 'tui/scripts/pty-smoke.ts', ...forwarded], { env: BUILT_ENV })
}

/** Print what this dispatcher can run. */
function usage(): void {
  console.log(`usage: tui/scripts/tui.ts <command> [arguments]

run the app
  dev [args]         component loop under Bun: no harness, no agent, no key
                     (--replay watches rows arrive, --locale zh checks a dictionary)
  source [args]      the TUI from src/ through tsx: fast, but tools do not work
  app [args]         the TUI from lib/: the production path, where tools work
  build              bundle the plugin and recorder for Node with production React

verify
  check [targets]    static and unit gate; \`check --list\` names the targets
  spec [args]        component and integration specs (vitest: -t NAME, --watch)
  unit [args]        pure modules and Bun tooling under bun test (--watch)
  e2e [args]         the built profile through a real terminal
                     (--list, --only NAME, --trace, --live, --no-build)
  perf [args]        built-profile latency and memory diagnostic; --mode development for a baseline
  verify             check + e2e: what to run before pushing

fixtures
  record "<task>"    record a session fixture through the headless profile (needs a key)

Arguments after the command reach the underlying tool unchanged.`)
}

const [command = 'help', ...args] = Bun.argv.slice(2)

switch (command) {
  case 'dev':
    await must(['bun', '--hot', 'tui/packages/harness/dev.tsx', ...args])
    break
  case 'source':
    // The product is Node, always: Bun cannot host an in-process dsh plugin.
    await must(['node', '--import', 'tsx/esm', 'apps/cli/src/bin.ts', '--profile', 'tui',
                '--patch', SOURCE_PATCH, ...args])
    break
  case 'app':
    await build()
    await must(['node', 'apps/cli/lib/bin.js', '--profile', 'tui', '--patch', BUILT_PATCH, ...args], { env: BUILT_ENV })
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
    await must(['node', 'node_modules/vitest/vitest.mjs', mode, '--config', 'tui/vitest.config.ts',
                ...args.filter(argument => argument !== '--watch')])
    break
  }
  case 'unit':
    await must(['bun', 'test', '.test.ts', ...args], { cwd: join(ROOT, 'tui') })
    break
  case 'e2e':
    await e2e(args)
    break
  case 'perf':
    await must(['bun', 'tui/packages/app/performance/terminal.perf.ts', ...args])
    break
  case 'verify':
    await check([])
    await e2e([])
    break
  case 'record':
    await build()
    await must(['node', 'apps/cli/lib/bin.js', '--profile', 'headless',
                '--patch', './tui/packages/harness/record.patch.yml', ...args], { env: BUILT_ENV })
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
