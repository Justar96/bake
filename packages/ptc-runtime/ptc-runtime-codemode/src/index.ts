/**
 * QuickJS provider for the PTC execution seam. Each program runs in a fresh
 * WebAssembly QuickJS VM on its own worker thread through pi-codemode; the
 * declared bindings are the program's only capability.
 * @module @deepseek-ai/dsh-ptc-runtime-codemode
 */

import { isAbsolute } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { CodemodeSandbox } from '@earendil-works/pi-codemode'
import { PtcRuntime } from '@deepseek-ai/dsh-ptc-runtime'
import type { PtcBindingNamespace, PtcJsonValue, PtcRunFailure, PtcRunRequest, PtcRunResult, PtcRunSpec } from '@deepseek-ai/dsh-ptc-runtime'
import { MAX_TIMER_DELAY_MS, clampTimeout } from '@deepseek-ai/dsh-timeout'
import { assertNever, snapshotJsonValue } from '@deepseek-ai/dsh-util-values'
import { validateBindings } from './bindings.ts'
import { OutputLedger, jsonValueBytesUpTo } from './output.ts'
import { CALL_GLOBAL, prepareProgram } from './program.ts'
import { MESSAGE_LIMIT_SIGNAL, OUTPUT_LIMIT_SIGNAL, programFailureText, withWorkerHeader } from './protocol.ts'
import type { ProgramLayout } from './protocol.ts'

/** Deployment-varying runtime bounds. */
export interface Config {
  /** Default elapsed deadline, including nested tool and approval waits. */
  timeoutMs?: number
  /** Maximum numeric elapsed budget accepted by resolve. */
  maxTimeoutMs?: number
  /** Combined serialized logs, completion and diagnostic byte cap. */
  maxOutputBytes?: number
  /** QuickJS heap limit for one program, in bytes. */
  maxMemoryBytes?: number
  /** Maximum serialized bytes of one binding call's arguments and of all outstanding binding arguments. */
  maxMessageBytes?: number
  /** Maximum simultaneous host binding calls accepted from a program. */
  maxPendingCalls?: number
}

type ResolvedConfig = Required<Config>
interface LiveRun { controller: AbortController; finished: Promise<void> }

/** Worker entry beside this module: TypeScript source in source runs, the bundle in builds. */
const WORKER_URL = new URL(import.meta.url.endsWith('.ts') ? './worker.ts' : './worker.js', import.meta.url)

function messageOf(error: unknown): string { return error instanceof Error ? error.message : String(error) }

/** QuickJS provider; programs reach nothing but their declared bindings. */
export class CodemodePtcRuntime extends PtcRuntime {
  static inject = []
  static Config: z<Config> = z.object({
    timeoutMs: z.number().default(120_000),
    maxTimeoutMs: z.number().default(600_000),
    maxOutputBytes: z.number().default(67_108_864),
    maxMemoryBytes: z.number().default(536_870_912),
    maxMessageBytes: z.number().default(134_217_728),
    maxPendingCalls: z.number().default(128),
  })
  readonly language = 'typescript'
  readonly isolation = 'worker-thread'
  override get executionInstructions(): string {
    return 'Each call runs in a fresh JavaScript sandbox with no state from earlier calls. The declared SDK bindings are the program\'s only access to files, processes, and the network: `import`, `require`, `process`, `fetch`, and timers such as `setTimeout` do not exist. Standard built-ins such as `JSON`, `Math`, `Date`, `RegExp`, `Map`, `Set`, and `Promise` are available, and `console.log` prints non-string values as JSON. A program that awaits a promise no pending binding call can settle fails immediately.'
  }
  private readonly config: ResolvedConfig
  private readonly live = new Set<LiveRun>()
  private disposed = false

