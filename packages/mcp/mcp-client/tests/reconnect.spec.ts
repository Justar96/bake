/**
 * Tests for the mcp-client connection supervisor: crash-driven reconnection
 * with bounded backoff, generation-safe tool re-registration, the failure
 * cap, the stability-window budget reset, and disposal stopping reconnection.
 * Isolated file so vi.mock of pi-mcp doesn't pollute other test suites.
 */
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import SystemPrompt from 'bake-system-prompt'
import ToolRuntime from 'bake-tools'
import McpResources from '@deepseek-ai/dsh-mcp-resources'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import type { Config } from '@deepseek-ai/dsh-mcp-client'

// ---- Mock pi-mcp ----

// vi.mock factories are hoisted above every import/const, so the mock fns and
// classes must be created inside vi.hoisted to exist when the factories run.
const {
  mockConnect, mockClose, mockTransportClose, mockListTools, mockCallTool, mockOnNotification,
  MockClient, FakeTransport, instances, transports,
} = vi.hoisted(() => {
  /** Transport double that emits its close signal once, as pi-mcp transports do. */
  class FakeTransport {
    private readonly closeListeners = new Set<() => void>()
    private closeEmitted = false
    constructor() {
      transports.push(this)
    }
    onClose(listener: () => void): () => void {
      this.closeListeners.add(listener)
      return () => void this.closeListeners.delete(listener)
    }
    /** The server process or session ended. */
    emitClose(): void {
      if (this.closeEmitted) return
      this.closeEmitted = true
      for (const listener of [...this.closeListeners]) listener()
    }
    close(): Promise<void> {
      return mockTransportClose.call(this)
    }
  }
  const mockConnect = vi.fn<(this: MockClient, transport: FakeTransport) => Promise<void>>()
  const mockClose = vi.fn<(this: MockClient) => Promise<void>>()
  const mockTransportClose = vi.fn<(this: FakeTransport) => Promise<void>>()
  const mockListTools = vi.fn<(_options?: unknown) => Promise<unknown>>()
  const mockCallTool = vi.fn<(
    _name: string, _args?: Record<string, unknown>, _options?: unknown,
  ) => Promise<unknown>>()
  const mockOnNotification = vi.fn<(method: string, listener: () => void) => void>()
  class MockClient {
    transport: FakeTransport | undefined
    serverCapabilities = { tools: { listChanged: true } }
    listResources = async () => []
    listTools = mockListTools
    callTool = mockCallTool
    constructor() {
      instances.push(this)
    }
    get instructions(): string | undefined { return undefined }
    connect(transport: FakeTransport): Promise<void> {
      this.transport = transport
      return mockConnect.call(this, transport)
    }
    close(): Promise<void> {
      return mockClose.call(this)
    }
    onNotification(method: string, listener: () => void): () => void {
      mockOnNotification(method, listener)
      return () => {}
    }
  }
  const instances: MockClient[] = []
  const transports: FakeTransport[] = []
  return {
    mockConnect, mockClose, mockTransportClose, mockListTools, mockCallTool, mockOnNotification,
    MockClient, FakeTransport, instances, transports,
  }
})

vi.mock('@earendil-works/pi-mcp', () => ({
  McpClient: MockClient,
  StdioTransport: FakeTransport,
  StreamableHttpTransport: FakeTransport,
}))

// vi.mock is hoisted above static imports, so the modules under test see the
// mocked client even through a static import.
import { apply } from '@deepseek-ai/dsh-mcp-client/src/index.ts'
import { RECONNECT_DEFAULTS, resolveReconnectPolicy, startConnection } from '@deepseek-ai/dsh-mcp-client/src/connection.ts'

// ---- Helpers ----

const testToolSignal = new AbortController().signal

async function mountRegistry(): Promise<Context> {
  const ctx = new Context()
  await ctx.plugin(SystemPrompt)
  await ctx.plugin(ToolRuntime)
  return ctx
}

function sleep(ms: number): Promise<void> {
  // Annotated binding (not withResolvers<void>()): the tests lint layer runs
  // no-invalid-void-type with default options, which rejects the explicit
  // type argument in call position but accepts the inferred form.
  const gate: PromiseWithResolvers<void> = Promise.withResolvers()
  setTimeout(gate.resolve, ms)
  return gate.promise
}

