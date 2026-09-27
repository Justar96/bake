# Deliverables

English | [中文](deliverables.zh.md)

The `present` tool lets an agent declare files for delivery and writes the declaration as a log-only Session event. The [group map](../../packages/deliverables/README.md) and [tool-present README](../../packages/deliverables/tool-present/README.md) own current configuration and behavior.

Source: [`packages/deliverables/tool-present/src/types.ts`](../../packages/deliverables/tool-present/src/types.ts)

## `PresentedFile` — one declared delivery

```ts type-equiv
/** A declared filesystem file whose current contents remain at its source path. */
interface PresentedFile {
  /** Original absolute path or path relative to the Session working directory. */
  path: string
  /** Optional description supplied by the model. */
  description?: string
}
```

## Durable delivery event

`tool-present` merges `deliverables/presented: { turn; callId; files: PresentedFile[] }` into `SessionEventMap` and appends it after a successful `present` call. The declaration does not enter model context.
