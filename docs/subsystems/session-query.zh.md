# 会话查询

[English](session-query.md) | 中文

本文定义逻辑会话语料库的查询词汇；当 live 数据存在时，该语料库优先使用 live 数据。[Service Definition 包](../../packages/session-query/session-query)负责精确读取、来源优先级、关系追踪、语义提取，以及与提供方无关的过滤器；[SQLite 提供方](../../packages/session-query/session-query-sqlite)负责具体全文索引的生命周期。

源码：[`packages/session-query/session-query/src/types.ts`](../../packages/session-query/session-query/src/types.ts)

## 逻辑记录

`SessionRecord` 由全语料库列表返回。它除了克隆的、优先取自 live 源的 header 外，还单独公开各源的可用性。`SessionEventRecord` 是轻量的原始日志投影；分类使用与模型历史推导相同的 `foldSurface()` 状态转换。

```ts type-equiv
/** Whether an event is current model context, replaced context, or raw-log-only. */
type SessionEventSurface = 'current' | 'shadowed' | 'log-only'
```

```ts type-equiv
/** Lightweight identity and source availability for one logical session. */
interface SessionRecord {
  /** Cloned session header selected from the live-preferred corpus. */
  header: SessionHeader
  /** Whether the id currently exists in `ctx.sessions`. */
  live: boolean
  /** Whether the active persistence backend currently lists the id, including a created-but-unmaterialized session it already observes. */
  persisted: boolean
}
```

`SessionSurfaceSnapshot` 表示一次精确读取的 surface 观测结果，而不是持续保留的订阅。

```ts type-equiv
/** One atomic live-preferred observation of a session's current model surface. */
interface SessionSurfaceSnapshot {
  /** Cloned session header selected from the same corpus observation as `events`. */
  session: SessionHeader
  /** Exact number of fork-inherited events in the observed log. */
  inheritedEventCount: SessionLogOffset
  /** Highest raw-log seq included in the observation, or `null` for an empty log. */
  capturedThroughSeq: OptionalSessionSeq
  /** Cloned current surface events in model-history order. */
  events: SurfaceEvent[]
}
```

`SessionTitleObservation` 将同样的原子观测规则应用于标题折叠，使执行授权检查的消费方能够验证提供标题的源 header。同一次观测还报告 `lastEventAt`，即该日志最新事件的时间，因此调用方无需再次读取即可按最近活动对会话排序；该日志开始过轮次时还会报告 `startedTurn`，使选择器可以略去打开后从未使用的会话。批量读取会按顺序为每个唯一请求 id 返回一个 `SessionTitleObservationResult`：操作失败只影响对应 id，而取消会拒绝整个操作。

```ts type-equiv
/** Latest folded title bound to the same session-header observation. */
interface SessionTitleObservation {
  /** Cloned header selected with the event log used for the title fold. */
  session: SessionHeader
  /** Latest title snapshot, absent when the observed log has no title. */
  title?: SessionTitleSnapshot
  /** Unix epoch milliseconds of the observed log's newest event, absent for an empty log. */
  lastEventAt?: number
  /** Present when the observed log started a turn; absent for a session opened and left without one. */
  startedTurn?: true
}
```

```ts type-equiv
/** One ordered result from a batch title observation. */
type SessionTitleObservationResult =
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
```

```ts type-equiv
/** Lightweight metadata for one event within a logical session. */
interface SessionEventRecord {
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
```

## 与提供方无关的过滤器和文档

会话和事件过滤器数组内的各项按逻辑与（AND）组合；单个列表子句中的各值按逻辑或（OR）组合。范围包含两端。事件的 `text` 子句会对提取出的语义文本执行正则表达式扫描：搜索文本按字面量处理，按 Unicode 规则执行不区分大小写的匹配，并允许灵活匹配空白字符；该过程与全文搜索提供方无关。

```ts type-equiv
/**
 * One logical-session predicate. A filter array is ANDed; `values` within a
 * clause are ORed.
 */
type SessionResultFilter =
  | { kind: 'id'; values: readonly SessionId[] }
  | { kind: 'cwd'; values: readonly (string | null)[] }
  | ({ kind: 'created-at' } & SessionResultRange)
  | { kind: 'parent'; values: readonly (SessionId | null)[] }
  | { kind: 'availability'; values: readonly SessionAvailability[] }
```

