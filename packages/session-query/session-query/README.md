---
description: "The unified session-history query service for consumers and backend authors: exact reads and provider-independent filters over live and durable session logs."
kind: "package-reference"
---

# bake-session-query

## Summary

`bake-session-query` lets application code list, filter, read, and search session history. Reads prefer live sessions over persisted copies and return detached clones from one consistent observation. Exact reads and filters work with any supported storage setup; ranked full-text search requires a backend such as `dsh-session-query-sqlite`. Use it when application code needs programmatic access to the history presented to the model.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Use `ctx.sessionQuery` from application code when you need to read or search session history without touching the session service or a storage backend directly. The service is provided by a concrete backend plugin — the shipped composition mounts `@deepseek-ai/dsh-session-query-sqlite` ([README](../session-query-sqlite/README.md)) — so this package is never mounted alone. Everything below is available on `ctx.sessionQuery` once a backend is composed.

### What you can do

| Operation | What you get |
|---|---|
| `listSessions()` | Every logical session, newest first, with `live` and `persisted` availability flags |
| `filterSessions(filters)` | Sessions matching ANDed metadata and availability predicates |
| `readTitleSnapshots(ids)` | The latest folded title per session, bound to its source header, with the newest event's time as `lastEventAt` and `startedTurn: true` when the log started a turn |
| `readSurface(id)` | The complete current model surface |
| `searchSessions(request)` / `searchEvents(request)` | Full-text search pages, implemented by the mounted backend |

Body-free records expose only `SessionHeader.isSeeded`. Reads that return event bodies (`readSurface`) and retained `SessionObservation` values also carry the exact `inheritedEventCount`, so callers can distinguish inherited and owned events without inferring a cut from the log.

### Filters

`SessionResultFilter` narrows sessions by id, nullable cwd, created-at range, nullable parent, or source availability — both for `filterSessions` and as search session filters. Full-text search additionally accepts event predicates as `SessionEventMetadataFilter`: seq/time range, event type, and surface. Filter arrays are ANDed and list values within one clause are ORed; empty list values match nothing, ranges are inclusive, and malformed ranges or unknown closed-union values fail with `SESSION_QUERY_INVALID_FILTER`.

### Configuration

The inherited knobs are set through the mounted backend's config:

| Field | Default | Meaning |
|---|---|---|
| `readWindowMax` | `50` | Maximum accepted raw event window on either side |
| `persistedReadConcurrency` | `4` | Concurrent persisted-log reads in one batch title read |
| `preparedSessionCacheSize` | `5` | Cold prepared-Session observations retained for reuse across `observeSession` reads |

### Failures and recovery

