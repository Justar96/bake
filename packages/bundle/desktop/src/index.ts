/**
 * @deepseek-ai/dsh-desktop — the long-lived bridge between Bake and the Bake
 * Desktop app. It speaks the desktop protocol (./protocol.ts) over the
 * utility-process message port or stdio, drives one root Agent per process,
 * streams its output, forwards approval questions to the desktop, applies the
 * desktop's permission tier, and reports harness spans for the desktop's
 * trace view.
 *
 * @module @deepseek-ai/dsh-desktop
 */

import { randomUUID } from 'node:crypto'
import type { Context } from '@deepseek-ai/cordis'
import { brandString } from '@deepseek-ai/dsh-brand'
import { installModelSelection } from '@deepseek-ai/dsh-agent'
import type { Agent, AssistantStreamFrame, ModelSelectionRef } from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import type {} from '@deepseek-ai/dsh-fs'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { Session, SessionEvent, SessionId } from '@deepseek-ai/dsh-session'
import type { PreToolDecision } from '@deepseek-ai/dsh-tools'
import type { ApprovalOutcome } from '@deepseek-ai/dsh-user-approval'
import type {} from '@deepseek-ai/dsh-permission-presets'
// Empty type imports carry the loader Context merge for the settlement await
// and the cmdline Context merge for the appExit host value.
import type {} from '@deepseek-ai/cordis-plugin-loader'
import type {} from '@deepseek-ai/dsh-cmdline'
import { classifyTool, TIER_PRESET } from './permission.ts'
import {
  type ApprovalDecision,
  type CoreMessage,
  type HarnessMessage,
  type PermissionTier,
  PERMISSION_TIERS,
  PROTOCOL_VERSION,
} from './protocol.ts'
import { type OpenSpan, SpanRecorder, nowUs } from './spans.ts'
import { openTransport, type Transport } from './transport.ts'

export { classifyTool, commandProgram, TIER_PRESET } from './permission.ts'
export type { ToolVerdict } from './permission.ts'

/** Stable Cordis plugin name. */
export const name = 'desktop-bridge'

/** Core services required before the bridge can create an Agent. */
export const inject = ['agentDefaultModel', 'agents', 'sessions']

/** Coalescing window for streamed deltas, so one IPC message carries a frame's worth of text. */
const DELTA_FLUSH_MS = 16
/** Interval between span uploads. */
const SPAN_FLUSH_MS = 1000
/** Longest string kept in tool arguments and results sent to the desktop. */
const MAX_FIELD_CHARS = 16 * 1024

/** Harness version reported in `ready`, from this package's manifest. */
const HARNESS_VERSION = '0.1.6-alpha.2'

function bound(text: string): { text: string; truncated: boolean } {
  return text.length > MAX_FIELD_CHARS
    ? { text: `${text.slice(0, MAX_FIELD_CHARS)}…`, truncated: true }
    : { text, truncated: false }
}

/** Parse tool-call arguments as the executor does; invalid JSON stays text. Long strings are bounded. */
function parseArguments(raw: string): unknown {
  if (raw === '') return {}
  try {
    return JSON.parse(raw, (_key, value: unknown) => typeof value === 'string' ? bound(value).text : value) as unknown
  } catch {
    return bound(raw).text
  }
}

function resultText(blocks: readonly { type: string; text?: string }[]): string {
  return blocks
    .filter((block): block is { type: string; text: string } => block.type === 'text' && typeof block.text === 'string')
    .map(block => block.text)
    .join('')
}

function isTier(value: unknown): value is PermissionTier {
  return typeof value === 'string' && (PERMISSION_TIERS as readonly string[]).includes(value)
}

function isDecision(value: unknown): value is ApprovalDecision {
  return value === 'approve_once' || value === 'approve_session' || value === 'deny'
}

/** One streamed assistant message whose deltas are being coalesced. */
interface OpenStream {
  id: number
  text: string
  reasoning: string
  timer: NodeJS.Timeout | undefined
  llm: OpenSpan | undefined
  startedUs: number
  firstChunk: boolean
}

/** Per-call context the classifier leaves for the approval answerer. */
interface CallContext {
  summary: string
  args: unknown
  grant?: { key: string; label: string }
}

interface PendingApproval {
  resolve(outcome: ApprovalOutcome): void
}

