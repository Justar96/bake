/** Canonical current-surface fold shared by exact session reads. */

import { currentSessionMessageProjections } from '@deepseek-ai/dsh-session-format-catalog/message-projections'
import { foldSurface, isSurfaceEvent, snapshotSessionEvent } from '@deepseek-ai/dsh-session'
import type {
  SessionEvent,
  SurfaceEvent,
} from '@deepseek-ai/dsh-session'
import { SessionQueryError } from './config.ts'

/**
 * Fold and return the current model surface after validating the whole log.
 * @param events - detached raw event log from one corpus observation.
 * @returns detached current surface events in folded order.
 */
export function currentSurfaceEvents(
  events: readonly SessionEvent[],
): SurfaceEvent[] {
  let folded: ReturnType<typeof foldSurface>
  try {
    folded = foldSurface(events, currentSessionMessageProjections)
  } catch (error: unknown) {
    throw new SessionQueryError(
      /* v8 ignore next -- foldSurface throws Error instances */
      `invalid session surface: ${error instanceof Error ? error.message : 'unknown error'}`,
      'SESSION_QUERY_INVALID_SURFACE',
      { cause: error },
    )
  }
  return [...folded.nodes].map((seq) => {
    const event = events[seq]
    /* v8 ignore next 6 -- foldSurface validated contiguous seqs and returned only surface-event seqs. */
    if (event === undefined || event.seq !== seq || !isSurfaceEvent(event)) {
      throw new SessionQueryError(
        `invalid session surface: current node ${seq} is not a surface event`,
        'SESSION_QUERY_INVALID_SURFACE',
      )
    }
    return snapshotSessionEvent(event)
  })
}
