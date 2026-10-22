/**
 * Service Definition for combined session-history reads, filters, and full-text search.
 *
 * @module bake-session-query
 */

import { Context, Service } from '@deepseek-ai/cordis'
import type { SessionId } from 'bake-session'
import { foldSessionTitle } from 'bake-session-title'
import type {
  SessionEventSearchPage,
  SessionEventSearchRequest,
  SessionRecord,
  SessionResultFilter,
  SessionSearchExecContext,
  SessionSearchHit,
  SessionSearchPage,
  SessionSearchRequest,
  SessionSurfaceSnapshot,
  SessionTitleObservation,
  SessionTitleObservationResult,
} from './types.ts'
import {
  SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY,
  SESSION_QUERY_DEFAULT_PREPARED_SESSION_CACHE_SIZE,
  SESSION_QUERY_READ_WINDOW_MAX,
  SessionQueryError,
  type Config,
} from './config.ts'
import { SessionCorpus } from './corpus.ts'
import {
  SessionObservationReader,
  type SessionObservation,
  type SessionObservationOptions,
} from './observation.ts'
import {
  filterSessionResults,
  materializeSessionResultFilters,
} from './filters.ts'
import { currentSurfaceEvents } from './tracing.ts'

export type * from './types.ts'
export { SessionSearchCursor } from './cursor.ts'
export type { Config, SessionQueryErrorCode } from './config.ts'
export {
  SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY,
  SESSION_QUERY_DEFAULT_PREPARED_SESSION_CACHE_SIZE,
  SESSION_QUERY_READ_WINDOW_MAX,
  SessionQueryError,
} from './config.ts'
export { readColdSessionLog } from './cold-read.ts'
export type { ColdSessionLog } from './cold-read.ts'
export { extractSessionEventText } from './extraction.ts'
export { buildSessionEventRecords, buildSessionEventSearchDocuments } from './documents.ts'
export {
  compileSessionTextFilter,
  filterSessionResults,
  materializeSessionEventResultFilters,
  materializeSessionResultFilters,
} from './filters.ts'
export { assertSessionHeadersCompatible } from './sources.ts'
export type { SessionObservation, SessionObservationOptions } from './observation.ts'

declare module '@deepseek-ai/cordis' {
  interface Context {
    sessionQuery: SessionQueryEngine
  }
}

/**
 * Unified live-preferred session query service.
 *
 * Exact reads and filters are backend-independent concrete behavior.
 * A backend implements full-text observation, reconciliation, ranking, cursor
 * generations, and query execution on the same `ctx.sessionQuery` service.
 */
export abstract class SessionQueryEngine extends Service {
  static inject = ['sessions']

  private readonly _corpus: SessionCorpus
  private readonly _observations: SessionObservationReader

  constructor(ctx: Context, config: Config = {}) {
    super(ctx, 'sessionQuery')
    const readWindowMax = config.readWindowMax ?? SESSION_QUERY_READ_WINDOW_MAX
    if (!Number.isInteger(readWindowMax) || readWindowMax < 0) {
      throw new SessionQueryError(
        'session-query: readWindowMax must be a non-negative integer',
        'SESSION_QUERY_INVALID_CONFIG',
      )
    }
    const persistedReadConcurrency = config.persistedReadConcurrency
      ?? SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY
    if (!Number.isSafeInteger(persistedReadConcurrency) || persistedReadConcurrency < 1) {
      throw new SessionQueryError(
        'session-query: persistedReadConcurrency must be a positive safe integer',
        'SESSION_QUERY_INVALID_CONFIG',
      )
    }
    const preparedSessionCacheSize = config.preparedSessionCacheSize
      ?? SESSION_QUERY_DEFAULT_PREPARED_SESSION_CACHE_SIZE
    if (!Number.isSafeInteger(preparedSessionCacheSize) || preparedSessionCacheSize < 1) {
      throw new SessionQueryError(
        'session-query: preparedSessionCacheSize must be a positive safe integer',
        'SESSION_QUERY_INVALID_CONFIG',
      )
    }
    this._corpus = new SessionCorpus(ctx, persistedReadConcurrency)
    this._observations = new SessionObservationReader(ctx, preparedSessionCacheSize)
  }