/** The bridge's state for one process: at most one root Agent. */
class DesktopBridge {
  private transport: Transport | undefined
  private tier: PermissionTier = 'normal'
  private workspace: string | undefined
  private resumeSessionId: string | undefined
  private agent: Agent | undefined
  private agentPending: Promise<Agent> | undefined
  private model = ''
  private streamSeq = 0
  private stream: OpenStream | undefined
  private readonly recorder = new SpanRecorder()
  private spanTimer: NodeJS.Timeout | undefined
  /** Trace context of the user message that the next turn answers. */
  private nextTurnParent: string | undefined
  private turnSpan: OpenSpan | undefined
  private stepSpan: OpenSpan | undefined
  private readonly toolSpans = new Map<string, OpenSpan>()
  private readonly calls = new Map<string, CallContext>()
  private readonly approvals = new Map<string, PendingApproval>()
  private disposed = false

  constructor(
    private readonly ctx: Context,
    private readonly exit: (code: number) => void,
  ) {}

  private send(message: HarnessMessage): void {
    this.transport?.send(message)
  }

  private fail(code: string, error: unknown, fatal = false): void {
    const message = error instanceof Error ? error.message : String(error)
    this.send({ v: PROTOCOL_VERSION, type: 'error', fatal, code, message })
    if (fatal) this.exit(1)
  }

  start(): void {
    this.transport = openTransport({
      onMessage: (message) => {
        void this.handle(message).catch((error: unknown) => {
          this.fail('HANDLER_FAILED', error)
        })
      },
      onInvalid: (reason) => {
        this.fail('INVALID_MESSAGE', reason)
      },
      onClose: () => {
        this.exit(0)
      },
    })
    this.spanTimer = setInterval(() => {
      this.flushSpans()
    }, SPAN_FLUSH_MS)
    this.spanTimer.unref()
    this.installListeners()
    void (async () => {
      // Loader siblings mount concurrently; announce readiness once the
      // complete application (tools, adapters, answerers) is composed.
      await this.ctx.get('loader')?.await()
      if (this.disposed) return
      this.send({ v: PROTOCOL_VERSION, type: 'ready', harness_version: HARNESS_VERSION, protocol: PROTOCOL_VERSION, pid: process.pid })
    })().catch((error: unknown) => {
      this.fail('STARTUP_FAILED', error, true)
    })
  }

  dispose(): void {
    this.disposed = true
    if (this.spanTimer !== undefined) clearInterval(this.spanTimer)
    if (this.stream?.timer !== undefined) clearTimeout(this.stream.timer)
    for (const pending of this.approvals.values()) pending.resolve('cancelled')
    this.approvals.clear()
    this.flushSpans()
    this.transport?.close()
  }

  private flushSpans(): void {
    const spans = this.recorder.drain()
    if (spans.length > 0) this.send({ v: PROTOCOL_VERSION, type: 'telemetry.spans', spans })
  }

  // ---------------------------------------------------------------------
  // Inbound

  private async handle(message: CoreMessage): Promise<void> {
    switch (message.type) {
      case 'init':
        return this.init(message.workspace, message.permission, message.resume_session_id)
      case 'user.message':
        if (typeof message.text !== 'string' || message.text.trim() === '') {
          throw new Error('user.message requires non-empty text')
        }
        return this.userMessage(message.text, message.trace)
      case 'approval.result': {
        if (!isDecision(message.decision)) throw new Error(`unknown approval decision ${String(message.decision)}`)
        const pending = this.approvals.get(message.request_id)
        if (pending === undefined) return
        this.approvals.delete(message.request_id)
        pending.resolve(message.decision === 'deny' ? 'rejected' : 'allowed-once')
        return
      }
      case 'cancel':
        if (this.agent !== undefined && message.session_id === this.agent.id) this.agent.cancel({ kind: 'user' })
        return
      case 'permission.set':
        if (!isTier(message.permission)) throw new Error(`unknown permission tier ${String(message.permission)}`)
        this.tier = message.permission
        if (this.agent !== undefined) this.applyTier(this.agent.session)
        return
      case 'clock.ping':
        this.send({ v: PROTOCOL_VERSION, type: 'clock.pong', id: message.id, now_us: nowUs() })
        return
      case 'shutdown':
        this.exit(0)
        return
    }
  }

