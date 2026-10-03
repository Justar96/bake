# Session Feedback

[`@deepseek-ai/dsh-command-feedback`](../../packages/feedback/command-feedback) owns `/feedback`, the immutable Session-level remark it records as `feedback/record`, and the `FeedbackCategory` taxonomy a remark is filed under. The event is log-only and never enters model context.

## Public types

Source: [`packages/feedback/command-feedback/src/types.ts`](../../packages/feedback/command-feedback/src/types.ts)

```ts type-equiv
/** One of the fixed feedback categories; the ids are durable log vocabulary. */
type FeedbackCategory =
  | 'task-result'
  | 'instruction-following'
  | 'product-interaction'
  | 'service-stability'
  | 'resource-cost'
  | 'security-privacy-permission'
  | 'other'
```

```ts type-equiv
/**
 * One recorded human remark about a Session. Both members are optional: a
 * submission with neither still records that the human asked for the
 * Session to be reviewed, which is what authorizes log delivery.
 */
interface FeedbackRecord {
  /** Free-text remark with surrounding whitespace removed; never empty when present. */
  readonly text?: string
  /** Category the human filed the remark under. */
  readonly category?: FeedbackCategory
}
```

```ts type-equiv
/** Record one Session-level remark through the Host Remote. */
interface SessionFeedbackRecordRequest {
  /** Live Session the remark describes. */
  readonly sessionId: SessionId
  /** Free-text remark; blank text is recorded as absent. */
  readonly text?: string
  /** Category the human filed the remark under. */
  readonly category?: FeedbackCategory
}
```

```ts type-equiv
/** Stable postcondition of a recorded remark. */
interface SessionFeedbackRecordValue {
  /** The remark is appended to the Session log; flushing follows the Session's own schedule. */
  readonly recorded: true
}
```

```ts type-equiv
/** No live Session carries the requested id. */
interface SessionFeedbackSessionNotFound {
  readonly code: 'session-not-found'
  readonly sessionId: SessionId
}
```

```ts type-equiv
/** Result returned by the `sessionFeedback.record` operation. */
type SessionFeedbackRecordResult =
  | { readonly ok: true; readonly value: SessionFeedbackRecordValue }
  | { readonly ok: false; readonly error: SessionFeedbackSessionNotFound }
```

## Persistence and Remote contract

A remark is appended to the live Session's log and flushed on the Session's own schedule. The package publishes the Host `sessionFeedback.record` unary Remote contract through `TypertRemoteService` and `@Remote`; the generated Cordis API below is the method-level authority.

By default, feedback stays in the local session log. Recording feedback does not trigger an LLM request. The [OTel backend](../../packages/session/session-telemetry-otel/README.md) has no default endpoint; once `DSH_TELEMETRY_OTLP_URL` names a collector, it releases the canonical prefix through recorded feedback to that collector, for every provider. The command acknowledgement confirms recording, identifies the Session and anonymous user, and states whether the telemetry backend uploads the session history or the feedback stays local; it does not report delivery.

Released Session logs can also carry per-message `feedback/message-put` and `feedback/message-delete` records. No plugin writes them now; they stay readable through the [retired vocabulary](../../packages/session/session-format-catalog/src/retired-vocabulary.ts).

## Boundaries and limitations

- A Session remark has no size bound.
- `sessionFeedback.record` serves live Sessions only and answers `session-not-found` otherwise.

<!-- BEGIN GENERATED cordis-surface (gen-cordis-catalog.ts) — do not edit between markers -->

<a id="cordis-surface"></a>

## Cordis API

Generated from source by `scripts/gen-cordis-catalog.ts` (verified fresh by `pnpm run verify-cordis-catalog` in doc-sync; regenerate with `pnpm run gen-cordis-catalog`) — the language sides differ only in locale-specific paired document paths. Signature blocks use a `ts cordis-catalog` fence and keep the original source JSDoc; dispatch modes are defined in the [primer](../cordis-primer.md#dispatch-modes), and the framework-inherited `ctx` API lives in [cordis-api/inherited.md](../cordis-api/inherited.md).

<a id="ctxsessionfeedback--sessionfeedbackservice"></a>

### `ctx.sessionFeedback` — `SessionFeedbackService`

Host Remote through which a product surface records a Session-level remark.

```ts cordis-catalog
/**
 * Record one remark on a live Session.
 * @param request - target Session plus the optional text and category.
 * @returns the recorded postcondition, or `session-not-found` when no live
 * Session carries the id.
 */
@Remote('record') record(request: SessionFeedbackRecordRequest): Promise<SessionFeedbackRecordResult>
```

Source: [`packages/feedback/command-feedback/src/index.ts`](../../packages/feedback/command-feedback/src/index.ts)
<!-- END GENERATED cordis-surface -->