/** Capture the supervisor's logger lines by level on one context. */
function captureLogs(ctx: Context): { warns: string[]; errors: string[]; infos: string[] } {
  const warns: string[] = []
  const errors: string[] = []
  const infos: string[] = []
  ctx.logger.warn = ((message: unknown) => { warns.push(String(message)) }) as typeof ctx.logger.warn
  ctx.logger.error = ((message: unknown) => { errors.push(String(message)) }) as typeof ctx.logger.error
  ctx.logger.info = ((message: unknown) => { infos.push(String(message)) }) as typeof ctx.logger.info
  return { warns, errors, infos }
}

function stdioConfig(reconnect?: Config['reconnect']): Config {
  return {
    transport: 'stdio',
    serverName: 'srv',
    command: 'echo',
    args: [],
    env: {},
    cwd: '',
    toolCallTimeoutMs: 60_000,
    failOnStartupError: false,
    ...reconnect === undefined ? {} : { reconnect },
  }
}

/** The tool list the mock server advertises after a successful (re)connect. */
function listing(...names: string[]): { name: string; inputSchema: { type: string } }[] {
  return names.map(name => ({ name, inputSchema: { type: 'object' } }))
}

let callSeq = 0
function nextCallId(): ToolCallId {
  return ToolCallId(`reconnect-${++callSeq}`)
}

// ---- Tests ----

