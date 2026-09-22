/**
 * Terminal presentation for the dsh TUI. Everything exported here is pure:
 * props in, rows or strings out, with no Cordis, no Node built-ins, and no
 * clock. That purity is what lets the component harness run under Bun with no
 * harness runtime, and it is a constraint, not an accident.
 *
 * @module @dsh-tui/ui
 */

export { App, RowView } from './app.tsx'
export type { AppProps, PendingInput } from './app.tsx'
export { project, projector } from './project.ts'
export type { Projection, Projector } from './project.ts'
export { ToolCards } from './cards.ts'
export type { Card, ToolLookup, ToolPresenters } from './cards.ts'
export { formatRow } from './plain.ts'
export { hasText } from './rows.ts'
export type { CardEmphasis, CardLine, NoticeTone, Row } from './rows.ts'
export { appendTranscript, emptyTranscript, transcriptRows } from './transcript.ts'
export type { Transcript } from './transcript.ts'
export type { Completion, CompletionCatalog, FileCatalog } from './completion.ts'
