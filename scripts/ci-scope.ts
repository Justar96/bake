/**
 * Decide which CI jobs an event runs. The `ci.yml` workflow's `scope` job
 * runs this and the other jobs read its outputs.
 *
 * - A pull request into the Rust 0.4.0 line ({@link RUST_LINE}), directly or
 *   through a stack whose trunk it is, runs the native job on every OS. It
 *   runs TypeScript checks on Linux only, and only as far as its files need:
 *   none for Rust sources alone, the static half for Markdown, and both parts
 *   for anything else. The pull request that syncs the line into `develop`
 *   runs everything.
 * - A release pull request from `develop` into `main` runs Linux alone; see
 *   the comment in `ci.yml`.
 * - Every other event runs every job on every platform.
 *
 * Any failure to read the pull request's chain or files plans the full set.
 * @module scripts/ci-scope
 */

import { appendFileSync } from 'node:fs'

/** The long-lived integration branch Rust port pull requests target. */
export const RUST_LINE = 'rust/0.4.0'

/** Branches a stack can bottom out on other than the Rust line. */
const TRUNKS = new Set(['develop', 'main'])

/** How far down a stack {@link onRustLine} follows bases before giving up. */
const MAX_STACK = 20

/** Paths the native job alone checks: the Cargo workspace and its drivers. */
const RUST_ONLY = /^(?:rust\/|scripts\/rust-conformance\/|scripts\/rust-preview-pty\.ts$)/u

/** Files the static checks cover: links, the roadmap ledger, and the changelog. */
const MARKDOWN = /\.md$/u

const ALL_OS = ['ubuntu-latest', 'macos-latest'] as const
const RUST_OS = ['ubuntu-latest', 'macos-latest', 'windows-latest'] as const

/** One preflight job: where it runs and what it runs. */
export interface PreflightJob {
  readonly os: string
  readonly name: string
  readonly args: string
}

const CHECKS = { name: 'checks', args: '--full --skip runtime,native' }
const RUNTIME = { name: 'runtime', args: '--full --only build,runtime' }
/** The static half without the native group, for Markdown-only changes. */
const STATIC = { name: 'checks', args: '--fast --skip native' }

/** The jobs an event runs, and why. */
export interface Plan {
  readonly preflight: readonly PreflightJob[]
  readonly rust: readonly string[]
  readonly windowsRelease: boolean
  readonly reason: string
}

/** What a pull request's files ask of the TypeScript checks. */
export type TypeScriptNeed = 'none' | 'static' | 'full'

/**
 * @param files - paths the pull request changes, relative to the repository.
 * @returns `none` when every file is Rust-only, `static` when the rest are
 *   Markdown, and `full` otherwise, including for an empty list, which says
 *   nothing about what changed.
 */
export function typeScriptNeed(files: readonly string[]): TypeScriptNeed {
  if (files.length === 0) return 'full'
  const rest = files.filter(file => !RUST_ONLY.test(file))
  if (rest.length === 0) return 'none'
  return rest.every(file => MARKDOWN.test(file)) ? 'static' : 'full'
}

/** The event as the workflow reports it. */
export interface Event {
  readonly name: string
  readonly baseRef?: string | undefined
  readonly headRef?: string | undefined
  /** Whether the pull request's head is in this repository. */
  readonly sameRepository: boolean
}

const jobs = (os: readonly string[], parts: readonly { name: string; args: string }[]): PreflightJob[] =>
  os.flatMap(each => parts.map(part => ({ os: each, ...part })))

/**
 * @param event - the triggering event.
 * @param rustLine - whether a pull request's stack bottoms out on {@link RUST_LINE}.
 * @param files - the pull request's changed files, or undefined when unknown.
 */