  constructor(ctx: Context, config: Config) {
    super(ctx)
    this.config = config as ResolvedConfig
    for (const [key, value] of Object.entries(this.config)) {
      if (typeof value === 'number' && (!Number.isFinite(value) || value <= 0)) throw new Error(`dsh-ptc-runtime-codemode: ${key} must be positive and finite`)
    }
    for (const key of ['timeoutMs', 'maxTimeoutMs'] as const) {
      if (this.config[key] > MAX_TIMER_DELAY_MS) throw new Error(`dsh-ptc-runtime-codemode: ${key} exceeds the supported timer range`)
    }
    if (!Number.isSafeInteger(this.config.maxOutputBytes) || this.config.maxOutputBytes < 4) throw new Error('dsh-ptc-runtime-codemode: maxOutputBytes must be an integer of at least 4')
    for (const key of ['maxMemoryBytes', 'maxMessageBytes', 'maxPendingCalls'] as const) {
      if (!Number.isSafeInteger(this.config[key])) throw new Error(`dsh-ptc-runtime-codemode: ${key} must be an integer`)
    }
    ctx.effect(() => async () => {
      this.disposed = true
      const active = [...this.live]
      for (const run of active) run.controller.abort('runtime disposed')
      await Promise.all(active.map(run => run.finished))
    }, 'QuickJS ptc-runtime cleanup')
  }

  override get timeout(): { defaultMs: number; maxMs: number } {
    return { defaultMs: Math.min(this.config.timeoutMs, this.config.maxTimeoutMs), maxMs: this.config.maxTimeoutMs }
  }

  /**
   * Resolve an execution's directory and deadline.
   * @param request - Program, bindings, and optional cwd and deadline.
   * @returns Complete execution inputs with a capped numeric budget or an explicit null deadline.
   * @throws When the request carries a sandbox policy: programs have no file access to confine.
   */
  resolve(request: PtcRunRequest): PtcRunSpec {
    if (this.disposed) throw new Error('dsh-ptc-runtime-codemode: resolve after disposal')
    if (request.sandboxPolicy !== undefined) throw new Error('dsh-ptc-runtime-codemode: programs have no file access, so a sandbox policy is unsupported')
    const cwd = request.cwd ?? process.cwd()
    if (!isAbsolute(cwd)) throw new Error('dsh-ptc-runtime-codemode: cwd must be absolute')
    return {
      ...request,
      cwd,
      timeoutMs: request.timeoutMs === null ? null : clampTimeout(request.timeoutMs, this.config.timeoutMs, this.config.maxTimeoutMs, 'dsh-ptc-runtime-codemode: timeoutMs'),
    }
  }

  /**
   * Run a resolved program in a fresh QuickJS VM on its own worker thread.
   * @param spec - Inputs returned by resolve.
   * @returns Output and outcome after the worker has terminated.
   */
  async run(spec: PtcRunSpec): Promise<PtcRunResult> {
    if (this.disposed) throw new Error('dsh-ptc-runtime-codemode: run after disposal')
    if (spec.sandboxPolicy !== undefined) throw new Error('dsh-ptc-runtime-codemode: programs have no file access, so a sandbox policy is unsupported')
    if (!isAbsolute(spec.cwd) || (spec.timeoutMs !== null && (!Number.isFinite(spec.timeoutMs) || spec.timeoutMs <= 0 || spec.timeoutMs > this.config.maxTimeoutMs))) throw new Error('dsh-ptc-runtime-codemode: run requires resolved cwd and timeout')
    const bindings = [...validateBindings(spec).values()]
    const controller = new AbortController()
    const completion = Promise.withResolvers<void>()
    const live = { controller, finished: completion.promise }
    this.live.add(live)
    try {
      return await this.execute(spec, bindings, controller)
    } finally {
      this.live.delete(live)
      completion.resolve()
    }
  }

