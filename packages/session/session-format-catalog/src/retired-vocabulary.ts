/**
 * Event and message-source vocabulary that released Session logs carry but no
 * mounted plugin writes any more.
 *
 * Bake dropped the features that wrote these records: the web session
 * controller (`model/selection` and browser-correlated `user` sources), Agent
 * Teams (`team/*` and `team-message` sources), workspace change summaries
 * (`workspace/changes`), plan mode (`plan/mode`), Session-log delivery to
 * the DeepSeek API (`session-log-deepseek/delivery-accepted`), webhook rules
 * that started Sessions (`webhook` sources), the `todo_write` task list
 * (`todo/write`), per-message feedback (`feedback/message-put` and
 * `feedback/message-delete`), the `present` tool (`deliverables/presented`),
 * and the `workflow` tool (`tool-workflow/*`). Released formats v0 through v3
 * admit them, and a log that holds one must still open. The storage contract refuses
 * any unknown event type that is not marked ignorable, and the known set is
 * generated from these declarations, so removing one would refuse such a log
 * as if a newer harness had written it.
 *
 * The declarations keep the resolved types each record was released with, so
 * the persistence schema's digests do not move. Nothing here is written. Delete
 * an entry only with the Session-format version bump and migration that
 * retires its records.
 *
 * @module @deepseek-ai/dsh-session-format-catalog/retired-vocabulary
 */
import type { Branded } from '@deepseek-ai/dsh-brand'
import type { ContentBlock } from '@deepseek-ai/dsh-llm/types'
import type { MessageId, ToolCallId } from '@deepseek-ai/dsh-llm/brand'
import type { SessionId } from 'bake-session/types'

/** Complete model selection a web client recorded for one Session. */
export interface RetiredModelSelection {
  readonly provider: string
  readonly model: string
  readonly reasoningEffort?: string
}

/** Client-minted prompt identity a web client used to reconcile its messages. */
export type RetiredSessionRequestId = Branded<'session-request-id'>

/** Identifies the webhook rule that admitted a message. */
export type RetiredWebhookRuleId = Branded<'WebhookRuleId'>

/** Identifies the configured webhook adapter a delivery came through. */
export type RetiredWebhookSourceId = Branded<'WebhookSourceId'>

/** Identifies one provider delivery. */
export type RetiredWebhookDeliveryId = Branded<'WebhookDeliveryId'>

/** The implicit team rooted at one top-level Session. */
export type RetiredTeamId = Branded<'TeamId'>

/** One task in a Team. */
export type RetiredTeamTaskId = Branded<'TeamTaskId'>

/** One durable peer message. */
export type RetiredTeamMessageId = Branded<'TeamMessageId'>

/** Teammate lifecycle. */
export type RetiredTeamMemberPhase = 'provisioning' | 'active' | 'failed'

/** Whole teammate value written on every lifecycle change. */
export interface RetiredTeamMemberSnapshot {
  readonly id: SessionId
  readonly name: string
  readonly description: string
  readonly provider: string
  readonly context: 'fresh' | 'fork'
  readonly phase: RetiredTeamMemberPhase
  readonly error?: string
}

/** Task lifecycle. */
export type RetiredTeamTaskStatus = 'pending' | 'in_progress' | 'completed' | 'deleted'

/** Whole task value; every mutation incremented `revision`. */
export interface RetiredTeamTaskSnapshot {
  readonly id: RetiredTeamTaskId
  readonly revision: number
  readonly subject: string
  readonly description: string
  readonly status: RetiredTeamTaskStatus
  readonly ownerId?: SessionId
  readonly blockedBy: RetiredTeamTaskId[]
  readonly writeScopes: string[]
}

/** One peer message retained until its target Session recorded it. */
export interface RetiredTeamMessageSnapshot {
  readonly id: RetiredTeamMessageId
  readonly senderId: SessionId
  readonly senderName: string
  readonly targetId: SessionId
  readonly content: ContentBlock[]
}

/** One entry of a `todo/write` whole-list snapshot. */
export interface RetiredTodoItem {
  readonly content: string
  readonly status: 'pending' | 'in_progress' | 'completed'
}

/** Feedback category a human filed one message rating under. */
export type RetiredFeedbackCategory =
  | 'task-result'
  | 'instruction-following'
  | 'product-interaction'
  | 'service-stability'
  | 'resource-cost'
  | 'security-privacy-permission'
  | 'other'

/** Opaque compare-and-set token of one feedback item revision. */
export type RetiredMessageFeedbackVersion = Branded<'MessageFeedbackVersion'>