  private async init(workspace: unknown, permission: unknown, resume: unknown): Promise<void> {
    if (typeof workspace !== 'string' || workspace === '') throw new Error('init requires a workspace path')
    if (!isTier(permission)) throw new Error(`unknown permission tier ${String(permission)}`)
    if (this.workspace !== undefined) throw new Error('init was already received')
    const cwd = await this.cwd()
    if (cwd !== workspace) {
      throw new Error(`init workspace "${workspace}" differs from the harness working directory "${cwd}"`)
    }
    this.workspace = workspace
    this.tier = permission
    this.resumeSessionId = typeof resume === 'string' && resume !== '' ? resume : undefined
    this.model = this.selection().model
    this.send({ v: PROTOCOL_VERSION, type: 'initialized', workspace, permission, model: this.model })
    if (this.resumeSessionId !== undefined) await this.ensureAgent()
  }

  private async cwd(): Promise<string> {
    const fs = this.ctx.get('fs')
    return fs === undefined ? process.cwd() : fs.processPath(await fs.resolve('.'))
  }

  private selection(): { provider: string; model: string } {
    const defaultModel = this.ctx.get('agentDefaultModel')
    if (defaultModel === undefined) throw new Error('the default-model service is not available')
    const { provider, model } = defaultModel.currentSelection()
    return { provider, model }
  }

  private ensureAgent(): Promise<Agent> {
    if (this.agent !== undefined) return Promise.resolve(this.agent)
    this.agentPending ??= this.createAgent().finally(() => {
      this.agentPending = undefined
    })
    return this.agentPending
  }

  private async createAgent(): Promise<Agent> {
    const agents = this.ctx.get('agents')
    const defaultModel = this.ctx.get('agentDefaultModel')
    if (agents === undefined || defaultModel === undefined) throw new Error('the agent services are not available')
    if (this.workspace === undefined) throw new Error('init must precede the first user message')
    const selection = defaultModel.currentSelection()
    const agentOptions = { provider: selection.provider, model: selection.model }
    // This bundle composes no preset roster; model-facing rows read the
    // global layer, as in dsh-headless.
    const setup = (agentCtx: Context): void => {
      const selected: ModelSelectionRef = { current: selection, assembled: undefined }
      installModelSelection(agentCtx, selected)
    }
    const resumeId = this.resumeSessionId
    const resumed = resumeId !== undefined
    const { agent } = resumeId !== undefined
      ? await agents.resume({ resumeSessionId: brandString<SessionId>(resumeId), agentOptions, setup })
      : await agents.create({
        sessionId: brandString<SessionId>(`session-${randomUUID()}`),
        meta: { cwd: this.workspace },
        agentOptions,
        setup,
      })
    this.agent = agent
    this.applyTier(agent.session)
    this.send({
      v: PROTOCOL_VERSION,
      type: 'session.started',
      session_id: agent.id,
      cwd: this.workspace,
      model: selection.model,
      resumed,
    })
    return agent
  }

  /** Pins the tier's preset on the Session; later calls see it from their next confined operation. */
  private applyTier(session: Session): void {
    const presets = this.ctx.get('permissionPresets')
    if (presets === undefined) throw new Error('the permission-presets service is not available')
    presets.set(session, TIER_PRESET[this.tier])
  }

