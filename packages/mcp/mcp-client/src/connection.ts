/**
 * Connection supervisor: owns the MCP client/transport generations for one
 * plugin instance, keeps the harness tool registry in sync with the live
 * generation, and — when the connection drops — restarts the configured
 * server with bounded exponential backoff.
 *
 * One outage shares one attempt budget (`maxAttempts` consecutive failed
 * attempts, delays doubling from `initialDelayMs` up to `maxDelayMs`). A
 * connection that stays up past the stability window closes the outage, so
 * the next disconnect starts a fresh budget while a crash-looping server —
 * even one whose connects briefly succeed — still exhausts the cap instead of
 * restarting forever. Exhaustion unregisters the server's tools and stops;
 * disposal (including HMR) is the only way back from that state.
 *
 * @module
 */

import { McpClient, type McpTransport } from '@earendil-works/pi-mcp'
import type { Context } from '@deepseek-ai/cordis'
import { assertNever, type JsonValue } from '@deepseek-ai/dsh-util-values'
import type { ServerContext } from './server-context.ts'
import { MAX_TIMER_DELAY_MS } from '@deepseek-ai/dsh-timeout'
import { createTransport } from './transport.ts'
import { parseReadResourceResult, resourceListResult, resourceTemplateListResult } from './protocol.ts'
import { syncTools } from './tools.ts'
import type { ToolBridgeOptions, ToolDisposers } from './tools.ts'
import type { Config } from './index.ts'

/** Automatic reconnect policy for one MCP server connection. */
export interface ReconnectConfig {
  /** Reconnect automatically after a lost connection (default true). */
  enabled?: boolean
  /** First reconnect delay in milliseconds; doubles per consecutive failed attempt (default 500). */
  initialDelayMs?: number
  /** Backoff ceiling in milliseconds; also the uptime after which the attempt budget resets (default 30000). */
  maxDelayMs?: number
  /** Consecutive failed attempts per outage before giving up for good (default 10). */
  maxAttempts?: number
}

/** Defaults shared by the Config schema and {@link resolveReconnectPolicy}. */
export const RECONNECT_DEFAULTS: Required<ReconnectConfig> = Object.freeze({
  enabled: true,
  initialDelayMs: 500,
  maxDelayMs: 30_000,
  maxAttempts: 10,
})

/** Default UTF-8 byte limit for attributed server instructions. */
export const DEFAULT_MAX_INSTRUCTION_BYTES = 32_768

// pi-mcp's stdio close ends stdin, sends SIGTERM to the server's process group
// after 500 ms, and SIGKILL 2 s later. Keep the remaining time for the
// process-close event that proves the old generation is gone; timing out fails
// closed instead of overlapping children.
const GENERATION_CLOSE_TIMEOUT_MS = 5_000

// Budget for `initialize` and discovery requests, which have no per-call
// config. Tool calls and resource requests use `toolCallTimeoutMs`.
const PROTOCOL_REQUEST_TIMEOUT_MS = 60_000

/** Fully resolved reconnect policy captured at plugin load. */
export type ResolvedReconnectPolicy = Readonly<Required<ReconnectConfig>>

/**
 * The one explicit resolve step from raw reconnect config to the policy the
 * supervisor runs. Programmatic construction may bypass Schemastery
 * normalization, so every default and bound is re-judged here — misconfiguration
 * fails the plugin instance at load.
 *
 * @param config - Raw `reconnect` config; omission uses the defaults.
 * @param path - Diagnostic prefix naming the config location in thrown messages.
 * @returns The frozen resolved policy.
 */
export function resolveReconnectPolicy(config: ReconnectConfig | undefined, path: string): ResolvedReconnectPolicy {
  if (config !== undefined) {
    for (const key of Object.keys(config)) {
      if (!Object.hasOwn(RECONNECT_DEFAULTS, key)) throw new Error(`${path}.${key} is not a reconnect option`)
    }
  }
  const enabled = config?.enabled ?? RECONNECT_DEFAULTS.enabled
  const initialDelayMs = config?.initialDelayMs ?? RECONNECT_DEFAULTS.initialDelayMs
  const maxDelayMs = config?.maxDelayMs ?? RECONNECT_DEFAULTS.maxDelayMs
  const maxAttempts = config?.maxAttempts ?? RECONNECT_DEFAULTS.maxAttempts
  /* jscpd:ignore-start — domain-specific delay validation parallels llm retry-policy; not extractable */
  if (!Number.isFinite(initialDelayMs) || initialDelayMs <= 0 || initialDelayMs > MAX_TIMER_DELAY_MS) {
    throw new Error(`${path}.initialDelayMs must be a positive finite number no greater than ${MAX_TIMER_DELAY_MS}`)
  }
  if (!Number.isFinite(maxDelayMs) || maxDelayMs <= 0 || maxDelayMs > MAX_TIMER_DELAY_MS) {
    throw new Error(`${path}.maxDelayMs must be a positive finite number no greater than ${MAX_TIMER_DELAY_MS}`)
  }
  if (initialDelayMs > maxDelayMs) {
    throw new Error(`${path}.initialDelayMs must be less than or equal to maxDelayMs`)
  }
  if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
    throw new Error(`${path}.maxAttempts must be a positive integer`)
  }
  /* jscpd:ignore-end */
  return Object.freeze({ enabled, initialDelayMs, maxDelayMs, maxAttempts })
}

