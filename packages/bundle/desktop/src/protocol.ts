// Bake Desktop ↔ Bake harness protocol, version 1: one JSON object per
// message. Under Electron each message is a string on the utility process's
// message port; under plain Node each is a line on stdin (core → harness) or
// stdout (harness → core).
//
// This file is the source of truth. Bake vendors an identical copy at
// packages/bundle/desktop/src/protocol.ts; a desktop test checks that the
// two stay byte-identical. It is written in Bake's lint style (no
// semicolons), and the desktop formatter skips it for that reason.

/** Protocol version carried in every message. */
export const PROTOCOL_VERSION = 1

/** Largest accepted line in either direction, in bytes (UTF-8, excluding the newline). */
export const MAX_LINE_BYTES = 4 * 1024 * 1024

/**
 * The three permission tiers the desktop offers.
 * - `read-only`: the agent can read the workspace; writes and commands are refused.
 * - `normal`: ordinary tools run without asking; dangerous tools and shell commands ask.
 * - `full-access`: nothing asks.
 */
export type PermissionTier = 'read-only' | 'normal' | 'full-access'

export const PERMISSION_TIERS: readonly PermissionTier[] = ['read-only', 'normal', 'full-access']

/** A user's answer to an approval request. */
export type ApprovalDecision = 'approve_once' | 'approve_session' | 'deny'

/** Fields every message carries. `trace` is a W3C `traceparent`. */
interface Envelope<T extends string> {
  v: typeof PROTOCOL_VERSION
  type: T
  trace?: string
}

// ---------------------------------------------------------------------------
// Core → harness

export interface InitMessage extends Envelope<'init'> {
  /** Absolute workspace path; the harness process also runs with it as cwd. */
  workspace: string
  permission: PermissionTier
  /** Resume this Bake session instead of starting a new one on the first message. */
  resume_session_id?: string
}

export interface UserMessage extends Envelope<'user.message'> {
  text: string
}

export interface ApprovalResultMessage extends Envelope<'approval.result'> {
  request_id: string
  decision: ApprovalDecision
}

export interface CancelMessage extends Envelope<'cancel'> {
  session_id: string
}

export interface PermissionSetMessage extends Envelope<'permission.set'> {
  permission: PermissionTier
}

export interface ClockPingMessage extends Envelope<'clock.ping'> {
  id: number
}

export interface ShutdownMessage extends Envelope<'shutdown'> {}

export type CoreMessage =
  | InitMessage
  | UserMessage
  | ApprovalResultMessage
  | CancelMessage
  | PermissionSetMessage
  | ClockPingMessage
  | ShutdownMessage

// ---------------------------------------------------------------------------
// Harness → core

export interface ReadyMessage extends Envelope<'ready'> {
  harness_version: string
  protocol: typeof PROTOCOL_VERSION
  pid: number
}

export interface InitializedMessage extends Envelope<'initialized'> {
  workspace: string
  permission: PermissionTier
  model: string
}

export interface SessionStartedMessage extends Envelope<'session.started'> {
  session_id: string
  cwd: string
  model: string
  resumed: boolean
}

export interface TurnStartedMessage extends Envelope<'turn.started'> {
  session_id: string
  turn: number
}

export interface MessageDeltaMessage extends Envelope<'message.delta'> {
  session_id: string
  agent_id: string
  /** Identifies one streamed assistant message within the session. */
  stream_id: number
  channel: 'text' | 'reasoning'
  text: string
}

export interface MessageDoneMessage extends Envelope<'message.done'> {
  session_id: string
  agent_id: string
  stream_id: number
  outcome: 'committed' | 'abandoned'
}

export interface ToolStartedMessage extends Envelope<'tool.started'> {
  session_id: string
  agent_id: string
  call_id: string
  name: string
  /** Parsed arguments, or the raw text when they are not valid JSON; strings are bounded. */
  args: unknown
}

export interface ToolFinishedMessage extends Envelope<'tool.finished'> {
  session_id: string
  call_id: string
  status: 'completed' | 'error'
  /** The tool result's text content, bounded. */
  output: string
  truncated: boolean
}

export interface ApprovalRequestMessage extends Envelope<'approval.request'> {
  request_id: string
  session_id: string
  call_id?: string
  tool: string
  /** One-line description of what the tool will do, for the approval card. */
  summary: string
  args: unknown
  /** Why the policy asked, when it says. */
  reason?: string
  /**
   * Key for an "approve for this session" grant, and its label for the card
   * (for example `shell:git` / "`git` commands"). Absent when a session
   * grant is not offered, such as for sandbox escalations.
   */
  grant?: { key: string; label: string }
}

export interface ApprovalWithdrawnMessage extends Envelope<'approval.withdrawn'> {
  request_id: string
}

export interface UsageMessage extends Envelope<'usage'> {
  session_id: string
  agent_id: string
  model: string
  input_tokens: number
  output_tokens: number
  cache_read_tokens?: number
  reasoning_tokens?: number
}

export interface TurnDoneMessage extends Envelope<'turn.done'> {
  session_id: string
  turn: number
  reason: 'completed' | 'aborted' | 'error'
  error?: { code: string; message: string }
}

/** A span as the harness records it (same shape as the desktop's wire span). */
export interface HarnessSpan {
  traceId: string
  spanId: string
  parentSpanId?: string
  name: string
  lane?: string
  startUs: number
  endUs: number
  status: 'ok' | 'error'
  attrs: Record<string, string | number | boolean>
}

export interface TelemetrySpansMessage extends Envelope<'telemetry.spans'> {
  spans: HarnessSpan[]
}

export interface ClockPongMessage extends Envelope<'clock.pong'> {
  id: number
  now_us: number
}

export interface ErrorMessage extends Envelope<'error'> {
  /** A fatal error means the harness is exiting. */
  fatal: boolean
  code: string
  message: string
}

export type HarnessMessage =
  | ReadyMessage
  | InitializedMessage
  | SessionStartedMessage
  | TurnStartedMessage
  | MessageDeltaMessage
  | MessageDoneMessage
  | ToolStartedMessage
  | ToolFinishedMessage
  | ApprovalRequestMessage
  | ApprovalWithdrawnMessage
  | UsageMessage
  | TurnDoneMessage
  | TelemetrySpansMessage
  | ClockPongMessage
  | ErrorMessage

export type CoreMessageType = CoreMessage['type']
export type HarnessMessageType = HarnessMessage['type']

/** Every message type the harness may send, for decoders that check membership. */
export const HARNESS_MESSAGE_TYPES: readonly HarnessMessageType[] = [
  'ready',
  'initialized',
  'session.started',
  'turn.started',
  'message.delta',
  'message.done',
  'tool.started',
  'tool.finished',
  'approval.request',
  'approval.withdrawn',
  'usage',
  'turn.done',
  'telemetry.spans',
  'clock.pong',
  'error',
]

/** Every message type the core may send. */
export const CORE_MESSAGE_TYPES: readonly CoreMessageType[] = [
  'init',
  'user.message',
  'approval.result',
  'cancel',
  'permission.set',
  'clock.ping',
  'shutdown',
]
