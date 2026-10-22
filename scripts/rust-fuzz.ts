#!/usr/bin/env bun
/**
 * Run the cargo-fuzz targets in `rust/fuzz` on its pinned nightly toolchain.
 *
 *   bun run fuzz:rust --list
 *   bun run fuzz:rust <target> [--seconds N] [--jobs N]
 *   bun run fuzz:rust --all [--seconds N] [--jobs N]
 *
 * Each target reads its committed seeds from `rust/fuzz/corpus/<target>`
 * and keeps the inputs a run grows in the ignored `rust/fuzz/generated/<target>`.
 * A crash leaves its input under `rust/fuzz/artifacts/<target>`; the run
 * names the file with the commands that reproduce and minimize it, and
 * exits nonzero.
 */
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, relative, resolve } from 'node:path'

export const FUZZ_DIR = resolve(import.meta.dirname, '../rust/fuzz')
export const DEFAULT_SECONDS = 60

export type FuzzRequest =
  | { readonly kind: 'list' }
  | { readonly kind: 'run'; readonly targets: readonly string[]; readonly seconds: number; readonly jobs: number }

const USAGE = 'usage: bun run fuzz:rust --list | <target> [--seconds N] [--jobs N] | --all [--seconds N] [--jobs N]'

/** The `[[bin]]` names of the fuzz crate's manifest, in manifest order. */
export function fuzzTargets(manifest: string): readonly string[] {
  const parsed = Bun.TOML.parse(manifest) as { bin?: readonly { name?: unknown }[] }
  return (parsed.bin ?? []).flatMap(bin => typeof bin.name === 'string' ? [bin.name] : [])
}

function positive(flag: string, value: string | undefined): number {
  if (value === undefined || !/^[1-9][0-9]*$/.test(value)) throw new Error(`${flag} takes a positive whole number\n${USAGE}`)
  return Number(value)
}

/** Read the command line against the known targets; throws with the usage on a mistake. */
export function parseFuzzArgs(args: readonly string[], known: readonly string[]): FuzzRequest {
  let list = false
  let all = false
  let seconds = DEFAULT_SECONDS
  let jobs = 1
  const targets: string[] = []
  for (let index = 0; index < args.length; index++) {
    const arg = args[index] ?? ''
    if (arg === '--list') list = true
    else if (arg === '--all') all = true
    else if (arg === '--seconds') seconds = positive(arg, args[++index])
    else if (arg === '--jobs') jobs = positive(arg, args[++index])
    else if (arg.startsWith('-')) throw new Error(`unknown option ${arg}\n${USAGE}`)
    else targets.push(arg)
  }
  if (list) {
    if (all || targets.length > 0 || args.length > 1) throw new Error(`--list takes no other arguments\n${USAGE}`)
    return { kind: 'list' }
  }
  if (all === (targets.length > 0) || targets.length > 1) throw new Error(`name one target or --all\n${USAGE}`)
  for (const target of targets) {
    if (!known.includes(target)) throw new Error(`unknown fuzz target ${target}; known: ${known.join(', ')}`)
  }
  return { kind: 'run', targets: all ? known : targets, seconds, jobs }
}

/** The cargo-fuzz command for one target, run from `rust/fuzz`: grown inputs first, so libFuzzer writes there, then the seeds. */
export function fuzzCommand(target: string, seconds: number, jobs: number): readonly string[] {
  return [
    'cargo', 'fuzz', 'run', target,
    ...jobs > 1 ? ['--jobs', String(jobs)] : [],
    `generated/${target}`, `corpus/${target}`,
    '--', `-max_total_time=${seconds}`, '-print_final_stats=1',
  ]
}

/** cargo-fuzz has no `--locked`; this fails first when `rust/fuzz/Cargo.lock` would change. */
export const LOCK_CHECK: readonly string[] = ['cargo', 'fetch', '--locked']

/** The input files libFuzzer reports writing (crash, leak, timeout, or out-of-memory), in order. */
export function crashArtifacts(output: string): readonly string[] {
  return [...output.matchAll(/Test unit written to (\S+)/g)].flatMap(match => match[1] === undefined ? [] : [match[1]])
}

export interface FuzzStats { readonly cov: number; readonly ft: number; readonly execsPerSecond: number | undefined }

/**
 * The last coverage line of a run: edges (`cov`), features (`ft`), and executions per second.
 * With `--jobs`, libFuzzer's fork mode prints the merged totals as `#N: cov: ...` lines between
 * its workers' own lines, so those totals win.
 */