  private async userMessage(text: string, trace: string | undefined): Promise<void> {
    const agent = await this.ensureAgent()
    this.nextTurnParent = trace
    agent.followup(createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } }))
  }

  // ---------------------------------------------------------------------
  // Outbound projection, approvals, and spans

  private installListeners(): void {
    const { ctx } = this
    ctx.on('agent/assistant-stream', ({ agent, frame }) => {
      if (agent === this.agent) this.onStreamFrame(agent, frame)
    })
    ctx.on('session/event', (session, event) => {
      if (this.agent !== undefined && session === this.agent.session) this.onSessionEvent(this.agent, event)
    })

    // The desktop's tier decides what asks, what is refused, and what runs.
    ctx.on('tools/pre-execute', (exec, next): Promise<PreToolDecision> => {
      if (exec.agent === undefined || exec.agent !== this.agent) return next()
      const verdict = classifyTool(this.tier, exec.name, exec.arguments)
      if (verdict.kind === 'deny') return Promise.resolve({ kind: 'deny', reason: verdict.reason })
      if (verdict.kind === 'allow') return next()
      this.calls.set(exec.callId, {
        summary: verdict.summary,
        args: exec.arguments,
        ...verdict.grant === undefined ? {} : { grant: verdict.grant },
      })
      return Promise.resolve({ kind: 'ask', reason: `${exec.name} requires approval in normal mode` })
    })

    // Times the tool body alone, inside its pipeline span.
    ctx.on('tools/execute', async (exec, next) => {
      const parent = this.toolSpans.get(exec.callId)
      if (parent === undefined) return next()
      const span = this.recorder.start('tool.execute', parent, { tool: exec.name })
      try {
        const result = await next()
        span.end(result.isError ? 'error' : 'ok', { is_error: result.isError })
        return result
      } catch (error: unknown) {
        span.end('error')
        throw error
      }
    })

    ctx.on('approval/request', (request, next) => {
      if (request.agent !== this.agent) return next()
      return this.askDesktop(request.agent, request.toolName, request.callId, request.reason, request.signal)
    })
  }

  private askDesktop(
    agent: Agent,
    tool: string,
    callId: string | undefined,
    reason: string | undefined,
    signal: AbortSignal | undefined,
  ): Promise<ApprovalOutcome> {
    const context = callId === undefined ? undefined : this.calls.get(callId)
    if (callId !== undefined) this.calls.delete(callId)
    // Read-only means no escalation: the sandbox's own asks are refused here.
    if (this.tier === 'read-only') return Promise.resolve('rejected')
    if (signal?.aborted === true) return Promise.resolve('cancelled')
    const requestId = randomUUID()
    const parent = callId === undefined ? this.turnSpan : this.toolSpans.get(callId) ?? this.turnSpan
    const span = this.recorder.start('approval.wait', parent, { tool })
    return new Promise<ApprovalOutcome>((resolve) => {
      const settle = (outcome: ApprovalOutcome): void => {
        signal?.removeEventListener('abort', onAbort)
        span.end(outcome === 'allowed-once' ? 'ok' : 'error', { outcome })
        resolve(outcome)
      }
      const onAbort = (): void => {
        if (!this.approvals.delete(requestId)) return
        this.send({ v: PROTOCOL_VERSION, type: 'approval.withdrawn', request_id: requestId })
        settle('cancelled')
      }
      this.approvals.set(requestId, { resolve: settle })
      signal?.addEventListener('abort', onAbort, { once: true })
      this.send({
        v: PROTOCOL_VERSION,
        type: 'approval.request',
        trace: span.traceparent,
        request_id: requestId,
        session_id: agent.id,
        ...callId === undefined ? {} : { call_id: callId },
        tool,
        summary: context?.summary ?? reason ?? tool,
        args: context?.args ?? null,
        ...reason === undefined ? {} : { reason },
        ...context?.grant === undefined ? {} : { grant: context.grant },
      })
    })
  }

  private onStreamFrame(agent: Agent, frame: AssistantStreamFrame): void {
    if (frame.type === 'start') {
      this.closeStream(agent, 'abandoned')
      this.stream = {
        id: this.streamSeq++,
        text: '',
        reasoning: '',
        timer: undefined,
        llm: this.recorder.start('llm.request', this.stepSpan ?? this.turnSpan, { model: this.model }),
        startedUs: nowUs(),
        firstChunk: true,
      }
      return
    }
    const stream = this.stream
    if (stream === undefined) return
    if (frame.type === 'end') {
      this.closeStream(agent, frame.outcome.kind === 'committed' ? 'committed' : 'abandoned')
      return
    }
    const chunk = frame.chunk
    if (stream.firstChunk && stream.llm !== undefined) {
      stream.firstChunk = false
      stream.llm.set({ ttft_ms: Math.round((nowUs() - stream.startedUs) / 100) / 10 })
    }
    if (chunk.type === 'text-delta') stream.text += chunk.text
    else if (chunk.type === 'reasoning-delta') stream.reasoning += chunk.text
    else if (chunk.type === 'usage') {
      stream.llm?.set({ tokens_in: chunk.usage.inputTokens, tokens_out: chunk.usage.outputTokens })
      return
    } else return
    stream.timer ??= setTimeout(() => {
      this.flushDeltas(agent)
    }, DELTA_FLUSH_MS)
  }

  private flushDeltas(agent: Agent): void {
    const stream = this.stream
    if (stream === undefined) return
    if (stream.timer !== undefined) clearTimeout(stream.timer)
    stream.timer = undefined
    const base = { v: PROTOCOL_VERSION, type: 'message.delta', session_id: agent.id, agent_id: agent.id, stream_id: stream.id } as const
    if (stream.reasoning !== '') this.send({ ...base, channel: 'reasoning', text: stream.reasoning })
    if (stream.text !== '') this.send({ ...base, channel: 'text', text: stream.text })
    stream.text = ''
    stream.reasoning = ''
  }

  private closeStream(agent: Agent, outcome: 'committed' | 'abandoned'): void {
    const stream = this.stream
    if (stream === undefined) return
    this.flushDeltas(agent)
    stream.llm?.end(outcome === 'committed' ? 'ok' : 'error', { outcome })
    this.stream = undefined
    this.send({ v: PROTOCOL_VERSION, type: 'message.done', session_id: agent.id, agent_id: agent.id, stream_id: stream.id, outcome })
  }

  private onSessionEvent(agent: Agent, event: SessionEvent): void {
    const base = { v: PROTOCOL_VERSION, session_id: agent.id } as const
    switch (event.type) {
      case 'turn/start': {
        this.turnSpan = this.recorder.start('bake.turn', this.nextTurnParent, { session_id: agent.id, turn: event.data.turn })
        this.nextTurnParent = undefined
        this.send({ ...base, type: 'turn.started', trace: this.turnSpan.traceparent, turn: event.data.turn })
        return
      }
      case 'step/start':
        this.stepSpan = this.recorder.start('bake.step', this.turnSpan, { step: event.data.step })
        return
      case 'step/end':
        this.stepSpan?.end()
        this.stepSpan = undefined
        return
      case 'assistant/message': {
        const usage = event.data.usage
        if (usage === undefined) return
        this.send({
          ...base,
          type: 'usage',
          agent_id: agent.id,
          model: this.model,
          input_tokens: usage.inputTokens,
          output_tokens: usage.outputTokens,
          ...usage.cacheReadTokens === undefined ? {} : { cache_read_tokens: usage.cacheReadTokens },
          ...usage.reasoningTokens === undefined ? {} : { reasoning_tokens: usage.reasoningTokens },
        })
        return
      }
      case 'tool/call': {
        const span = this.recorder.start('tool.pipeline', this.stepSpan ?? this.turnSpan, { tool: event.data.name, call_id: event.data.callId })
        this.toolSpans.set(event.data.callId, span)
        this.send({
          ...base,
          type: 'tool.started',
          trace: span.traceparent,
          agent_id: agent.id,
          call_id: event.data.callId,
          name: event.data.name,
          args: parseArguments(event.data.arguments),
        })
        return
      }
      case 'tool/result': {
        // Compaction replaces older results on the surface; only this run's appends are new.
        if (event.surfaceOp !== 'append') return
        const block = event.data.message.content[0]
        const status = block.isError === true ? 'error' : 'completed'
        this.toolSpans.get(block.toolCallId)?.end(status === 'error' ? 'error' : 'ok')
        this.toolSpans.delete(block.toolCallId)
        this.calls.delete(block.toolCallId)
        const { text, truncated } = bound(resultText(block.content))
        this.send({ ...base, type: 'tool.finished', call_id: block.toolCallId, status, output: text, truncated })
        return
      }
      case 'turn/end': {
        const reason = event.data.reason
        const kind = reason.kind === 'completed' ? 'completed' : reason.kind === 'aborted' ? 'aborted' : 'error'
        const error = reason.kind === 'error'
          ? { code: reason.error.code, message: reason.error.message }
          : kind === 'error' ? { code: reason.kind.toUpperCase(), message: `turn ended: ${reason.kind}` } : undefined
        this.closeStream(agent, 'abandoned')
        for (const span of this.toolSpans.values()) span.end('error', { unfinished: true })
        this.toolSpans.clear()
        this.stepSpan?.end()
        this.stepSpan = undefined
        this.turnSpan?.end(kind === 'completed' ? 'ok' : 'error', { reason: reason.kind })
        const trace = this.turnSpan?.traceparent
        this.turnSpan = undefined
        // Spans first, so the desktop holds the complete turn when it sees the end.
        this.flushSpans()
        this.send({
          ...base,
          type: 'turn.done',
          ...trace === undefined ? {} : { trace },
          turn: event.data.turn,
          reason: kind,
          ...error === undefined ? {} : { error },
        })
        return
      }
      default:
        return
    }
  }
}

/**
 * Mount the desktop bridge.
 * @param ctx - plugin context carrying core services and the launcher-provided exit request.
 */
export function apply(ctx: Context): void {
  // Read through the global service store: appExit is an optional host value, never an injection.
  const exit = ctx.get('appExit')
  if (exit === undefined) {
    throw new Error('desktop-bridge: the launcher must provide ctx.appExit before the tree mounts')
  }
  const bridge = new DesktopBridge(ctx, exit)
  bridge.start()
  ctx.effect(() => () => {
    bridge.dispose()
  }, 'desktop bridge')
}
