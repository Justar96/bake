/** Display-only evidence captured when a child is admitted; never part of model history. */

import type { ToolCallId } from 'bake-llm'
import type { SessionId } from 'bake-session'

/** An optional ing assessment, with finite normalized difficulty and bounded explanatory text. */
export interface SubagentRoutingAssessment {
  readonly policy: string
  readonly status: 'normal' | 'cautious' | 'needs_context' | 'fallback'
  readonly difficulty: number
  readonly reasons: readonly string[]
}

/** The router's explanation, including refusals that leave the effective default route in place. */
export interface SubagentRouterDecision {
  readonly reason: string
  readonly fallback: boolean
  readonly assessment?: SubagentRoutingAssessment
}

/** One successfully admitted child's route decision, owned by its direct parent's Session. */
export interface SubagentRoutingDecision {
  readonly childId: SessionId
  readonly callId: ToolCallId
  readonly source: 'explicit' | 'default' | 'auto' | 'fallback'
  /** Effective child route when known. Omitted effort means the resolved effort is unknown. */
  readonly route?: {
    readonly provider: string
    readonly model: string
    readonly reasoningEffort?: string
  }
  readonly router?: SubagentRouterDecision
}
