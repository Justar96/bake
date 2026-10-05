/**
 * Public records for exact reads and provider-independent filters over the
 * live-preferred logical session corpus.
 *
 * @module bake-session-query/types
 */

import type {
  SessionEventType,
  SessionHeader,
  SessionId,
  SessionLogOffset,
  SessionSeq,
  OptionalSessionSeq,
  SurfaceEvent,
} from 'bake-session'
import type { SessionTitleSnapshot } from '@deepseek-ai/dsh-session-title'
import type { SessionSearchCursor } from './cursor.ts'

export type { SessionSearchCursor } from './cursor.ts'

/** Whether an event is current model context, replaced context, or raw-log-only. */
export type SessionEventSurface = 'current' | 'shadowed' | 'log-only'

/** Lightweight identity and source availability for one logical session. */
export interface SessionRecord {
  /** Cloned session header selected from the live-preferred corpus. */
  header: SessionHeader
  /** Whether the id currently exists in `ctx.sessions`. */
  live: boolean
  /** Whether the active persistence backend currently lists the id, including a created-but-unmaterialized session it already observes. */
  persisted: boolean
}

/** One atomic live-preferred observation of a session's current model surface. */
export interface SessionSurfaceSnapshot {
  /** Cloned session header selected from the same corpus observation as `events`. */
  session: SessionHeader
  /** Exact number of fork-inherited events in the observed log. */
  inheritedEventCount: SessionLogOffset
  /** Highest raw-log seq included in the observation, or `null` for an empty log. */
  capturedThroughSeq: OptionalSessionSeq
  /** Cloned current surface events in model-history order. */
  events: SurfaceEvent[]
}

/** Lightweight metadata for one event within a logical session. */
export interface SessionEventRecord {
  /** Session that owns the event. */
  sessionId: SessionId
  /** Monotonic event seq within the session. */
  seq: SessionSeq
  /** Discriminant of the session event. */
  type: SessionEventType
  /** Event timestamp in Unix epoch milliseconds. */
  time: number
  /** Event placement in the folded session surface. */
  surface: SessionEventSurface
}

/** Latest folded title bound to the same session-header observation. */
export interface SessionTitleObservation {
  /** Cloned header selected with the event log used for the title fold. */
  session: SessionHeader
  /** Latest title snapshot, absent when the observed log has no title. */
  title?: SessionTitleSnapshot
  /** Unix epoch milliseconds of the observed log's newest event, absent for an empty log. */
  lastEventAt?: number
  /** Present when the observed log started a turn; absent for a session opened and left without one. */
  startedTurn?: true
}

/** One ordered result from a batch title observation. */
export type SessionTitleObservationResult =
  | {
    /** Requested session id. */
    sessionId: SessionId
    /** Successful atomic header/title observation. */
    status: 'fulfilled'
    /** Header and optional latest title from one logical source. */
    value: SessionTitleObservation
  }
  | {
    /** Requested session id. */
    sessionId: SessionId
    /** Operational failure isolated to this session. */
    status: 'rejected'
    /** Original failure from logical-source resolution or title folding. */
    reason: unknown
  }

/** Inclusive numeric interval used by time and sequence filters. */
export interface SessionResultRange {
  /** Inclusive lower bound. */
  from?: number
  /** Inclusive upper bound. */
  to?: number
}

/** Source availability predicates understood by logical-session filters. */
export type SessionAvailability = 'live' | 'persisted'

/**
 * One logical-session predicate. A filter array is ANDed; `values` within a
 * clause are ORed.
 */
export type SessionResultFilter =
  | { kind: 'id'; values: readonly SessionId[] }
  | { kind: 'cwd'; values: readonly (string | null)[] }
  | ({ kind: 'created-at' } & SessionResultRange)
  | { kind: 'parent'; values: readonly (SessionId | null)[] }
  | { kind: 'availability'; values: readonly SessionAvailability[] }

/**
 * One event predicate. A filter array is ANDed; list-valued clauses are ORed.
 * Text is a literal, case-insensitive, whitespace-flexible semantic-text scan.
 */
export type SessionEventResultFilter =
  | ({ kind: 'seq' } & SessionResultRange)
  | ({ kind: 'time' } & SessionResultRange)
  | { kind: 'type'; values: readonly SessionEventType[] }
  | { kind: 'surface'; values: readonly SessionEventSurface[] }
  | { kind: 'text'; text: string }

/** Event predicates a full-text provider can apply before relevance ranking. */
export type SessionEventMetadataFilter = Exclude<SessionEventResultFilter, { kind: 'text' }>

/** Searchable semantic document derived from one session event. */
export interface SessionEventSearchDocument extends SessionEventRecord {
  /** First-party semantic text used by scan filters and full-text indexes. */
  text: string
}

/** One cursor-paginated result page. */
export interface SessionSearchPage<T> {
  /** Results for this page in contract-defined order. */
  items: readonly T[]
  /** Opaque continuation cursor, absent on the final page. */
  nextCursor?: SessionSearchCursor
}

/** Event-search results bound to the indexed target-session observation. */
export interface SessionEventSearchPage extends SessionSearchPage<SessionEventSearchHit> {
  /** Cloned target header from the same indexed generation as `items`. */
  session: SessionHeader
}

/** Controls shared by cross-session and within-session search calls. */
export interface SessionSearchExecContext {
  /** Abort caller waiting and interrupt provider work where supported. */
  signal?: AbortSignal
}

/** Cross-session full-text search request. */
export interface SessionSearchRequest {
  /** Full-text query interpreted as data, never executable FTS syntax. */
  query: string
  /** Logical-session predicates applied before event ranking. */
  sessionFilters?: readonly SessionResultFilter[]
  /** Event predicates applied before event ranking. */
  eventFilters?: readonly SessionEventMetadataFilter[]
  /** Maximum sessions in this page. */
  limit?: number
  /** Opaque cursor returned for the identical normalized request. */
  cursor?: SessionSearchCursor
}

/** Within-session full-text search request. */
export interface SessionEventSearchRequest {
  /** Session whose live-preferred logical log is searched. */
  sessionId: SessionId
  /** Full-text query interpreted as data, never executable FTS syntax. */
  query: string
  /** Event predicates applied before ranking. */
  filters?: readonly SessionEventMetadataFilter[]
  /** Maximum events in this page. */
  limit?: number
  /** Opaque cursor returned for the identical normalized request. */
  cursor?: SessionSearchCursor
}

/** One event full-text search hit with a bounded plain-text excerpt. */
export interface SessionEventSearchHit extends SessionEventRecord {
  /** Plain text excerpt selected around the match. */
  snippet: string
}

/** One grouped cross-session hit, ranked by its strongest matching event. */
export interface SessionSearchHit extends SessionRecord {
  /** Strongest matching event for this session. */
  bestMatch: SessionEventSearchHit
}