describe('reconnect supervisor', () => {
  let ctx: Context

  beforeEach(async () => {
    vi.clearAllMocks()
    instances.length = 0
    transports.length = 0
    mockConnect.mockResolvedValue(undefined)
    mockClose.mockImplementation(function (this: InstanceType<typeof MockClient>) {
      return this.transport?.close() ?? Promise.resolve()
    })
    mockTransportClose.mockImplementation(function (this: InstanceType<typeof FakeTransport>) {
      this.emitClose()
      return Promise.resolve()
    })
    mockListTools.mockResolvedValue(listing('remote'))
    mockCallTool.mockResolvedValue({ content: [{ type: 'text', text: 'ok' }] })
    ctx = await mountRegistry()
  })

  it('keeps instructions withdrawn when disposal interrupts initial discovery', async () => {
    const listingGate: PromiseWithResolvers<ReturnType<typeof listing>> = Promise.withResolvers()
    const instructionSpy = vi.spyOn(MockClient.prototype, 'instructions', 'get').mockReturnValue('Instructions after discovery.')
    mockListTools.mockImplementation(() => listingGate.promise)
    const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
    try {
      await vi.waitFor(() => { expect(mockListTools).toHaveBeenCalled() })
      const disposing = handle.dispose()
      listingGate.resolve(listing('remote'))
      await disposing
      expect(handle.instructions()).toBe('')
    } finally {
      instructionSpy.mockRestore()
      listingGate.resolve(listing('remote'))
      await handle.dispose()
      await ctx.fiber.dispose()
    }
  })

  it('rejects resource reads while a replacement connection is still negotiating', async () => {
    await ctx.plugin(McpResources)
    const config = stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 2 })
    const handle = startConnection(ctx, config, resolveReconnectPolicy(config.reconnect, 'reconnect'))
    const reconnectGate: PromiseWithResolvers<void> = Promise.withResolvers()
    try {
      await handle.ready
      ctx.mcpResources.register('srv', handle.resources)
      mockConnect.mockImplementationOnce(() => reconnectGate.promise)
      transports[0]!.emitClose()
      await vi.waitFor(() => { expect(instances).toHaveLength(2) })
      const result = await ctx.tools.execute({
        name: 'list_mcp_resources', arguments: { server: 'srv' },
        callId: nextCallId(), signal: testToolSignal,
      })
      expect(result.isError).toBe(true)
      expect(JSON.stringify(result.content)).toContain('server is disconnected')
    } finally {
      reconnectGate.resolve()
      await handle.dispose()
      await ctx.fiber.dispose()
    }
  })

  it('reconnects after a transport close, re-syncs tools through the new generation, and serves calls', async () => {
    const { warns, infos } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 5, maxDelayMs: 40, maxAttempts: 5 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })
    expect(instances).toHaveLength(1)

    // The recovered server advertises a different list: the swap must neither
    // duplicate nor leak the pre-crash generation.
    mockListTools.mockResolvedValue(listing('revived'))
    transports[0]!.emitClose()

    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__revived')).toBeDefined() })
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
    expect(instances).toHaveLength(2)
    expect(mockConnect).toHaveBeenCalledTimes(2)

    // Post-recovery calls execute through the re-registered definition.
    const result = await ctx.tools.execute({
      signal: testToolSignal,
      callId: nextCallId(), name: 'mcp__srv__revived', arguments: {},
    })
    expect(result.isError).toBe(false)

    // User-visible state: reconnecting and recovered are distinct lines.
    expect(warns.some(line => line.includes('reconnecting in 5ms (attempt 1/5)'))).toBe(true)
    expect(infos.some(line => line.includes('reconnected and re-synced tools'))).toBe(true)

    // A late close signal from the replaced generation is ignored.
    transports[0]!.emitClose()
    await sleep(30)
    expect(instances).toHaveLength(2)
  })

  it('stops at the failure cap, unregisters the tools, and reports final failure', async () => {
    const { warns, errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 2 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    mockConnect.mockRejectedValue(new Error('server gone'))
    // A failing close on the failed attempt's cleanup must not break the loop.
    mockClose.mockImplementation(function (this: InstanceType<typeof MockClient>) {
      this.transport?.emitClose()
      return Promise.reject(new Error('already closed'))
    })
    transports[0]!.emitClose()

    await vi.waitFor(() => {
      expect(errors.some(line => line.includes('giving up after 2 consecutive failed reconnect attempts'))).toBe(true)
    })
    // Stale tools do not leak past final failure.
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
    // Initial connect + exactly maxAttempts reconnect attempts.
    expect(mockConnect).toHaveBeenCalledTimes(3)
    expect(warns.some(line => line.includes('connection attempt failed: Error: server gone'))).toBe(true)
    expect(warns.some(line => line.includes('connection failed; retrying in 4ms (attempt 2/2)'))).toBe(true)
    await sleep(30)
    expect(mockConnect).toHaveBeenCalledTimes(3)
  })

  it('gives up behind an in-flight re-sync and removes the generation it publishes', async () => {
    const { errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 1 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    const gate: PromiseWithResolvers<unknown> = Promise.withResolvers()
    mockListTools.mockImplementation(() => gate.promise)
    const handler = mockOnNotification.mock.calls[0]![1]
    handler()
    await vi.waitFor(() => { expect(mockListTools).toHaveBeenCalledTimes(2) })

    mockConnect.mockRejectedValue(new Error('server gone'))
    transports[0]!.emitClose()
    await vi.waitFor(() => {
      expect(errors.some(line => line.includes('giving up after 1 consecutive failed reconnect attempts'))).toBe(true)
    })

    gate.resolve(listing('late'))
    await vi.waitFor(() => {
      expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
      expect(ctx.tools.get('mcp__srv__late')).toBeUndefined()
    })
    expect(mockConnect).toHaveBeenCalledTimes(2)
  })

  it('does not start a replacement until a failed generation reports that it closed', async () => {
    const { warns } = captureLogs(ctx)
    mockConnect.mockRejectedValueOnce(new Error('initialize failed'))
    // Model a stdio close that returns before the child has exited.
    mockClose.mockResolvedValue(undefined)
    mockTransportClose.mockResolvedValue(undefined)

    const applying = apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 2 }))
    await vi.waitFor(() => { expect(mockClose).toHaveBeenCalled() })
    await sleep(30)
    expect(instances).toHaveLength(1)

    transports[0]!.emitClose()
    await applying
    await vi.waitFor(() => { expect(instances).toHaveLength(2) })
    expect(warns.some(line => line.includes('connection failed; retrying in 2ms (attempt 1/2)'))).toBe(true)
  })

  it('stops reconnecting when a failed generation never reports that it closed', async () => {
    vi.useFakeTimers()
    try {
      const { errors } = captureLogs(ctx)
      mockConnect.mockRejectedValue(new Error('initialize failed'))
      mockClose.mockResolvedValue(undefined)
      mockTransportClose.mockResolvedValue(undefined)

      const applying = apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 2 }))
      await vi.advanceTimersByTimeAsync(5_000)
      await applying

      expect(instances).toHaveLength(1)
      expect(errors.some(line => line.includes('reconnect stopped to avoid overlapping server processes'))).toBe(true)
    } finally {
      vi.useRealTimers()
    }
  })

  it('suppresses retry reporting when disposal owns a pending connect rejection', async () => {
    const { warns } = captureLogs(ctx)
    const gate: PromiseWithResolvers<void> = Promise.withResolvers()
    mockConnect.mockImplementation(() => gate.promise)
    const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
    await vi.waitFor(() => { expect(instances).toHaveLength(1) })

    const disposing = handle.dispose()
    gate.reject(new Error('disposed connect'))
    await disposing
    await handle.ready

    expect(warns.some(line => line.includes('connection attempt failed'))).toBe(false)
    expect(instances).toHaveLength(1)
  })

  it('stops reconnecting when disposal overlaps failed-generation cleanup', async () => {
    vi.useFakeTimers()
    const entered: PromiseWithResolvers<void> = Promise.withResolvers()
    const closed: PromiseWithResolvers<void> = Promise.withResolvers()
    const { warns } = captureLogs(ctx)
    mockConnect.mockRejectedValue(new Error('initialize failed'))
    mockClose.mockImplementation(async function (this: InstanceType<typeof MockClient>) {
      entered.resolve()
      await closed.promise
      this.transport?.emitClose()
    })
    const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
    try {
      await entered.promise
      expect(warns.some(line => line.includes('connection attempt failed'))).toBe(true)
      const disposing = handle.dispose()
      closed.resolve()
      await disposing
      await handle.ready
      await vi.runAllTimersAsync()
      expect(instances).toHaveLength(1)
      expect(mockListTools).not.toHaveBeenCalled()
      expect(warns.some(line => line.includes('retrying'))).toBe(false)
    } finally {
      closed.resolve()
      await handle.dispose()
      await ctx.fiber.dispose()
      vi.useRealTimers()
    }
  })

  it.each(['connect', 'discovery'] as const)('bounds disposal during %s when the transport never reports closure', async (phase) => {
    vi.useFakeTimers()
    try {
      const { errors } = captureLogs(ctx)
      const gate: PromiseWithResolvers<void> = Promise.withResolvers()
      if (phase === 'connect') mockConnect.mockImplementation(() => gate.promise)
      else mockListTools.mockImplementation(() => gate.promise.then(() => listing('late')))
      mockClose.mockResolvedValue(undefined)
      mockTransportClose.mockResolvedValue(undefined)
      const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
      await vi.advanceTimersByTimeAsync(0)

      const disposing = handle.dispose()
      await vi.advanceTimersByTimeAsync(5_000)
      gate.resolve()
      await vi.advanceTimersByTimeAsync(5_000)
      await disposing

      expect(mockListTools).toHaveBeenCalledTimes(phase === 'connect' ? 0 : 1)
      expect(ctx.tools.get('mcp__srv__late')).toBeUndefined()
      expect(errors.some(line => line.includes('server shutdown may be incomplete'))).toBe(true)
    } finally {
      vi.useRealTimers()
    }
  })

  it('closes the connecting transport when disposal starts before connect resolves', async () => {
    const gate: PromiseWithResolvers<void> = Promise.withResolvers()
    mockConnect.mockImplementation(() => gate.promise)
    const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
    const disposing = handle.dispose()
    gate.resolve()
    await disposing
    await handle.ready
    expect(mockClose).toHaveBeenCalledTimes(1)
    expect(mockTransportClose).toHaveBeenCalled()
    expect(mockListTools).not.toHaveBeenCalled()
  })

  it('discards a queued tool refresh when disposal starts before it runs', async () => {
    const handle = startConnection(ctx, stdioConfig(), resolveReconnectPolicy(undefined, 'reconnect'))
    await handle.ready
    const notify = mockOnNotification.mock.calls[0]![1]
    notify()
    await handle.dispose()
    expect(mockListTools).toHaveBeenCalledTimes(1)
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
  })

  it('dispose during the backoff wait cancels the pending reconnect', async () => {
    await apply(ctx, stdioConfig({ initialDelayMs: 60_000, maxDelayMs: 60_000, maxAttempts: 5 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    transports[0]!.emitClose()
    // Now waiting out a 60s backoff; disposal must return promptly anyway.
    await ctx.fiber.dispose()
    await sleep(30)
    expect(mockConnect).toHaveBeenCalledTimes(1)
    expect(instances).toHaveLength(1)
  })

  it('a transport close after dispose schedules nothing', async () => {
    const fiber = ctx.plugin({ name: 'mcp-client', inject: ['tools'], apply }, stdioConfig())
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    await fiber.dispose()
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()

    // The disposer's close already ended the transport; a repeated signal is inert.
    transports[0]!.emitClose()
    await sleep(30)
    expect(instances).toHaveLength(1)
    expect(mockConnect).toHaveBeenCalledTimes(1)
  })

  it('reconnect disabled keeps the registered tools and reports manual recovery', async () => {
    const { errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ enabled: false }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    transports[0]!.emitClose()
    await sleep(30)
    expect(mockConnect).toHaveBeenCalledTimes(1)
    // Pre-reconnect contract: the generation stays registered until disposal.
    expect(ctx.tools.get('mcp__srv__remote')).toBeDefined()
    expect(errors.some(line => line.includes('connection lost and reconnect is disabled'))).toBe(true)
  })
  it('reconnect disabled after a failed initial connect reports no registered tools', async () => {
    const { errors } = captureLogs(ctx)
    mockConnect.mockRejectedValue(new Error('refused'))
    await apply(ctx, stdioConfig({ enabled: false }))
    await sleep(30)
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
    expect(errors.some(line => line.includes('connection failed and reconnect is disabled'))).toBe(true)
    expect(errors.some(line => line.includes('no tools were registered'))).toBe(true)
  })

  it('an uptime past the stability window resets the attempt budget', async () => {
    const { errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 30, maxAttempts: 1 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    transports[0]!.emitClose()
    await vi.waitFor(() => { expect(instances).toHaveLength(2) })
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    // Outlive the stability window (= maxDelayMs), then crash again: the
    // budget restarts at attempt 1 instead of exceeding maxAttempts.
    await sleep(40)
    transports[1]!.emitClose()
    await vi.waitFor(() => { expect(instances).toHaveLength(3) })
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })
    expect(errors).toHaveLength(0)
  })

  it('a crash loop with briefly successful connects still exhausts the cap', async () => {
    const { errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 10_000, maxAttempts: 1 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    // Crash, recover (attempt 1 of 1), crash again well inside the stability
    // window: the successful connect must not launder the budget.
    transports[0]!.emitClose()
    await vi.waitFor(() => { expect(instances).toHaveLength(2) })
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })
    transports[1]!.emitClose()

    await vi.waitFor(() => {
      expect(errors.some(line => line.includes('giving up after 1 consecutive failed reconnect attempts'))).toBe(true)
    })
    expect(instances).toHaveLength(2)
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
  })

  it('a connect rejection racing its own transport close schedules exactly one retry per attempt', async () => {
    const { errors } = captureLogs(ctx)
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 3 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    // Each reconnect attempt sees the stdio transport close AND its connect()
    // reject, as a spawn failure produces both.
    mockConnect.mockImplementation(async () => {
      transports.at(-1)!.emitClose()
      throw new Error('spawn failed')
    })
    transports[0]!.emitClose()

    await vi.waitFor(() => {
      expect(errors.some(line => line.includes('giving up after 3 consecutive failed reconnect attempts'))).toBe(true)
    })
    // Initial generation + exactly one generation per budgeted attempt: a
    // double-scheduled retry would create more.
    expect(instances).toHaveLength(4)
    expect(errors.filter(line => line.includes('giving up')).length).toBe(1)
  })

  it('a transport that closes during a resolving connect registers nothing from the dead generation', async () => {
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 2 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })
    expect(mockListTools).toHaveBeenCalledTimes(1)

    mockConnect.mockImplementation(async () => {
      transports.at(-1)!.emitClose()
    })
    transports[0]!.emitClose()

    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined() })
    // The dead generations never reached tool discovery.
    expect(mockListTools).toHaveBeenCalledTimes(1)
  })

  it('dispose during an in-flight initial sync quiesces without leaking tools', async () => {
    const fiber = ctx.plugin({ name: 'mcp-client', inject: ['tools'], apply }, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 5 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    // Block the reconnect attempt's tool discovery until after dispose starts.
    const gate: PromiseWithResolvers<unknown> = Promise.withResolvers()
    mockListTools.mockImplementation(() => gate.promise)
    transports[0]!.emitClose()
    await vi.waitFor(() => { expect(mockListTools).toHaveBeenCalledTimes(2) })

    const disposing = fiber.dispose()
    await sleep(10)
    gate.resolve(listing('late'))
    await disposing

    // The late sync's swap ran, then disposal unregistered its result: no
    // generation survives the plugin.
    expect(ctx.tools.get('mcp__srv__remote')).toBeUndefined()
    expect(ctx.tools.get('mcp__srv__late')).toBeUndefined()
  })

  it('a re-sync failing because dispose closed the transport stays silent', async () => {
    const { errors } = captureLogs(ctx)
    const fiber = ctx.plugin({ name: 'mcp-client', inject: ['tools'], apply }, stdioConfig())
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    const gate: PromiseWithResolvers<unknown> = Promise.withResolvers()
    mockListTools.mockImplementation(() => gate.promise)
    const handler = mockOnNotification.mock.calls[0]![1]
    handler()
    await vi.waitFor(() => { expect(mockListTools).toHaveBeenCalledTimes(2) })

    const disposing = fiber.dispose()
    await sleep(10)
    gate.reject(new Error('Connection closed'))
    await disposing

    expect(errors.some(line => line.includes('tool re-sync failed'))).toBe(false)
  })

  it('a stale notification handler from a replaced generation is ignored', async () => {
    await apply(ctx, stdioConfig({ initialDelayMs: 2, maxDelayMs: 8, maxAttempts: 5 }))
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })

    transports[0]!.emitClose()
    await vi.waitFor(() => { expect(instances).toHaveLength(2) })
    await vi.waitFor(() => { expect(ctx.tools.get('mcp__srv__remote')).toBeDefined() })
    const listCalls = mockListTools.mock.calls.length

    const staleHandler = mockOnNotification.mock.calls[0]![1]
    staleHandler()
    expect(mockListTools).toHaveBeenCalledTimes(listCalls)
  })
})