Failures are typed with a stable `SessionQueryError.code`. The ones you will meet: `SESSION_QUERY_SESSION_NOT_FOUND` when an id is absent; `SESSION_QUERY_SOURCE_CONFLICT` when live and persisted observations of one session disagree on immutable headers; `SESSION_QUERY_PERSISTENCE_FAILED` when mounted persistence is unreadable; `SESSION_QUERY_CORRUPT_SESSION` when a durable record fails Session validation; and `SESSION_QUERY_INVALID_SURFACE` when a loaded log breaks the surface contract. Reads targeting a known live session never consult persistence, so a failing backend cannot make current in-memory history unreadable.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains the design decisions behind the service and points at the code that realizes them; the observable behavior is fully covered in [Use this package](#use-this-package).

### Design philosophy

The service is built on one separation and three commitments:

- **Live-preferred logical corpus.** Every read resolves one consistent observation: live `ctx.sessions` wins, optional `ctx.sessionPersistence` fills the rest, and conflicting immutable headers fail rather than merge.
- **Detached results.** All returned headers, events, and records are cloned; nothing exposes live state or a retained subscription.
- **Exact reads concrete, search abstract.** Reads and filters are implemented here once; the two full-text methods are the only abstract surface a backend owns.
- **One canonical surface fold.** `readSurface` validates the whole log with the same `dsh-session` fold, so search and surface reads agree with model-history derivation.

The decision history lives in the [unified service decision](../../../.agents/notes/archived/architecture/2026-07-23-unified-session-query-service.md), the [tracing note](../../../.agents/notes/archived/feature/2026-07-13-session-query-tracing.md), and the [SQLite provider note](../../../.agents/notes/archived/feature/2026-07-10-sqlite-session-query-provider.md).

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Service definition: the abstract `SessionQueryEngine`, concrete reads, config validation |
| [`src/corpus.ts`](src/corpus.ts) | Live-preferred corpus resolution, optional persistence binding, batch projections |
| [`src/observation.ts`](src/observation.ts) | Live-preferred point observations with a bounded revision-keyed prepared-Session cache |
| [`src/cold-read.ts`](src/cold-read.ts) | Handle-based cold log read with in-memory interrupted-turn closers |
| [`src/types.ts`](src/types.ts) | Public records, filters, requests, and page types |
| [`src/config.ts`](src/config.ts) | Inherited config and the closed `SessionQueryError` taxonomy |
| [`src/filters.ts`](src/filters.ts) | Provider-independent predicates and the literal text scan |
| [`src/extraction.ts`](src/extraction.ts) | First-party semantic text extraction per event type |
| [`src/documents.ts`](src/documents.ts) | Surface-aware semantic document projection |
| [`src/tracing.ts`](src/tracing.ts) | Canonical current-surface fold for exact surface reads |
| [`src/sources.ts`](src/sources.ts) | Immutable-header compatibility check |
| — | No runtime invariant companion is published; query results are immutable per-call projections validated while they are built; the service retains no observable result state. |

### Corpus resolution

`SessionCorpus` binds optional `ctx.sessionPersistence` through a fiber and resolves each read live-first: a known live target is snapshotted without consulting persistence; otherwise the session is listed, read completely through a short-lived read handle, and re-checked for a live attachment before cloning. A cold log whose writer crashed mid-turn is balanced in memory with `interruptedTurnClosers` — persistence is never mutated by a read. Header compatibility is asserted between listed and loaded observations. Batch title reads run one metadata listing and bounded-concurrency reads, isolating per-session failures while cancellation rejects the whole batch.

### Observation cache

`observeSession` builds point observations without a listing preflight. A live observation fixes its cut as the current log length and materializes `events` on first read, so header-, cursor-, or projection-only consumers never copy the log; the log only appends, so a late first read still yields exactly that prefix. The cold path stats the stored session first and consults its own bounded cache keyed by a stable persistence-service identity and the `stat` revision, rather than a Context proxy reference: an unchanged revision reuses the restored unpublished Session without re-reading the log; a changed revision, or a replaced persistence instance, reloads through the handle seam and replaces the entry. The cache holds `preparedSessionCacheSize` entries with least-recently-used eviction, entries pinned by active observation leases are never evicted, and a session that goes live mid-read retries the live path.

### Reads

`readSurface` runs one `foldSurface` pass that classifies events as `current`, `shadowed`, or `log-only` and validates zero-based contiguous seqs, surface-marker eligibility, and replacement or citation integrity; any violation fails with `SESSION_QUERY_INVALID_SURFACE`.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough. They move from the shared query vocabulary to the concrete backend and the decision evidence.

- [Session Query subsystem reference](../../../docs/subsystems/session-query.md) — the full type-level contract: records, filters, search pages, and errors.
- [dsh-session-query-sqlite](../session-query-sqlite/README.md) — the shipped full-text backend and its index lifecycle.
- [Session query relationship tracing](../../../.agents/notes/archived/feature/2026-07-13-session-query-tracing.md) — trace semantics and the validation boundary.
- [SQLite FTS5 session search](../../../.agents/notes/archived/feature/2026-07-10-sqlite-session-query-provider.md) — how the search surface is implemented and reconciled.

-----

<a id="model-experience"></a>
## Model Experience

None, as the trusted query service exposes cloned records only to callers and registers nothing model-facing.

#### KV Cache effect

None; this package neither assembles nor sends a provider request.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define when this package is a poor fit or needs special operational care. They are current package constraints, not a task backlog.

- **No caller authorization** — this is trusted context-wide infrastructure; a model tool or UI must constrain which sessions its caller may inspect.
- **No provider coordinator or fallback** — the service is abstract over search, so a composition must mount a concrete backend; there is no search-provider registry or fallback implementation.
- **Exact reads replay whole logs** — `readSurface` and batch title reads load and validate the complete logical log, so very large histories pay full inspection per call; `listSessions` stays lightweight.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is working context for maintainers: open design questions and directions that are not decided. It is explicitly non-authoritative — shipped behavior, limits, and accepted rationale live in the sections above, the package code, and the linked Agent Notes.

#### Future: extractor and search-provider registries

Extractor and search-provider registries, and additional model-facing surfaces are deferred.

</details>