  /**
   * Observe one exact live or prepared Session without a persistence listing preflight.
   * @param sessionId - logical Session identity.
   * @param options - cancellation and projection selection for this read.
   * @returns a caller-owned observation lease.
   */
  observeSession(
    sessionId: SessionId,
    options: SessionObservationOptions = {},
  ): Promise<SessionObservation> {
    return this._observations.read(sessionId, options)
  }

  /**
   * Search the live-preferred logical corpus and group by session.
   * @param request - query text, metadata filters, page size, and cursor.
   * @param exec - optional cancellation control.
   * @returns session hits ranked by their strongest matching event.
   */
  abstract searchSessions(
    request: SessionSearchRequest,
    exec?: SessionSearchExecContext,
  ): Promise<SessionSearchPage<SessionSearchHit>>

  /**
   * Search events within one live-preferred logical session.
   * @param request - target session, query text, filters, page size, and cursor.
   * @param exec - optional cancellation control.
   * @returns matching event hits and their target header from one indexed generation.
   */
  abstract searchEvents(
    request: SessionEventSearchRequest,
    exec?: SessionSearchExecContext,
  ): Promise<SessionEventSearchPage>

  /**
   * List the complete logical corpus using live-preferred records.
   * @param signal - optional cancellation for persistence listing.
   * @returns deterministic newest-first cloned session records.
   */
  listSessions(signal?: AbortSignal): Promise<SessionRecord[]> {
    return this._corpus.listSessions(signal)
  }

  /**
   * Filter the complete logical corpus with provider-independent predicates.
   * @param filters - ANDed session metadata and availability clauses.
   * @param signal - optional cancellation for persistence listing.
   * @returns matching cloned records in deterministic newest-first order.
   */
  async filterSessions(
    filters: readonly SessionResultFilter[],
    signal?: AbortSignal,
  ): Promise<SessionRecord[]> {
    const ownedFilters = materializeSessionResultFilters(filters)
    return this._filterSessions(ownedFilters, signal)
  }

  /**
   * Fold titles for unique sessions from one cancellable corpus observation.
   *
   * Results preserve first-occurrence input order. Operational failures stay
   * isolated per session, while cancellation rejects the complete operation.
   * @param sessionIds - live or persisted session ids to observe.
   * @param signal - optional cancellation shared by all source reads.
   * @returns one fulfilled or rejected result per unique requested id.
   */
  async readTitleSnapshots(
    sessionIds: readonly SessionId[],
    signal?: AbortSignal,
  ): Promise<SessionTitleObservationResult[]> {
    return this._corpus.projectMany(sessionIds, (source): SessionTitleObservation => {
      const title = foldSessionTitle(source.events)
      // From the same log as the title, so a picker orders by recent use, and
      // leaves out sessions that never started a turn, without a second read.
      const lastEventAt = source.events.at(-1)?.time
      const startedTurn = source.events.some(event => event.type === 'turn/start')
      return {
        session: structuredClone(source.header),
        ...title === undefined ? {} : { title },
        ...lastEventAt === undefined ? {} : { lastEventAt },
        ...startedTurn ? { startedTurn: true as const } : {},
      }
    }, signal)
  }

  private async _filterSessions(
    filters: readonly SessionResultFilter[],
    signal?: AbortSignal,
  ): Promise<SessionRecord[]> {
    return filterSessionResults(await this._corpus.listSessions(signal), filters)
  }

  /**
   * Read one session's complete current model surface from one corpus observation.
   * @param sessionId - live-preferred session id to read.
   * @returns cloned header, current surface, and the last sequence number included in the raw-log capture.
   * @throws when source resolution fails or the session surface is invalid.
   */
  async readSurface(sessionId: SessionId): Promise<SessionSurfaceSnapshot> {
    const loaded = await this._corpus.load(sessionId)
    return {
      session: structuredClone(loaded.header),
      inheritedEventCount: loaded.inheritedEventCount,
      capturedThroughSeq: loaded.events.at(-1)?.seq ?? null,
      events: currentSurfaceEvents(loaded.events),
    }
  }
}

export default SessionQueryEngine