export function plan(event: Event, rustLine: boolean, files: readonly string[] | undefined): Plan {
  if (event.name === 'pull_request' && event.baseRef === 'main' && event.headRef === 'develop' && event.sameRepository) {
    return {
      preflight: jobs(['ubuntu-latest'], [CHECKS, RUNTIME]), rust: ['ubuntu-latest'], windowsRelease: false,
      reason: 'a release pull request from develop: Linux alone',
    }
  }
  if (event.name === 'pull_request' && rustLine) {
    const need = files === undefined ? 'full' : typeScriptNeed(files)
    const parts = { none: [], static: [STATIC], full: [CHECKS, RUNTIME] }[need]
    return {
      preflight: jobs(['ubuntu-latest'], parts), rust: [...RUST_OS], windowsRelease: false,
      reason: `a pull request into ${RUST_LINE}: native checks on every OS, TypeScript checks: ${need}${files === undefined ? ' (changed files unknown)' : ''}`,
    }
  }
  return {
    preflight: jobs(ALL_OS, [CHECKS, RUNTIME]), rust: [...RUST_OS], windowsRelease: true,
    reason: 'every job on every platform',
  }
}

/**
 * Follow a pull request's base down its stack: the base of the open pull
 * request whose head is the current base, until a trunk is reached.
 * @param base - the pull request's own base branch.
 * @param baseOf - the base of the one open pull request from a branch, or
 *   undefined when there is none or more than one.
 * @returns whether the stack's trunk is {@link RUST_LINE}.
 */
export function onRustLine(base: string, baseOf: (head: string) => string | undefined): boolean {
  const seen = new Set<string>()
  let ref: string | undefined = base
  while (ref !== undefined && seen.size < MAX_STACK) {
    if (ref === RUST_LINE) return true
    if (TRUNKS.has(ref) || seen.has(ref)) return false
    seen.add(ref)
    ref = baseOf(ref)
  }
  return false
}

/** Runs `gh` and returns its standard output, or undefined when it fails. */
function gh(args: readonly string[]): string | undefined {
  try {
    const result = Bun.spawnSync(['gh', ...args], { stdout: 'pipe', stderr: 'pipe' })
    return result.exitCode === 0 ? result.stdout.toString() : undefined
  } catch {
    return undefined
  }
}

function main(): void {
  const env = process.env
  const repository = env.GITHUB_REPOSITORY ?? ''
  const event: Event = {
    name: env.GITHUB_EVENT_NAME ?? '',
    baseRef: env.BASE_REF || undefined,
    headRef: env.HEAD_REF || undefined,
    sameRepository: env.HEAD_REPOSITORY === repository,
  }
  const pull = env.PR_NUMBER
  const isPull = event.name === 'pull_request' && event.baseRef !== undefined && pull !== undefined && pull !== ''
  const baseOf = (head: string): string | undefined => {
    const out = gh(['pr', 'list', '--repo', repository, '--head', head, '--state', 'open', '--json', 'baseRefName', '--jq', '.[].baseRefName'])
    const bases = out?.split('\n').filter(Boolean) ?? []
    return bases.length === 1 ? bases[0] : undefined
  }
  const rustLine = isPull && onRustLine(event.baseRef ?? '', baseOf)
  const files = rustLine
    ? gh(['pr', 'diff', pull ?? '', '--repo', repository, '--name-only'])?.split('\n').filter(Boolean)
    : undefined
  const chosen = plan(event, rustLine, files)
  const lines = [
    `preflight=${JSON.stringify(chosen.preflight)}`,
    `rust=${JSON.stringify(chosen.rust)}`,
    `windows_release=${String(chosen.windowsRelease)}`,
  ]
  console.log(`${chosen.reason}\n${lines.join('\n')}`)
  if (env.GITHUB_OUTPUT !== undefined) appendFileSync(env.GITHUB_OUTPUT, `${lines.join('\n')}\n`)
  if (env.GITHUB_STEP_SUMMARY !== undefined) appendFileSync(env.GITHUB_STEP_SUMMARY, `CI scope: ${chosen.reason}\n`)
}

if (import.meta.main) main()
