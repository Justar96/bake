/** Parent-owned, log-only route decisions for child inspection and Session replay. */

import { z } from 'zod'
import type { ToolCallId } from 'bake-llm'
import type { Session, SessionId, SessionLogOffset } from 'bake-session'
import type SessionProjectionRegistry from 'bake-session-projection'
import type { ProjectionDefinition } from 'bake-session-projection'
import type { SubagentRoutingDecision } from './types.ts'

declare module 'bake-session/types' {
  interface SessionEventMap {
    /** A successfully admitted child's route decision; no surfaceOp, so it never enters model history. */
    'subagent/routing-decision': SubagentRoutingDecision
  }
}

interface RoutingState {
  readonly inheritedEventCount: SessionLogOffset
  readonly decisions: Record<string, SubagentRoutingDecision>
}

declare module 'bake-session-projection/types' {
  interface SessionProjectionMap {
    /** Display decisions indexed by direct-child id, including after Session restoration. */
    subagentRoutingDecisions: Record<string, SubagentRoutingDecision>
  }
  interface SessionProjectionStateMap {
    /** Direct-child decisions only; inherited fork history is excluded. */
    subagentRoutingDecisions: RoutingState
  }
}

const decisionSchema = z.object({
  childId: z.string() as unknown as z.ZodType<SessionId>,
  callId: z.string() as unknown as z.ZodType<ToolCallId>,
  source: z.enum(['explicit', 'default', 'auto', 'fallback']),
  route: z.object({
    provider: z.string().min(1), model: z.string().min(1), reasoningEffort: z.string().min(1).optional(),
  }).strict().optional(),
  router: z.object({
    reason: z.string().max(1000), fallback: z.boolean(),
    assessment: z.object({
      policy: z.string().max(128),
      status: z.enum(['normal', 'cautious', 'needs_context', 'fallback']),
      difficulty: z.number().finite().min(0).max(1),
      reasons: z.array(z.string().max(480)).max(8),
    }).strict().optional(),
  }).strict().optional(),
}).strict() as unknown as z.ZodType<SubagentRoutingDecision>

/** Fold only successful delegations created by this Session, excluding inherited parent decisions. */
export const subagentRoutingProjectionDefinition = {
  key: 'subagentRoutingDecisions',
  stateVersion: 1,
  stateSchema: z.object({
    inheritedEventCount: z.number().int().nonnegative() as unknown as z.ZodType<SessionLogOffset>,
    decisions: z.record(z.string(), decisionSchema),
  }).strict(),
  init: (_header, inheritedEventCount) => ({ inheritedEventCount, decisions: {} }),
  apply: (state, event) => {
    if (event.type !== 'subagent/routing-decision' || event.seq < state.inheritedEventCount) return state
    const decision = decisionSchema.parse(event.data)
    return { ...state, decisions: { ...state.decisions, [decision.childId]: decision } }
  },
  wire: { viewSchema: z.record(z.string(), decisionSchema), view: (state: RoutingState) => state.decisions },
} satisfies ProjectionDefinition<'subagentRoutingDecisions', RoutingState>

/**
 * Read a child's committed routing explanation from its parent's authoritative projection.
 * @param projections - registry owning routing state.
 * @param session - direct parent Session, including a restored Session.
 * @param childId - successfully admitted child to inspect.
 * @returns its recorded decision, or undefined for old Sessions and unrelated children.
 */
export function subagentRoutingDecision(
  projections: Pick<SessionProjectionRegistry, 'stateOf'>,
  session: Session,
  childId: SessionId,
): SubagentRoutingDecision | undefined {
  const decisions = projections.stateOf(session, 'subagentRoutingDecisions')?.decisions
  return decisions !== undefined && Object.hasOwn(decisions, childId) ? decisions[childId] : undefined
}

/**
 * Record a successful child admission; rejected starts must never call this helper.
 * @param parent - direct parent owning the delegation.
 * @param decision - bounded display-only route evidence, never the rejected router suggestion.
 */
export function recordSubagentRoutingDecision(parent: Session, decision: SubagentRoutingDecision): void {
  parent.append('subagent/routing-decision', decisionSchema.parse(decision), { ignorable: true })
}