```ts type-equiv
/**
 * One event predicate. A filter array is ANDed; list-valued clauses are ORed.
 * Text is a literal, case-insensitive, whitespace-flexible semantic-text scan.
 */
type SessionEventResultFilter =
  | ({ kind: 'seq' } & SessionResultRange)
  | ({ kind: 'time' } & SessionResultRange)
  | { kind: 'type'; values: readonly SessionEventType[] }
  | { kind: 'surface'; values: readonly SessionEventSurface[] }
  | { kind: 'text'; text: string }
```

```ts type-equiv
/** Searchable semantic document derived from one session event. */
interface SessionEventSearchDocument extends SessionEventRecord {
  /** First-party semantic text used by scan filters and full-text indexes. */
  text: string
}
```

`ctx.sessionQuery.filterSessions(filters)` 会对完整的逻辑会话语料库应用 `SessionResultFilter`。消息、工具调用和工具结果、待办事项，以及失败和状态详情会纳入语义文本；推理（reasoning）块、被阻止的提示词、结构事件和流分片则不会。

## 全文搜索结果页

整合后的 `ctx.sessionQuery` seam 提供两个全文搜索范围。`searchSessions()` 按匹配度最强的事件对语料库分组；`searchEvents()` 搜索单个会话。请求将不透明游标与规范化后的查询、元数据过滤器和结果数量上限绑定。提供方的元数据过滤器有意不包含事件文本扫描。

```ts type-equiv
/** Provider-owned opaque continuation token returned by session search. */
type SessionSearchCursor = Branded<'SessionSearchCursor'>
```

```ts type-equiv
/** Cross-session full-text search request. */
interface SessionSearchRequest {
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
```

```ts type-equiv
/** Within-session full-text search request. */
interface SessionEventSearchRequest {
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
```

```ts type-equiv
/** One cursor-paginated result page. */
interface SessionSearchPage<T> {
  /** Results for this page in contract-defined order. */
  items: readonly T[]
  /** Opaque continuation cursor, absent on the final page. */
  nextCursor?: SessionSearchCursor
}
```

与跨会话分组 hit 不同，会话内搜索结果即使没有命中项，也必须公开搜索时观测到的目标 header。

```ts type-equiv
/** Event-search results bound to the indexed target-session observation. */
interface SessionEventSearchPage extends SessionSearchPage<SessionEventSearchHit> {
  /** Cloned target header from the same indexed generation as `items`. */
  session: SessionHeader
}
```

```ts type-equiv
/** One event full-text search hit with a bounded plain-text excerpt. */
interface SessionEventSearchHit extends SessionEventRecord {
  /** Plain text excerpt selected around the match. */
  snippet: string
}
```

```ts type-equiv
/** One grouped cross-session hit, ranked by its strongest matching event. */
interface SessionSearchHit extends SessionRecord {
  /** Strongest matching event for this session. */
  bestMatch: SessionEventSearchHit
}
```

## 错误

封闭的 code 联合类型区分请求校验、目标缺失、surface 日志格式错误、可选后端故障、部署关闭搜索与矛盾的源元数据。

```ts type-equiv
/** Stable machine-routable failure taxonomy for session reads and search. */
type SessionQueryErrorCode =
  | 'SESSION_QUERY_ABORTED'
  | 'SESSION_QUERY_CORRUPT_SESSION'
  | 'SESSION_QUERY_INDEX_FAILED'
  | 'SESSION_QUERY_INVALID_CONFIG'
  | 'SESSION_QUERY_INVALID_CURSOR'
  | 'SESSION_QUERY_INVALID_FILTER'
  | 'SESSION_QUERY_INVALID_LIMIT'
  | 'SESSION_QUERY_INVALID_QUERY'
  | 'SESSION_QUERY_INVALID_SURFACE'
  | 'SESSION_QUERY_PERSISTENCE_FAILED'
  | 'SESSION_QUERY_SEARCH_DISABLED'
  | 'SESSION_QUERY_SESSION_NOT_FOUND'
  | 'SESSION_QUERY_STALE_CURSOR'
  | 'SESSION_QUERY_SOURCE_CONFLICT'
```