/** Result from the initial connection attempt, for startup-await semantics. */
export interface ConnectionOutcome {
  /** If the initial connection or tool sync failed, the error; otherwise absent. */
  error?: unknown
}

/** Handle for one plugin instance's supervised connection. */
export interface ConnectionHandle extends ServerContext {
  /**
   * Settles when the first connection attempt completes (success or failure).
   * The supervisor enters its reconnect loop regardless; the caller decides
   * whether a failed startup is fatal via `failOnStartupError`.
   */
  ready: Promise<ConnectionOutcome>
  /**
   * Stop reconnection, close the negotiating transport or live client, wait
   * for the in-flight attempt and queued tool syncs to quiesce, then
   * unregister every tool this server still owns.
   */
  dispose(): Promise<void>
}

/**
 * Start the supervised connection for one MCP server and keep it alive per
 * the reconnect policy.
 *
 * @param ctx - Cordis context providing the `tools` registry and logger.
 * @param config - Resolved plugin config selecting the transport and server identity.
 * @param policy - Resolved reconnect policy from {@link resolveReconnectPolicy}.
 * @returns Handle with a `ready` promise for startup-await and a `dispose` for teardown.
 */
export function startConnection(ctx: Context, config: Config, policy: ResolvedReconnectPolicy): ConnectionHandle {
  const label = `mcp-client(${config.serverName})`
  const incompleteDisposalMessage = `${label}: transport closure could not be confirmed during disposal — server shutdown may be incomplete`
  const opts: ToolBridgeOptions = {
    registrationFailure: 'contain',
    serverName: config.serverName,
    toolCallTimeoutMs: config.toolCallTimeoutMs,
  }
  // The initial sync uses 'throw' when failOnStartupError is configured, so
  // a registration conflict propagates to the startup-await path. Re-syncs
  // and reconnect syncs always contain conflicts.
  const startupOpts: ToolBridgeOptions = config.failOnStartupError
    ? { ...opts, registrationFailure: 'throw' }
    : opts

  let disposed = false
  const maxInstructionBytes = config.maxInstructionBytes ?? DEFAULT_MAX_INSTRUCTION_BYTES
  let serverInstructions = ''
  /** Current generation: the connecting or connected client; undefined during backoff waits and after final failure. */
  let client: McpClient | undefined
  /** Transport-aware close operation paired with {@link client}. */
  let closeClient: (() => Promise<boolean>) | undefined
  /** Live tool registrations owned by this server; only {@link enqueueSync} and dispose swap it. */
  let disposers: ToolDisposers = new Map()
  let reconnectTimer: NodeJS.Timeout | undefined
  /** Consecutive failed connection attempts within the current outage. */
  let failedAttempts = 0
  /** When the current generation finished connect + initial sync; undefined while down. */
  let connectedAt: number | undefined
  /** The real error from the first connection attempt, for startup-await diagnostics. */
  let firstAttemptError: unknown

  /** A generation may act only while it is the current one on a live plugin. */
  const isCurrent = (generation: McpClient): boolean => !disposed && client === generation

  /**
   * Serializes every syncTools call — initial syncs and notification re-syncs
   * across all generations — so two syncs can never interleave their
   * dispose-previous/register-next swap (which would double-dispose one
   * generation and leak another).
   */
  let syncChain: Promise<void> = Promise.resolve()
  function enqueueSync(generation: McpClient, syncOpts: ToolBridgeOptions = opts): Promise<void> {
    const run = syncChain.then(async () => {
      if (!isCurrent(generation)) return
      disposers = await syncTools(generation, ctx, syncOpts, disposers)
    })
    // The chain tail must survive a failed sync; the enqueuing caller owns reporting.
    syncChain = run.catch(() => {})
    return run
  }

  /** One disconnect decision per generation: the isCurrent guard makes racing close/error signals idempotent. */
  function generationDown(generation: McpClient): void {
    if (!isCurrent(generation)) return
    client = undefined
    closeClient = undefined
    scheduleReconnect()
  }

  /** Decide retry ownership after a failed connection's close barrier settles. */
  function settleFailedGeneration(generation: McpClient, quiesced: boolean): void {
    if (!isCurrent(generation)) return
    if (!quiesced) {
      client = undefined
      closeClient = undefined
      ctx.logger.error(`${label}: failed generation could not confirm transport closure — reconnect stopped to avoid overlapping server processes; reload the plugin or restart the Host to retry`)
      return
    }
    generationDown(generation)
  }

  /**
   * Wait for the transport-owned close signal without letting a broken
   * transport wedge teardown forever. A rejected close settles at once with
   * whatever closure was already observed.
   */
  function waitForClose(closing: Promise<void>, closed: Promise<void>, hasClosed: () => boolean): Promise<boolean> {
    return new Promise((resolve) => {
      const settle = (quiesced: boolean): void => {
        clearTimeout(timeout)
        resolve(quiesced)
      }
      const timeout = setTimeout(() => { resolve(false) }, GENERATION_CLOSE_TIMEOUT_MS)
      timeout.unref()
      void closed.then(() => { settle(true) })
      closing.catch(() => { settle(hasClosed()) })
    })
  }

  function scheduleReconnect(): void {
    const lostEstablishedConnection = connectedAt !== undefined
    if (!policy.enabled) {
      const message = lostEstablishedConnection
        ? 'connection lost and reconnect is disabled — registered tools will fail until an HMR reload or Host restart'
        : 'connection failed and reconnect is disabled — no tools were registered; reload the plugin or restart the Host to connect'
      ctx.logger.error(`${label}: ${message}`)
      return
    }
    // A connection that stayed up past the stability window (= maxDelayMs, the
    // longest backoff spacing) ended the previous outage: start a fresh budget.
    if (connectedAt !== undefined && Date.now() - connectedAt >= policy.maxDelayMs) failedAttempts = 0
    connectedAt = undefined
    failedAttempts += 1
    if (failedAttempts > policy.maxAttempts) {
      // Enqueue the give-up disposal so it cannot race an in-flight sync's
      // phase-2 swap (which checks isCurrent inside the queue).
      syncChain = syncChain.then(() => {
        for (const dispose of disposers.values()) dispose()
        disposers = new Map()
        serverInstructions = ''
      })
      ctx.logger.error(`${label}: giving up after ${policy.maxAttempts} consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect`)
      return
    }
    const delayMs = Math.min(policy.maxDelayMs, policy.initialDelayMs * 2 ** (failedAttempts - 1))
    const action = lostEstablishedConnection ? 'connection lost; reconnecting' : 'connection failed; retrying'
    ctx.logger.warn(`${label}: ${action} in ${delayMs}ms (attempt ${failedAttempts}/${policy.maxAttempts})`)
    reconnectTimer = setTimeout(() => {
      reconnectTimer = undefined
      settling = connectGeneration(false)
    }, delayMs)
    // An armed reconnect timer must never hold the process open on its own.
    reconnectTimer.unref()
  }

  /**
   * One connection attempt: fresh transport + client (an McpClient connects
   * once for life), connect, then queue the initial tool sync. The startup
   * flag belongs to the attempt rather than the shared sync queue, so an early
   * notification cannot consume strict startup semantics. Every failure
   * funnels through {@link generationDown}; success arms the transport-close
   * disconnect path. Never rejects.
   *
   * @param startup - Whether this is the plugin's activation attempt.
   */
  async function connectGeneration(startup: boolean): Promise<void> {
    const generation = new McpClient({
      name: 'dsh-mcp-client',
      version: '0.0.1',
      requestTimeoutMs: PROTOCOL_REQUEST_TIMEOUT_MS,
    })
    const closed: PromiseWithResolvers<void> = Promise.withResolvers()
    let attemptSettled = false
    let closeObserved = false
    let transport: McpTransport | undefined
    const hasClosed = (): boolean => closeObserved
    client = generation
    closeClient = closeGeneration
    /**
     * Close the client, then its transport, and wait for the transport's own
     * close signal. The client's close event fires before the transport
     * stops, so only the transport signal proves the server is gone.
     */
    async function closeGeneration(): Promise<boolean> {
      const owned = transport
      if (owned === undefined) return true
      // A failed connect already closed its transport; transport close is idempotent.
      return await waitForClose(generation.close().then(() => owned.close()), closed.promise, hasClosed)
    }
    async function refreshTools(): Promise<void> {
      if (!isCurrent(generation)) return
      ctx.logger.info(`${label}: tool list changed, re-syncing`)
      try {
        await enqueueSync(generation)
      } catch (error) {
        if (!disposed) ctx.logger.error(`${label}: tool re-sync failed: ${String(error)}`)
      }
    }
    let instructions: string
    try {
      transport = createTransport(config)
      transport.onClose(() => {
        closeObserved = true
        closed.resolve()
        // A failed connect owns its close barrier in the catch path below. An
        // established generation can transition down directly from this signal.
        if (attemptSettled) generationDown(generation)
      })
      // Registered before connect: a server may announce a change as soon as
      // it is initialized. Only servers that advertise the capability count.
      generation.onNotification('notifications/tools/list_changed', () => {
        if (generation.serverCapabilities?.tools?.listChanged === true) void refreshTools()
      })
      await generation.connect(transport)
      if (hasClosed()) {
        attemptSettled = true
        generationDown(generation)
        return
      }
      if (!isCurrent(generation)) {
        if (!await closeGeneration()) ctx.logger.error(incompleteDisposalMessage)
        return
      }
      const serverText = generation.instructions?.trimEnd() ?? ''
      instructions = serverText ? `### MCP server: ${config.serverName}\n\n${serverText}` : ''
      if (Buffer.byteLength(instructions) > maxInstructionBytes) {
        throw new Error(`${label}: server instructions exceed maxInstructionBytes (${maxInstructionBytes})`)
      }
      await enqueueSync(generation, startup ? startupOpts : opts)
    } catch (error) {
      if (firstAttemptError === undefined) firstAttemptError = error
      // Disposal clears current ownership before it closes the generation, so
      // only a live supervisor reports an attempt failure.
      if (isCurrent(generation)) ctx.logger.warn(`${label}: connection attempt failed: ${String(error)}`)
      const quiesced = await closeGeneration()
      attemptSettled = true
      settleFailedGeneration(generation, quiesced)
      return
    }
    attemptSettled = true
    if (hasClosed()) {
      generationDown(generation)
      return
    }
    if (!isCurrent(generation)) return
    serverInstructions = instructions
    connectedAt = Date.now()
    if (failedAttempts > 0) ctx.logger.info(`${label}: reconnected and re-synced tools (attempt ${failedAttempts}/${policy.maxAttempts})`)
  }

  /** The in-flight (or last settled) connection attempt; dispose awaits it for quiescence. */
  let settling = connectGeneration(true)

  // The ready promise settles when the first attempt finishes (regardless of
  // success). If the first attempt fails and reconnect is enabled, the
  // supervisor is already scheduling a retry — ready just reports the outcome.
  const ready: Promise<ConnectionOutcome> = settling.then(() => {
    // After settling: if client is set the initial connect+sync succeeded.
    // If not, the supervisor either scheduled a retry (error logged) or gave
    // up (error logged). Either way the outcome is reported with the real error.
    // Note: settling.then() is a microtask; the stdio close event is a
    // macrotask — so a server that crashes AFTER a successful initial sync
    // cannot flip client to undefined before this continuation runs.
    if (client !== undefined) return {}
    /* v8 ignore next -- defensive: firstAttemptError is always set when connect/sync fails */
    return { error: firstAttemptError ?? new Error(`${label}: initial connection failed`) }
  })

  return {
    ready,
    instructions: () => serverInstructions,
    resources: {
      async request(request, exec): Promise<JsonValue> {
        const generation = client
        if (!generation || connectedAt === undefined) throw new Error(`${label}: server is disconnected`)
        const options = { signal: exec.signal, timeoutMs: config.toolCallTimeoutMs }
        // A server without the resources capability lists nothing; reads still reach it.
        const listable = generation.serverCapabilities?.resources !== undefined
        switch (request.method) {
          case 'resources/list': {
            if (!listable) return resourceListResult([])
            if (request.cursor === undefined) return resourceListResult(await generation.listResources(options))
            const page = await generation.listResourcesPage(request.cursor, options)
            return resourceListResult(page.resources, page.nextCursor)
          }
          case 'resources/templates/list': {
            if (!listable) return resourceTemplateListResult([])
            if (request.cursor === undefined) {
              return resourceTemplateListResult(await generation.listResourceTemplates(options))
            }
            const page = await generation.listResourceTemplatesPage(request.cursor, options)
            return resourceTemplateListResult(page.resourceTemplates, page.nextCursor)
          }
          case 'resources/read':
            return parseReadResourceResult(await generation.readResource(request.uri, options))
          /* v8 ignore next 2 -- resource requests are the closed, typed tool operation union */
          default:
            return assertNever(request)
        }
      },
    },
    async dispose(): Promise<void> {
      disposed = true
      serverInstructions = ''
      if (reconnectTimer !== undefined) {
        clearTimeout(reconnectTimer)
        reconnectTimer = undefined
      }
      const close = closeClient
      client = undefined
      closeClient = undefined
      if (close !== undefined && !await close()) {
        ctx.logger.error(incompleteDisposalMessage)
      }
      // Quiesce, don't just request it: the in-flight attempt enqueues its
      // sync before settling, so awaiting both leaves `disposers` final.
      await settling
      await syncChain
      for (const dispose of disposers.values()) dispose()
      disposers = new Map()
    },
  }
}