  private async execute(spec: PtcRunSpec, bindings: PtcBindingNamespace[], controller: AbortController): Promise<PtcRunResult> {
    const ledger = new OutputLedger(this.config.maxOutputBytes)
    let prepared: { code: string; layout: ProgramLayout }
    try {
      prepared = prepareProgram(spec.program, bindings)
    } catch (error: unknown) {
      return ledger.failure([], { kind: 'exception', message: messageOf(error) })
    }
    // pi-codemode reports every stop it did not choose as `aborted`; this records why the run was stopped.
    let stopped: PtcRunFailure | undefined
    const stop = (failure: PtcRunFailure): void => {
      stopped ??= failure
      controller.abort(failure.message)
    }
    const onAbort = (): void => { stop({ kind: 'abort', message: messageOf(spec.signal?.reason) }) }
    spec.signal?.addEventListener('abort', onAbort, { once: true })
    if (spec.signal?.aborted === true) onAbort()
    controller.signal.addEventListener('abort', () => { stop({ kind: 'abort', message: messageOf(controller.signal.reason) }) }, { once: true })

    let pending = 0
    let pendingBytes = 0
    const protocol = (message: string): never => {
      stop({ kind: 'protocol', message })
      throw new Error(message)
    }
    const call = async (input: unknown): Promise<PtcJsonValue> => {
      if (!Array.isArray(input) || input.length !== 3 || !Number.isSafeInteger(input[0]) || typeof input[1] !== 'string') return protocol('invalid binding call')
      const [index, name, payload] = input as [number, string, unknown]
      const functions = bindings[index]?.functions
      const fn = functions !== undefined && Object.hasOwn(functions, name) ? functions[name] : undefined
      if (typeof fn !== 'function') return protocol('program requested an undeclared binding')
      // Validated here: the program can reach the call global without the prelude.
      const args = snapshotJsonValue(payload as PtcJsonValue)
      if (args === undefined) return protocol('binding arguments must be lossless JSON')
      const bytes = jsonValueBytesUpTo(args, this.config.maxMessageBytes - pendingBytes)
      if (bytes === undefined || pending >= this.config.maxPendingCalls) return protocol('pending binding calls exceed configured limits')
      pending += 1
      pendingBytes += bytes
      try {
        const value = snapshotJsonValue(await fn(args))
        if (value === undefined) throw new Error('binding resolution must be lossless JSON')
        return value
      } finally {
        pending -= 1
        pendingBytes -= bytes
      }
    }

    try {
      const sandbox = new CodemodeSandbox({
        globals: [{ name: CALL_GLOBAL, execute: call }],
        memoryLimitBytes: this.config.maxMemoryBytes,
        workerUrl: WORKER_URL,
      })
      const result = await sandbox.execute(withWorkerHeader({
        maxOutputBytes: this.config.maxOutputBytes,
        maxMessageBytes: this.config.maxMessageBytes,
        ...prepared.layout,
      }, prepared.code), {
        signal: controller.signal,
        timeoutMs: spec.timeoutMs ?? Number.POSITIVE_INFINITY,
      })

      const logs: string[] = []
      for (const item of result.output) {
        if (item.type !== 'text') continue
        logs.push(item.text)
        if (!ledger.admit(item.text)) return ledger.limit(logs)
      }
      if (result.ok) {
        if (result.value === undefined) return ledger.success(logs)
        const envelope = result.value as unknown
        const value = Array.isArray(envelope) && envelope.length === 2 && envelope[0] === 0
          ? snapshotJsonValue(envelope[1] as PtcJsonValue)
          : undefined
        return value === undefined
          ? ledger.failure(logs, { kind: 'invalid-output', message: 'program completion must be lossless JSON' })
          : ledger.success(logs, value)
      }
      switch (result.error.kind) {
        case 'script':
          return ledger.failure(logs, { kind: 'exception', message: programFailureText(result.error, prepared.layout) })
        case 'timeout':
          return ledger.failure(logs, { kind: 'timeout', message: `execution deadline reached (${spec.timeoutMs}ms)` })
        case 'aborted':
          return ledger.failure(logs, stopped ?? { kind: 'abort', message: result.error.message })
        case 'sandbox':
          if (result.error.message === OUTPUT_LIMIT_SIGNAL) return ledger.limit(logs)
          if (result.error.message === MESSAGE_LIMIT_SIGNAL) return ledger.failure(logs, { kind: 'protocol', message: 'pending binding calls exceed configured limits' })
          return ledger.failure(logs, { kind: 'worker-exit', message: result.error.message })
        default:
          return assertNever(result.error.kind, 'pi-codemode error kind')
      }
    } finally {
      spec.signal?.removeEventListener('abort', onAbort)
    }
  }
}

export default CodemodePtcRuntime
