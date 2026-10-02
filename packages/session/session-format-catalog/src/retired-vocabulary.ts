/**
 * Event and message-source vocabulary that released Session logs carry but no
 * mounted plugin writes any more.
 *
 * Bake dropped the features that wrote these records: the web session
 * controller (`model/selection` and browser-correlated `user` sources), Agent
 * Teams (`team/*` and `team-message` sources), workspace change summaries
 * (`workspace/changes`), plan mode (`plan/mode`), and Session-log delivery to
 * the DeepSeek API (`session-log-deepseek/delivery-accepted`). Released formats v0 through v3 admit
 * them, and a log that holds one must still open. The storage contract refuses
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
import type { SessionId } from '@deepseek-ai/dsh-session/types'

/** Complete model selection a web client recorded for one Session. */
export interface RetiredModelSelection {
  readonly provider: string
  readonly model: string
  readonly reasoningEffort?: string
}

/** Client-minted prompt identity a web client used to reconcile its messages. */
export type RetiredSessionRequestId = Branded<'session-request-id'>

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
  }
}

declare module '@deepseek-ai/dsh-session/types' {
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