<!-- BEGIN GENERATED cordis-surface (gen-cordis-catalog.ts) — do not edit between markers -->

<a id="cordis-surface"></a>

## Cordis API

Generated from source by `scripts/gen-cordis-catalog.ts` (verified fresh by `pnpm run verify-cordis-catalog` in doc-sync; regenerate with `pnpm run gen-cordis-catalog`) — the language sides differ only in locale-specific paired document paths. Signature blocks use a `ts cordis-catalog` fence and keep the original source JSDoc; dispatch modes are defined in the [primer](../cordis-primer.zh.md#dispatch-modes), and the framework-inherited `ctx` API lives in [cordis-api/inherited.md](../cordis-api/inherited.md).

<a id="ctxsessionquery--sessionqueryengine-abstract-seam"></a>

### `ctx.sessionQuery` — `SessionQueryEngine` (abstract seam)

Unified live-preferred session query service.

Exact reads and filters are backend-independent concrete behavior. A backend implements full-text observation, reconciliation, ranking, cursor generations, and query execution on the same `ctx.sessionQuery` service.

```ts cordis-catalog
/**
 * Observe one exact live or prepared Session without a persistence listing preflight.
 * @param sessionId - logical Session identity.
 * @param options - cancellation and projection selection for this read.
 * @returns a caller-owned observation lease.
 */
observeSession( sessionId: SessionId, options: SessionObservationOptions = {}, ): Promise<SessionObservation>

/**
 * Search the live-preferred logical corpus and group by session.
 * @param request - query text, metadata filters, page size, and cursor.
 * @param exec - optional cancellation control.
 * @returns session hits ranked by their strongest matching event.
 */
abstract searchSessions( request: SessionSearchRequest, exec?: SessionSearchExecContext, ): Promise<SessionSearchPage<SessionSearchHit>>

/**
 * Search events within one live-preferred logical session.
 * @param request - target session, query text, filters, page size, and cursor.
 * @param exec - optional cancellation control.
 * @returns matching event hits and their target header from one indexed generation.
 */
abstract searchEvents( request: SessionEventSearchRequest, exec?: SessionSearchExecContext, ): Promise<SessionEventSearchPage>

/**
 * List the complete logical corpus using live-preferred records.
 * @param signal - optional cancellation for persistence listing.
 * @returns deterministic newest-first cloned session records.
 */
listSessions(signal?: AbortSignal): Promise<SessionRecord[]>

/**
 * Filter the complete logical corpus with provider-independent predicates.
 * @param filters - ANDed session metadata and availability clauses.
 * @param signal - optional cancellation for persistence listing.
 * @returns matching cloned records in deterministic newest-first order.
 */
async filterSessions( filters: readonly SessionResultFilter[], signal?: AbortSignal, ): Promise<SessionRecord[]>

/**
 * Fold titles for unique sessions from one cancellable corpus observation.
 *
 * Results preserve first-occurrence input order. Operational failures stay
 * isolated per session, while cancellation rejects the complete operation.
 * @param sessionIds - live or persisted session ids to observe.
 * @param signal - optional cancellation shared by all source reads.
 * @returns one fulfilled or rejected result per unique requested id.
 */
async readTitleSnapshots( sessionIds: readonly SessionId[], signal?: AbortSignal, ): Promise<SessionTitleObservationResult[]>

/**
 * Read one session's complete current model surface from one corpus observation.
 * @param sessionId - live-preferred session id to read.
 * @returns cloned header, current surface, and the last sequence number included in the raw-log capture.
 * @throws when source resolution fails or the session surface is invalid.
 */
async readSurface(sessionId: SessionId): Promise<SessionSurfaceSnapshot>
```

Types: [SessionId](core.zh.md)

Source: [`packages/session-query/session-query/src/index.ts`](../../packages/session-query/session-query/src/index.ts)
<!-- END GENERATED cordis-surface -->