export function lastStats(output: string): FuzzStats | undefined {
  const lines = output.split(/[\r\n]+/).filter(line => /\bcov: \d+ ft: \d+/.test(line))
  const merged = lines.filter(line => /^#\d+: cov: /.test(line))
  const line = (merged.length > 0 ? merged : lines).at(-1)
  if (line === undefined) return undefined
  const number = (pattern: RegExp) => {
    const found = pattern.exec(line)?.[1]
    return found === undefined ? undefined : Number(found)
  }
  return { cov: number(/\bcov: (\d+)/) ?? 0, ft: number(/\bft: (\d+)/) ?? 0, execsPerSecond: number(/exec\/s: (\d+)/) }
}

/** How to look at a crash: reproduce it, then minimize it, from the repository root. */
export function crashReport(target: string, artifacts: readonly string[], root: string): string {
  return artifacts.map((artifact) => {
    const path = relative(join(root, 'rust/fuzz'), resolve(join(root, 'rust/fuzz'), artifact))
    return [
      `fuzz:rust: ${target} crashed on rust/fuzz/${path}`,
      `  reproduce: cd rust/fuzz && cargo fuzz run ${target} ${path}`,
      `  minimize:  cd rust/fuzz && cargo fuzz tmin ${target} ${path}`,
    ].join('\n')
  }).join('\n')
}

/** Copy a child's stream to `sink` and keep its text. */
async function tee(stream: ReadableStream<Uint8Array>, sink: NodeJS.WriteStream): Promise<string> {
  const decoder = new TextDecoder()
  let text = ''
  for await (const chunk of stream) {
    sink.write(chunk)
    text += decoder.decode(chunk, { stream: true })
  }
  return text + decoder.decode()
}

/** The environment without an inherited toolchain override, which would bypass `rust/fuzz/rust-toolchain.toml`. */
function toolchainEnv(): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = { ...process.env }
  delete env['RUSTUP_TOOLCHAIN']
  return env
}

interface TargetResult {
  readonly target: string
  readonly code: number
  readonly artifacts: readonly string[]
  readonly stats: FuzzStats | undefined
}

async function runTarget(target: string, seconds: number, jobs: number, scratch: string): Promise<TargetResult> {
  mkdirSync(join(FUZZ_DIR, 'generated', target), { recursive: true })
  const env = { ...toolchainEnv(), TMPDIR: scratch }
  const command = fuzzCommand(target, seconds, jobs)
  console.log(`fuzz:rust: ${target} for ${seconds}s (${command.join(' ')})`)
  const child = Bun.spawn([...command], { cwd: FUZZ_DIR, env, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' })
  const [out, err, code] = await Promise.all([tee(child.stdout, process.stdout), tee(child.stderr, process.stderr), child.exited])
  const output = `${out}\n${err}`
  return { target, code, artifacts: crashArtifacts(output), stats: lastStats(output) }
}

if (import.meta.main) {
  const root = resolve(import.meta.dirname, '..')
  const known = fuzzTargets(readFileSync(join(FUZZ_DIR, 'Cargo.toml'), 'utf8'))
  let request: FuzzRequest
  try {
    request = parseFuzzArgs(process.argv.slice(2), known)
  } catch (error) {
    console.error(`fuzz:rust: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(2)
  }
  if (request.kind === 'list') {
    for (const target of known) console.log(target)
    process.exit(0)
  }
  if (Bun.which('cargo-fuzz') === null) {
    console.error('fuzz:rust: cargo-fuzz is not installed; run `cargo install --locked cargo-fuzz@0.13.2`.')
    process.exit(1)
  }
  const locked = Bun.spawnSync([...LOCK_CHECK], { cwd: FUZZ_DIR, env: toolchainEnv(), stdout: 'inherit', stderr: 'inherit' })
  if (locked.exitCode !== 0) {
    console.error('fuzz:rust: rust/fuzz/Cargo.lock is out of date with rust/fuzz/Cargo.toml.')
    process.exit(1)
  }
  // libFuzzer's fork mode and the session target write scratch files; they
  // go here and are removed with it.
  const scratch = mkdtempSync(join(tmpdir(), 'bake-fuzz-'))
  const results: TargetResult[] = []
  try {
    for (const target of request.targets) results.push(await runTarget(target, request.seconds, request.jobs, scratch))
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
  console.log('\nfuzz:rust summary')
  for (const { target, code, stats } of results) {
    const rate = stats?.execsPerSecond === undefined ? '' : ` exec/s ${stats.execsPerSecond}`
    const coverage = stats === undefined ? 'no coverage line' : `cov ${stats.cov} ft ${stats.ft}${rate}`
    console.log(`  ${target}: ${code === 0 ? 'ok' : `exit ${code}`}, ${coverage}`)
  }
  const failed = results.filter(result => result.code !== 0)
  for (const { target, artifacts } of failed) {
    if (artifacts.length > 0) console.error(crashReport(target, artifacts, root))
    else console.error(`fuzz:rust: ${target} failed without a crash input; see its output above.`)
  }
  process.exit(failed.length === 0 ? 0 : 1)
}