/** Whole feedback value for one assistant message after a create or edit. */
export interface RetiredMessageFeedbackItem {
  readonly messageId: MessageId
  readonly rating: 'positive' | 'negative'
  readonly note?: string
  readonly category?: RetiredFeedbackCategory
  readonly version: RetiredMessageFeedbackVersion
  readonly createdAt: number
  readonly updatedAt: number
}

/** A declared file one `present` call delivered from its source path. */
export interface RetiredPresentedFile {
  path: string
  description?: string
}

/** Identifies one workflow run. */
export type RetiredWorkflowRunId = Branded<'WorkflowRunId'>

/** Source a target Session recorded for mailbox de-duplication. */
export interface RetiredTeamMessageSource {
  readonly kind: 'team-message'
  readonly teamId: RetiredTeamId
  readonly messageId: RetiredTeamMessageId
  readonly senderId: SessionId
  readonly senderName: string
}

declare module '@deepseek-ai/dsh-llm' {
  interface MessageSourceMap {
    /** A web client's prompt correlation and optional validated time zone. */
    'user-rpc': { kind: 'user'; rpcId: RetiredSessionRequestId; clientTimeZone?: string }
    /** A peer message delivered by Agent Teams. */
    'team-message': RetiredTeamMessageSource
    /** Programmatic input admitted from one verified webhook rule. */
    webhook: {
      readonly kind: 'webhook'
      readonly provider: string
      readonly source: RetiredWebhookSourceId
      readonly deliveryId: RetiredWebhookDeliveryId
      readonly ruleId: RetiredWebhookRuleId
      readonly form: 'notice'
      readonly summary: string
    }
  }
}

declare module 'bake-session/types' {
  interface SessionEventMap {
    /** Model selection a web client requested for later prompt assembly. Log-only. */
    'model/selection': RetiredModelSelection
    /** Whole teammate lifecycle value, stored only in the Team Lead Session. */
    'team/member': { version: 2; teamId: RetiredTeamId; member: RetiredTeamMemberSnapshot }
    /** Whole shared-task value, stored only in the Team Lead Session. */
    'team/task': { version: 2; teamId: RetiredTeamId; task: RetiredTeamTaskSnapshot }
    /** Mailbox enqueue, stored before delivery was attempted. */
    'team/message/queued': { version: 2; teamId: RetiredTeamId; message: RetiredTeamMessageSnapshot }
    /** Acknowledgement that the target Session recorded the message. */
    'team/message/delivered': {
      version: 2
      teamId: RetiredTeamId
      messageId: RetiredTeamMessageId
      targetId: SessionId
    }
    /** A completed turn's changed files were summarized on the host; only the turn is logged. */
    'workspace/changes': { turn: number }
    /** Whole todo-list snapshot the `todo_write` tool wrote; the last record won. Log-only. */
    'todo/write': { todos: RetiredTodoItem[] }
    /** Log-only feedback value for one assistant message; inherited feedback in a fork names its parent. */
    'feedback/message-put': { readonly sessionId: SessionId; readonly item: RetiredMessageFeedbackItem }
    /** Log-only removal of one message's feedback. */
    'feedback/message-delete': { readonly sessionId: SessionId; readonly messageId: MessageId }
    /** Files a successful `present` result declared, including nested calls. */
    'deliverables/presented': { turn: number; callId: ToolCallId; files: RetiredPresentedFile[] }
    /** Opens one top-level workflow run record. */
    'tool-workflow/run-start': { readonly runId: RetiredWorkflowRunId; readonly name: string }
    /** Records one workflow member after its child Session was published. */
    'tool-workflow/agent-start': {
      readonly runId: RetiredWorkflowRunId
      readonly seq: number
      readonly label: string
      readonly phase?: string
      readonly childId: SessionId
    }
    /** Settles one workflow member. */
    'tool-workflow/agent-end': {
      readonly runId: RetiredWorkflowRunId
      readonly seq: number
      readonly outcome: 'completed' | 'failed' | 'cancelled'
    }
    /** Closes one workflow run record after cleanup. */
    'tool-workflow/run-end': { readonly runId: RetiredWorkflowRunId; readonly stopReason: 'completed' | 'cancelled' | 'error' }
    /** Whether plan mode was in force from this point on; the last record won. */
    'plan/mode': { active: boolean }
    /** Records that the configured endpoint accepted one delivery through `throughSeq`. */
    'session-log-deepseek/delivery-accepted': {
      /** Session identity the accepted delivery carried; inherited fork markers retain the parent's id. */
      sessionId: SessionId
      /** Accepted Session format generation; absence identifies version 0. */
      sessionFormatVersion?: number
      /** Last canonical event included in the accepted request. */
      throughSeq: SessionSeq
    }
  }
}
