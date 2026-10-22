/**
 * The two kinds of measured build and how each is started on a workload.
 *
 * `native:<path>` runs a `bake` binary in inline mode, so resumed history is
 * written to the terminal as 0.3 writes it; a fullscreen frame shows only its
 * last rows and could never show every history marker. The workload is a Pi
 * session opened with `--session`, and the faux model is a `models.json`
 * provider.
 *
 * `ts:<worktree>` runs Bake 0.3's Node launcher from a built checkout,
 * `node <worktree>/apps/cli/lib/bin.js --profile tui --patch <file>`. The
 * patch resumes a seeded 0.3 Session log and routes the default model to the
 * faux model through the `llm-pi-ai` adapter; nothing from that checkout is
 * imported here.
 */
import { createHash } from 'node:crypto'
import { existsSync, readFileSync, realpathSync } from 'node:fs'
import { mkdir, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve, sep } from 'node:path'
import { seedPi, PERF_MODEL, PERF_PROVIDER, PI_SESSION_ID } from './seed-pi.ts'
import { seedSession03, SESSION03_ID } from './seed-session03.ts'
import { STREAM_DONE } from './model.ts'
import type { HistoryDimensions, WorkloadSpec } from './workloads.ts'

export type ArmKind = 'native' | 'ts'

/** An arm as given on the command line. */
export interface ArmSpec {
  /** The command-line text, which names the arm in the report. */
  readonly label: string
  readonly kind: ArmKind
  /** The binary for `native`; the checkout for `ts`. */
  readonly path: string
}

/**
 * Parse `native:<path>` or `ts:<worktree>`.
 * @throws for another kind or an empty path.
 */
export function parseArm(text: string, cwd = process.cwd()): ArmSpec {
  const match = /^(native|ts):(.+)$/u.exec(text)
  const [, kind, path] = match ?? []
  if ((kind !== 'native' && kind !== 'ts') || path === undefined) throw new Error(`--arm must be native:<bake binary> or ts:<v0.3.8 worktree>, got ${JSON.stringify(text)}`)
  return { label: text, kind, path: resolve(cwd, path) }
}

/** A seeded sample, ready to start. */
export interface Launch {
  readonly argv: readonly string[]
  readonly env: Record<string, string>
  readonly cwd: string
  /** Sizes of the seeded history, with the format's own counts. */
  readonly dimensions: HistoryDimensions & { fileBytes: number }
}

/** Where one sample's files live. */
export interface SampleDirs {
  /** The sample's private root. */
  readonly root: string
  /**
   * `BAKE_HOME`: a new empty directory for a cold sample, a copy of the
   * arm's prepared home for a warm one.
   */
  readonly home: string
  /** Process caches (`TMPDIR`): private to a cold sample, shared by an arm's warm samples. */
  readonly cache: string
}

/** Build identity recorded in the report. */
export type ArmBuild = Record<string, unknown>