// ---- Policy resolution ----

describe('resolveReconnectPolicy', () => {
  const path = 'mcp-client(srv): reconnect'

  it('resolves omission to the defaults, frozen', () => {
    const policy = resolveReconnectPolicy(undefined, path)
    expect(policy).toEqual(RECONNECT_DEFAULTS)
    expect(Object.isFrozen(policy)).toBe(true)
  })

  it('keeps explicit values', () => {
    expect(resolveReconnectPolicy(
      { enabled: false, initialDelayMs: 1, maxDelayMs: 2, maxAttempts: 7 },
      path,
    )).toEqual({ enabled: false, initialDelayMs: 1, maxDelayMs: 2, maxAttempts: 7 })
  })

  it('rejects unknown keys', () => {
    expect(() => resolveReconnectPolicy({ jitterRatio: 0.5 } as never, path))
      .toThrow(/reconnect\.jitterRatio is not a reconnect option/)
  })

  it('rejects out-of-range delays', () => {
    expect(() => resolveReconnectPolicy({ initialDelayMs: 0 }, path)).toThrow(/initialDelayMs must be a positive finite number/)
    expect(() => resolveReconnectPolicy({ initialDelayMs: Number.POSITIVE_INFINITY }, path)).toThrow(/initialDelayMs/)
    expect(() => resolveReconnectPolicy({ maxDelayMs: -1 }, path)).toThrow(/maxDelayMs must be a positive finite number/)
  })

  it('rejects an initial delay above the ceiling', () => {
    expect(() => resolveReconnectPolicy({ initialDelayMs: 100, maxDelayMs: 5 }, path))
      .toThrow(/initialDelayMs must be less than or equal to maxDelayMs/)
  })

  it('rejects non-positive-integer attempt caps', () => {
    expect(() => resolveReconnectPolicy({ maxAttempts: 0 }, path)).toThrow(/maxAttempts must be a positive integer/)
    expect(() => resolveReconnectPolicy({ maxAttempts: 1.5 }, path)).toThrow(/maxAttempts must be a positive integer/)
  })

  it('apply fails loud at load on a misconfigured reconnect', async () => {
    const ctx = await mountRegistry()
    await expect(apply(ctx, stdioConfig({ initialDelayMs: 100, maxDelayMs: 5 })))
      .rejects.toThrow(/initialDelayMs must be less than or equal to maxDelayMs/)
  })
})
