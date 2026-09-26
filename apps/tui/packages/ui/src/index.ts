/**
 * Terminal presentation for the dsh TUI. Everything exported here is pure.
 * props in, rows or strings out, with no Cordis, no Node built-ins, and no
 * clock — time reaches the turn header as an optional `clock` prop the
 * terminal owner supplies. That purity is what lets the component harness run under Bun with no
 * harness runtime, and it is a constraint, not an accident.
 *
 * @module @dsh-tui/ui
 */

export { App, goalState, RowView } from './app.tsx'
export type { AppProps, GoalEntry, PendingInput } from './app.tsx'
export { Welcome, WELCOME_WIDTH } from './welcome.tsx'
export { announcedCalls, project, projector } from './project.ts'
export { Actions, SETTLES } from './actions.ts'
export type { Projection, Projector } from './project.ts'
export { PALETTE } from './palette.ts'
export type { PaletteColor } from './palette.ts'
export { ToolCards } from './cards.ts'
export type { Card, ToolLookup, ToolPresenters } from './cards.ts'
export { formatRow } from './plain.ts'
export { finishedMarkdown } from './markdown.ts'
export { callsOf, hasText } from './rows.ts'
export type { CardEmphasis, CardLine, NoticeTone, Row, ToolCallRow, ToolOutcome } from './rows.ts'
export { appendTranscript, emptyTranscript, transcriptRows } from './transcript.ts'
export type { Transcript } from './transcript.ts'
export type { Completion, CompletionCatalog, FileCatalog } from './completion.ts'
export { commandUsage, requiresInput, suggestCommand } from './completion.ts'
export type { CodeToken, Highlight } from './present.ts'