function sha256(path: string): string {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function run(argv: readonly string[], cwd?: string): string | undefined {
  const result = Bun.spawnSync([...argv], { ...cwd === undefined ? {} : { cwd }, stdout: 'pipe', stderr: 'pipe' })
  return result.success ? result.stdout.toString().trim() : undefined
}

/** The environment both arms share; nothing is inherited but `PATH`. */
function baseEnv(dirs: SampleDirs, home: string, user: string): Record<string, string> {
  return { PATH: process.env.PATH ?? '/usr/bin:/bin', TERM: 'xterm-256color', LANG: 'C.UTF-8', HOME: user, TMPDIR: dirs.cache, BAKE_HOME: home }
}

/**
 * The Cargo profile of a binary, from its path under a target directory:
 * `target/<profile>/bake`, `target/<triple>/<profile>/bake`, or a
 * `deps` or `examples` directory below either.
 * @returns null when the path has no `target` directory in that shape.
 */
export function cargoProfile(binary: string): string | null {
  const parts = binary.split(sep)
  const target = parts.lastIndexOf('target')
  if (target < 0) return null
  const below = parts.slice(target + 1, -1).filter((part, index, all) => !(index === all.length - 1 && (part === 'deps' || part === 'examples')))
  return below.length === 1 || below.length === 2 ? below.at(-1) ?? null : null
}

/** How a native binary's profile is checked before a run. */
export interface ArmOptions {
  /** The Node executable a `ts` arm runs. */
  readonly node?: string
  /** Measure a native binary that is not a release build, for a run that proposes and checks no budget. */
  readonly allowNonRelease?: boolean
}

/** One arm: how to identify its build, start a sample, and see a turn settle. */
export interface Arm {
  readonly spec: ArmSpec
  /** Fail early when the build is missing, or a native build is not a release build and that was not allowed. */
  check(): void
  build(): ArmBuild
  /**
   * Seed `workload` under `dirs.home` and `dirs.root` and return the command that resumes it.
   * @param modelUrl - the faux model's base URL.
   */
  prepare(workload: WorkloadSpec, dirs: SampleDirs, modelUrl: string): Promise<Launch>
  /** Whether the visible text since the prompt was submitted shows the turn settled. */
  settled(sinceSubmit: string): boolean
}

const RELEASE_HINT = 'build one with: cd rust && cargo build --locked --release -p bake-cli'

class NativeArm implements Arm {
  constructor(readonly spec: ArmSpec, private readonly allowNonRelease: boolean) {}

  /** The Cargo profile, from the binary's resolved path; null when the path does not show one. */
  get profile(): string | null {
    return existsSync(this.spec.path) ? cargoProfile(realpathSync(this.spec.path)) : null
  }

  check(): void {
    if (!existsSync(this.spec.path)) throw new Error(`native arm: no binary at ${this.spec.path}; ${RELEASE_HINT}`)
    const profile = this.profile
    if (profile === 'release' || this.allowNonRelease) return
    const found = profile === null ? 'its Cargo profile cannot be determined from its path (expected target/[<triple>/]<profile>/bake)' : `it is a ${profile} build`
    throw new Error(`native arm: ${this.spec.path}: ${found}; D18 budgets compare release builds, so ${RELEASE_HINT}, or pass --allow-non-release for a run that sets no budget`)
  }

  build(): ArmBuild {
    const profile = this.profile
    return {
      binary: this.spec.path, sha256: sha256(this.spec.path), bytes: Bun.file(this.spec.path).size, version: run([this.spec.path, '--version']) ?? null, screen: 'inline',
      profile, profileSource: profile === null ? 'unknown: the path shows no target/<profile>/ directory' : 'the binary\'s resolved path under target/<profile>/',
      budgetEligible: profile === 'release',
    }
  }

  async prepare(workload: WorkloadSpec, dirs: SampleDirs, modelUrl: string): Promise<Launch> {
    const home = dirs.home
    const user = join(dirs.root, 'user')
    const cwd = join(dirs.root, 'workspace')
    for (const dir of [home, user, cwd]) await mkdir(dir, { recursive: true })
    await writeFile(join(home, 'models.json'), JSON.stringify({
      providers: { [PERF_PROVIDER]: { baseUrl: modelUrl, api: 'openai-responses', apiKey: 'perf-key', models: [{ id: PERF_MODEL, name: 'Synthetic', contextWindow: 1_000_000_000 }] } },
    }))
    // 0.3's terminal bundle runs compaction only on request; retries would hide a failed request.
    await writeFile(join(home, 'settings.json'), JSON.stringify({ compaction: { enabled: false }, retry: { enabled: false } }))
    const seed = seedPi(workload, cwd)
    const argv = [this.spec.path, '--inline']
    if (seed.bytes.length > 0) {
      // Pi's default directory for this working directory, with links resolved as `bake` resolves them.
      const sessions = join(home, 'sessions', `--${cwd.replace(/^[/\\]/u, '').replace(/[/\\:]/gu, '-')}--`)
      await mkdir(sessions, { recursive: true })
      const file = join(sessions, `2023-11-14T22-13-20-000Z_${PI_SESSION_ID}.jsonl`)
      await writeFile(file, seed.bytes)
      argv.push('--session', file)
    }
    return { argv, env: baseEnv(dirs, home, user), cwd, dimensions: seed.dimensions }
  }

  settled(sinceSubmit: string): boolean {
    const done = sinceSubmit.lastIndexOf(STREAM_DONE)
    return done >= 0 && sinceSubmit.indexOf('✓ Completed', done) > done
  }
}

/** 0.3's composer hints (`apps/tui/packages/ui/src/copy.ts` at v0.3.8). */
const TS_SEND_HINT = 'Enter sends'
const TS_INTERRUPT_HINT = 'Esc interrupts'

class TsArm implements Arm {
  constructor(readonly spec: ArmSpec, private readonly node: string) {}

  private get launcher(): string { return join(this.spec.path, 'apps/cli/lib/bin.js') }

  check(): void {
    if (!existsSync(this.launcher)) throw new Error(`ts arm: no built launcher at ${this.launcher}; run bun install --frozen-lockfile, bun run build, and bun run build:native-system in that worktree`)
  }

  build(): ArmBuild {
    const app = join(this.spec.path, 'apps/tui/packages/app/lib/index.js')
    return {
      worktree: this.spec.path,
      revision: run(['git', 'rev-parse', 'HEAD'], this.spec.path) ?? null,
      describe: run(['git', 'describe', '--tags', '--always', '--dirty'], this.spec.path) ?? null,
      launcherSha256: sha256(this.launcher),
      appSha256: existsSync(app) ? sha256(app) : null,
      lockSha256: existsSync(join(this.spec.path, 'bun.lock')) ? sha256(join(this.spec.path, 'bun.lock')) : null,
      node: this.node,
      nodeVersion: run([this.node, '--version']) ?? null,
    }
  }

  async prepare(workload: WorkloadSpec, dirs: SampleDirs, modelUrl: string): Promise<Launch> {
    const home = dirs.home
    const user = join(dirs.root, 'user')
    const cwd = join(dirs.root, 'workspace')
    for (const dir of [home, user, cwd]) await mkdir(dir, { recursive: true })
    const sessions = join(home, 'sessions')
    const seed = seedSession03(workload, cwd)
    if (seed.bytes.length > 0) {
      const file = join(sessions, seed.relativePath)
      await mkdir(dirname(file), { recursive: true })
      await writeFile(file, seed.bytes)
    }
    const patch = join(dirs.root, 'perf.patch.json')
    await writeFile(patch, JSON.stringify([
      { id: 'session-title-llm', disabled: true },
      { id: 'llm-pi-ai', config: { providers: { [PERF_PROVIDER]: { displayName: 'Perf', api: 'openai-responses', baseURL: modelUrl, apiKeyEnv: 'BAKE_PERF_API_KEY',
        models: [{ id: PERF_MODEL, contextWindow: 1_000_000_000 }] } } } },
      { id: 'agent-default-model', config: { provider: PERF_PROVIDER, model: PERF_MODEL } },
      { id: 'session-persistence-jsonl', config: { root: sessions, compression: 'none' } },
      { id: 'tui-runner', config: seed.bytes.length > 0 ? { resume: SESSION03_ID } : {} },
    ]))
    const agents = join(dirs.root, 'agents')
    return {
      argv: [this.node, this.launcher, '--profile', 'tui', '--patch', patch],
      env: { ...baseEnv(dirs, home, user), DSH_HOME: home, BAKE_AGENTS_HOME: agents, DSH_AGENTS_HOME: agents, BAKE_PERF_API_KEY: 'perf-key', NODE_ENV: 'production' },
      cwd,
      dimensions: seed.dimensions,
    }
  }

  settled(sinceSubmit: string): boolean {
    // The draft typed mid-stream is still there, so idle is the send hint taking the interrupt hint's place.
    const done = sinceSubmit.lastIndexOf(STREAM_DONE)
    const idle = sinceSubmit.lastIndexOf(TS_SEND_HINT)
    return done >= 0 && idle > done && idle > sinceSubmit.lastIndexOf(TS_INTERRUPT_HINT)
  }
}

/** The arm for `spec`. */
export function createArm(spec: ArmSpec, options: ArmOptions = {}): Arm {
  return spec.kind === 'native' ? new NativeArm(spec, options.allowNonRelease ?? false) : new TsArm(spec, options.node ?? 'node')
}

/** A short file-system-safe name for an arm. */
export function armSlug(spec: ArmSpec, index: number): string {
  return `${index}-${spec.kind}-${basename(spec.path).replace(/[^A-Za-z0-9._-]/gu, '_')}`
}
