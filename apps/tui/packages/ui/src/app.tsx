/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { lazy, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, Text, useApp, useInput, useIsScreenReaderEnabled, usePaste, useWindowSize } from 'ink'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import type { ContextUsage, TokenTotals } from './format.ts'
import { isNewline, useComposer, type Submit } from './composer.ts'
import { draftRows } from './editor.ts'
import { argumentQuery, commandUsage, completionMenu, requiresInput, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, selectionWindow, type Budget, type FrameStyle, type WindowSize } from './layout.ts'
import { present, type Highlight, type ResultBound } from './present.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { InspectionBar, SubagentRow, subagentLine, subagentSheet, subagentTab, type SubagentEntry, type WorkflowEntry } from './subagents.tsx'
import { goalSheet, goalState, type GoalEntry } from './goal.ts'
import type { GitState } from './git.ts'
import { Sheet, sheetPage, sheetRows, type SheetFollow, type SheetLine, type SheetTab } from './sheet.tsx'
import { Tasks, taskSheet, taskTab, tasksOpen, type TaskEntry } from './tasks.tsx'
import { Beat } from './beat.tsx'
import { statusFields } from './status-line.ts'
import { Installing } from './installing.tsx'
import type { InstallStep } from './install-progress.ts'
import { Scrollback, type Opening } from './scrollback.tsx'
import type { Fullscreen as FullscreenComponent, TranscriptScroll } from './fullscreen.tsx'
import { Chrome, Completion, composerHint, draftWidth, Line, LiveRegion, Notice, Panel, wrappedRows, type ActivityState } from './line.tsx'
import { activityWord, phaseLabel, phaseOf, lastTurn, turnSummary, type Clock } from './activity.ts'

const Fullscreen = lazy(async () => ({ default: (await import('./fullscreen.tsx')).Fullscreen as typeof FullscreenComponent }))
const WHEEL_ROWS = 3

/** Display-only projection of one pending inbox message. */
export interface PendingInput {
  readonly id: string
  readonly target: 'next-step' | 'next-turn'
  readonly text: string
  readonly attachments?: readonly AttachmentSummary[]
}

export type { TaskEntry } from './tasks.tsx'

export { goalState, type GoalEntry } from './goal.ts'

/** Application state and actions supplied by the terminal owner. */
export interface AppProps {
  /** Inline scrollback by default; fullscreen owns a scrollable transcript. */
  readonly screen?: 'inline' | 'fullscreen'
  /** Suppress composer edits while the application prepares a session handoff. */
  readonly inputBlocked?: boolean
  readonly attachments?: readonly AttachmentSummary[]
  readonly committed: Transcript
  readonly live: readonly Row[]
  readonly pending: readonly PendingInput[]
  readonly status: AgentStatus
  readonly stopping: boolean
  readonly command: string | undefined
  /**
   * Manual compaction progress from the matching command and session lifecycle
   * events. The header shows it; Enter queues a prompt behind it.
   */
  readonly compactPhase?: 'preparing' | 'summarizing' | 'saving'
  /**
   * Whether the running turn is compacting its context automatically, from
   * the live `compaction/start` to its end. The header shows it as it shows
   * `/compact`; Enter still steers the turn.
   */
  readonly autoCompacting?: boolean
  readonly notice: string | undefined
  /**
   * Whether an interrupt is armed, so a second one quits.
   *
   * Separate from `notice`. This is key state, not feedback about the
   * session. It stays beside the composer. A command result must not clear
   * it, and it must not clear a command result.
   */
  readonly quitting: boolean
  readonly interaction: Interaction | undefined
  /** The agent's current task list, absent until it writes one. */
  readonly todos: readonly TaskEntry[] | undefined
  /** Child identities and activity from the Harness catalog, including saved history. */
  readonly subagents?: readonly SubagentEntry[]
  /** Recorded workflows, including progress before their first child is published. */
  readonly workflows?: readonly WorkflowEntry[]
  /** Read-only child session; the parent composer stays mounted while it is open. */
  readonly inspection?: {
    readonly sessionId: string
    readonly label: string
    readonly committed: Transcript
    readonly live: readonly Row[]
    readonly status: AgentStatus
    readonly model: string
    readonly context?: ContextUsage | undefined
    readonly usage?: TokenTotals
    readonly permission?: string
    readonly thinkingLevel?: string
  } | undefined
  readonly inspectionParent?: string
  /** The open child's name, for the bar that says where the user is and how to leave. */
  readonly inspectionLabel?: string
  /** Open a child's session read-only, as `/agents <id>` does. */
  readonly onInspectSubagent?: (id: string) => void
  /**
   * A newer Bake release for this install. `installed` means `current` already
   * names it, so a restart runs it; otherwise `/update` installs it. Named in
   * the status line, the bounded field that yields first. Absent, nothing is said.
   */
  readonly update?: { readonly version: string; readonly installed: boolean }
  /**
   * The install `/update` is running: what it is doing, and how far from 0
   * to 1 when that is known. Drawn as one live row above the composer;
   * absent, no row.
   */
  readonly installing?: InstallStep
  /** Shift-Tab: step the selected model's reasoning effort. Absent, Shift-Tab does nothing. */
  readonly onCycleThinking?: () => void
  /** Harness plan projection; absent when this profile has no plan mode. */
  readonly plan?: { readonly active: boolean; readonly pending: boolean }
  /** Current goal from the Harness goal service; absent without one or before `/goal` sets it. */
  readonly goal?: GoalEntry | undefined
  /**
   * Also name the goal's objective on the header, cut to fit. Off by default:
   * the header carries the goal's state, and Ctrl+O opens the whole objective.
   */
  readonly goalObjective?: boolean
  /** Effective permission preset from the session projection; absent without that service. */
  readonly permission?: string
  /** Selected reasoning effort, or the model's advertised default when known. */
  readonly thinkingLevel?: string
  /** `provider/model` of the selected model; absent until one is selected. */
  readonly model?: string | undefined
  /**
   * The working directory as the status line reads it, already shortened
   * against home by the application: this layer reads no environment.
   */
  readonly cwd: string
  /**
   * The workspace's branch and uncommitted changes, read by the application.
   * Absent outside a git repository or before the first read: no field.
   */
  readonly git?: GitState | undefined
  readonly sessionId: string
  /**
   * Running Bake version. When supplied, a session that mounts with no history
   * opens with the welcome block, which carries its own session line. When
   * absent, it opens with the session heading alone.
   */
  readonly version?: string | undefined
  /** Projected context occupancy from the harness meter, absent before a usage sample. */
  readonly context: ContextUsage | undefined
  /** Provider-reported token totals for the session, absent before a request reports any. */
  readonly usage?: TokenTotals
  readonly copy: TuiCopy
  /** Border style this terminal can draw for the welcome card, resolved by the application. */
  readonly frame: FrameStyle
  readonly completion: CompletionCatalog
  readonly files: FileCatalog
  readonly onReferenceQuery: (query: string | undefined) => void
  /** Observe the first argument of a command advertising choices; undefined closes its menu session. */
  readonly onArgumentQuery?: (query: { name: string; partial: string } | undefined) => void
  /** Maximum visible completion and picker rows, supplied by application configuration. */
  readonly completionLimit: number
  /**
   * Tool-result lines the transcript keeps under each outcome. The rest are
   * counted. The full text is in the session log. An unbounded listing
   * scrolls the answer out of view on every call it makes. Supplied by
   * application configuration.
   */
  readonly resultLines: number
  /**
   * Syntax colour for the code in an edit's diff. Absent, or before it knows
   * a file's language, a change draws in its sides' tones alone.
   */
  readonly highlight?: Highlight
  /**
   * Clock for the header spinner and elapsed time.
   *
   * When absent, the header draws a resting glyph and no elapsed time. This
   * layer must not read the wall clock itself, so a test or a static preview
   * renders the same frame on every run. Ignored while a screen reader is
   * active, because each frame would be announced.
   */
  readonly clock?: Clock
  /**
   * Whether the header glyph cycles and a running action's marker blinks.
   * Defaults to true.
   *
   * Off under `NO_COLOR`. Elapsed seconds still advance, but nothing else on
   * the surface moves on its own.
   */
  readonly motion?: boolean
  readonly onSubmit: Submit
  readonly onCancel: () => void
  readonly onInterrupt: () => void
  /**
   * Alt-Up: send the steering now, interrupting the turn it waits on, instead
   * of at the next step. A draft typed during a turn is submitted first. The
   * steering placeholder and the pending panel name the key.
   */
  readonly onSendPending?: () => void
  /**
   * Called for any key other than Ctrl-C while `quitting`. The user went on
   * with something else, so the quit prompt goes instead of waiting out
   * its window. A later Ctrl-C starts over instead of quitting.
   */
  readonly onQuitDismiss?: () => void
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}

/**
 * One dim line under the completion panel. What it is doing, or why it is empty.
 * @param props.text - locale-owned message.
 * @param props.tone - `error` to colour a failure.
 * @returns the message row, aligned with the panel's names.
 */
function Status({ text, tone }: { readonly text: string, readonly tone?: 'error' }): React.ReactElement {
  return <Box flexDirection="row">
    <Box width={2} flexShrink={0}><Text> </Text></Box>
    <Text wrap="truncate-end" dimColor={tone === undefined} {...tone === 'error' ? { color: PALETTE.failed } : {}}>{text}</Text>
  </Box>
}

/**
 * Render one complete committed or transient row without silently truncating results.
 * @param props - the row to render.
 * @returns the row element.
 */
export function RowView({ row, budget, result }: {
  readonly row: Row
  readonly budget: Budget
  readonly result: ResultBound
}): React.ReactElement {
  return <>{present(row, result, line => wrappedRows(line, budget), budget.measure).map((line, index) => <Line key={index} line={line} budget={budget} />)}</>
}

/**
 * Whether the terminal has just become narrower or taller than the last painted frame.
 *
 * Ink erases the previous frame by counting its lines. A terminal that
 * reflows on resize has already wrapped each full-width row of that frame —
 * the composer surface's rows — onto two, so a narrowing leaves those rows
 * on screen. A terminal that grows taller adds rows under the frame, unless
 * it pulls history down from scrollback, and leaves the composer off the
 * bottom row. Ink clears the terminal and replays history only for a frame
 * that overflows the viewport. The caller overflows while this returns true.
 * The next frame no longer overflows, and it is cleared and replayed too.
 *
 * Ink throttles its writes, so a commit that is replaced within one throttle
 * window is never written. The overflow is held until Ink has flushed it:
 * dropped in the same flush, it would coalesce away and nothing would be
 * cleared.
 *
 * Child inspection changes the mounted transcript and frame together. Replay
 * also reanchors that transition at the terminal bottom.
 *
 * Closing a sheet does too. Opening one grows the frame and scrolls the
 * history above it into the terminal's scrollback, which cannot be drawn back
 * down; held, the frame would leave the sheet's rows as a blank gap over the
 * composer until enough new history printed to fill it. Replay brings the
 * history back down against the controls instead.
 * @param size - current terminal size.
 * @param view - displayed parent or child identity.
 * @param openingChild - whether the first frame must reanchor after a parent.
 * @param sheetOpen - whether a sheet is open over the controls.
 * @param enabled - whether the renderer uses inline scrollback.
 * @returns true from a resize, inspection, or sheet-closing transition until
 *   Ink has written the overflowing frame.
 */
function useRepaint(size: WindowSize, view: string, openingChild: boolean, sheetOpen: boolean, enabled: boolean): boolean {
  const { waitUntilRenderFlush } = useApp()
  const painted = useRef(size)
  const paintedView = useRef<string | undefined>(openingChild ? undefined : view)
  const paintedSheet = useRef(sheetOpen)
  const [overflowing, setOverflowing] = useState(false)
  const transition = enabled && (view !== paintedView.current || size.columns < painted.current.columns || size.rows > painted.current.rows
    || (paintedSheet.current && !sheetOpen))
  useLayoutEffect(() => {
    painted.current = size
    paintedView.current = view
    paintedSheet.current = sheetOpen
    if (transition) setOverflowing(true)
  })
  useEffect(() => {
    if (!overflowing) return
    let active = true
    // A failed flush is the renderer's to report; the frame still stops overflowing.
    const settle = (): void => { if (active) setOverflowing(false) }
    void waitUntilRenderFlush().then(settle, settle)
    return () => { active = false }
  }, [overflowing, waitUntilRenderFlush])
  return transition || (enabled && overflowing)
}

/**
 * Render the terminal session. Ink owns key decoding and bracketed-paste mode.
 * @param props - harness state, localized copy, and application callbacks.
 * @returns transcript, active interaction, status, and composer.
 */
export function App(props: AppProps): React.ReactElement {
  return <SessionView key={props.sessionId} {...props} />
}

/** A row around the composer that arrow keys can select. */
type Focus = 'tasks' | 'goal' | 'subagents'

/** What an open sheet shows in full. */
type SheetKind = 'tasks' | 'agents' | 'goal'

/** The order the cycle key visits the sheets in, skipping any with nothing to show. */
const SHEETS: readonly SheetKind[] = ['tasks', 'agents', 'goal']

/** An SGR mouse report: button code, column, row, then `M` for a press or `m` for a release. */
const MOUSE_REPORT = /^\[<(\d+);(\d+);(\d+)([Mm])$/
/** The Shift, Meta, and Ctrl bits a report adds to its button code. */
const MOUSE_MODIFIERS = 4 | 8 | 16
const WHEEL_UP = 64
const WHEEL_DOWN = 65

function SessionView(props: AppProps): React.ReactElement {
  const scroll = useRef<TranscriptScroll>(null)
  const fullscreen = props.screen === 'fullscreen'
  const composer = useComposer(props.onSubmit, () => inputHistory(props.committed, props.pending), (props.attachments?.length ?? 0) > 0)
  const { copy, interaction } = props
  // One arrow-key focus across the rows around the composer: the task row
  // and the goal above it, the subagents field below. Refs, because keys
  // decoded in one read see the focus before React renders it.
  const [focus, setFocus] = useState<Focus | undefined>(undefined)
  const focusRef = useRef<Focus | undefined>(undefined)
  const focusOn = (next: Focus | undefined): void => {
    focusRef.current = next
    setFocus(next)
  }
  const [sheet, setSheet] = useState<SheetKind | undefined>(undefined)
  const sheetRef = useRef<SheetKind | undefined>(undefined)
  const [sheetScroll, setSheetScroll] = useState(0)
  const [sheetFollowing, setSheetFollowing] = useState(true)
  const openSheet = (next: SheetKind | undefined): void => {
    sheetRef.current = next
    setSheet(next)
    setSheetScroll(0)
    setSheetFollowing(true)
  }
  // The child under the agents sheet's pointer. A ref, as focus is.
  const [agentIndex, setAgentIndex] = useState(0)
  const agentRef = useRef(0)
  const pointAt = (index: number): void => {
    agentRef.current = index
    setAgentIndex(index)
    setSheetFollowing(true)
  }
  const tasksShown = tasksOpen(props.todos)
  const hasTasks = (props.todos?.length ?? 0) > 0
  const hasSubagents = (props.subagents?.length ?? 0) > 0 || (props.workflows?.length ?? 0) > 0
  const available = (target: Focus): boolean =>
    target === 'goal' ? props.goal !== undefined : target === 'tasks' ? tasksShown : hasSubagents
  const showable = (kind: SheetKind): boolean =>
    kind === 'goal' ? props.goal !== undefined : kind === 'tasks' ? hasTasks : hasSubagents
  const showing = SHEETS.filter(showable)
  /**
   * The sheet `step` places away from the open one, among those with something
   * to show, wrapping past either end. Tab's step inside a sheet; each sheet's
   * own key opens and closes it.
   */
  const stepSheet = (step: 1 | -1): SheetKind | undefined => {
    const shown = SHEETS.filter(showable)
    if (shown.length === 0) return undefined
    const at = sheetRef.current === undefined ? -1 : shown.indexOf(sheetRef.current)
    if (at < 0) return step === 1 ? shown[0] : shown.at(-1)
    return shown[(at + step + shown.length) % shown.length]
  }
  const showSheet = (next: SheetKind | undefined): void => {
    focusOn(undefined)
    if (next === 'agents') pointAt(Math.max(0, props.subagents?.findIndex(entry => entry.inspectable) ?? 0))
    openSheet(next)
  }
  /** A sheet's own key opens it, or closes it when it is the one open. */
  const toggleSheet = (kind: SheetKind): void => { showSheet(sheetRef.current === kind ? undefined : kind) }
  const inert = interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined
  useEffect(() => {
    if (focusRef.current !== undefined && (inert || !available(focusRef.current))) focusOn(undefined)
  }, [inert, props.goal === undefined, tasksShown, hasSubagents])
  useEffect(() => {
    if (sheetRef.current !== undefined && (inert || !showable(sheetRef.current))) openSheet(undefined)
  }, [inert, props.goal === undefined, hasTasks, hasSubagents])
  // Children come and go while the list is open; the pointer stays on one.
  useEffect(() => {
    const last = Math.max(0, (props.subagents?.length ?? 0) - 1)
    if (agentRef.current > last) pointAt(last)
  }, [props.subagents?.length])
  const [menu, setMenu] = useState({ draft: '', cursor: 0, selected: '', dismissed: false })
  const currentMenu = useRef(menu)
  const updateMenu = (selected: string, dismissed: boolean): void => {
    currentMenu.current = { draft: composer.value, cursor: composer.position, selected, dismissed }
    setMenu(currentMenu.current)
  }
  const samePosition = (draft: string, cursor: number) => currentMenu.current.draft === draft && currentMenu.current.cursor === cursor
  const matchesFor = (draft: string, cursor: number) => interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined || composer.blocked
    || (samePosition(draft, cursor) && currentMenu.current.dismissed)
    ? undefined : completionMenu(props.completion, props.files, draft, cursor)
  const selectedIndex = (matches: readonly CompletionChoice[], draft: string, cursor: number): number =>
    samePosition(draft, cursor) ? Math.max(0, matches.findIndex(item => item.name === currentMenu.current.selected)) : 0
  const visibleMenu = matchesFor(composer.text, composer.cursor)
  const matches = visibleMenu?.entries
  const query = visibleMenu?.query
  useEffect(() => { props.onReferenceQuery(query) }, [props.onReferenceQuery, query])
  const argument = interaction === undefined && props.inputBlocked !== true && props.inspection === undefined && !composer.blocked
    ? argumentQuery(props.completion.entries, composer.text, composer.cursor) : undefined
  useEffect(() => { props.onArgumentQuery?.(argument === undefined ? undefined : { name: argument.name, partial: argument.partial }) },
    [props.onArgumentQuery, argument?.name, argument?.partial])
  const selected = matches === undefined ? 0 : selectedIndex(matches, composer.text, composer.cursor)
  // The right slot's words, which also set how wide the draft wraps.
  const hints = { send: copy.send, interrupt: copy.interrupt, select: copy.tabCompletes, answer: copy.send }
  // The width the composer wraps a draft at, so Up and Down move between the
  // rows it draws. Arrows reach it only with the menu closed.
  const rowWidth = (draft: string): number => draftWidth(size.columns, composerHint(size.columns, {
    running: props.inspectionParent === undefined && props.status === 'running', asking: false, listing: false, drafting: draft !== '',
  }, hints))
  const entryRows = (entry: string): number => draftRows(entry, rowWidth(entry))
  // A wheel scrolls what the arrows would: an open sheet, else the transcript,
  // which it also scrolls under an interaction, since the wheel takes none of
  // an interaction's keys. A click only reaches the jump-to-latest row.
  const pointer = (code: number, row: number, pressed: boolean): void => {
    const button = code & ~MOUSE_MODIFIERS
    const sheetOpen = sheetRef.current !== undefined
    if (button === WHEEL_UP || button === WHEEL_DOWN) {
      const rows = (button === WHEEL_UP ? -1 : 1) * WHEEL_ROWS
      if (sheetOpen) {
        setSheetFollowing(false)
        setSheetScroll(current => Math.max(0, Math.min(sheetMaxScroll, Math.min(current, sheetMaxScroll) + rows)))
      } else if ((!props.inputBlocked || props.inspectionParent !== undefined) && !composer.blocked) scroll.current?.scroll(rows)
    } else if (button === 0 && pressed && !sheetOpen && interaction === undefined) scroll.current?.press(row)
  }
  // Keep bracketed paste enabled for the whole mounted terminal. Modal hooks
  // still receive their own paste events, while this listener ignores them;
  // keeping one owner avoids a mode-off/mode-on gap when fullscreen opens a
  // sheet or an interaction replaces the composer.
  usePaste(text => {
    if (sheetRef.current === undefined && interaction === undefined && props.inputBlocked !== true
      && props.inspection === undefined && !composer.submitting) composer.paste(text)
  })
  useInput((text, key) => {
    if (props.inspection !== undefined) return
    // Fullscreen turns on SGR mouse reports, which Ink passes on as unknown
    // text without their Escape. None may reach the composer.
    const mouse = MOUSE_REPORT.exec(text)
    if (mouse !== null) {
      if (fullscreen) pointer(Number(mouse[1]), Number(mouse[3]) - 1, mouse[4] === 'M')
      return
    }
    if (key.ctrl && text === 'c') { props.onInterrupt(); return }
    if (sheetRef.current !== undefined) {
      if (key.escape) { openSheet(undefined); return }
      // Escape and the next key read together decode as one Meta key. No sheet
      // takes a Meta key, so it closes as the Escape would have, and a printed
      // character goes to the composer, as the rest of the same read does.
      if (key.meta) {
        openSheet(undefined)
        if (!key.ctrl && [...text].length === 1 && !/\p{Cc}/u.test(text)) composer.type(text)
        return
      }
      if (key.ctrl && text === 't' && hasTasks) { toggleSheet('tasks'); return }
      if (key.tab) { showSheet(stepSheet(key.shift ? -1 : 1)); return }
      if (key.ctrl && text === 'o' && props.goal !== undefined) { toggleSheet('goal'); return }
      if (key.ctrl && text === 'g' && hasSubagents) { toggleSheet('agents'); return }
      if (key.pageUp || key.pageDown) {
        setSheetFollowing(false)
        setSheetScroll(current => Math.max(0, Math.min(sheetMaxScroll, Math.min(current, sheetMaxScroll) + (key.pageUp ? -1 : 1) * sheetRowsShown)))
        return
      }
      if (sheetRef.current === 'agents' && (props.subagents?.length ?? 0) > 0) {
        const children = props.subagents ?? []
        if (key.upArrow || (key.ctrl && text === 'p')) pointAt(Math.max(0, agentRef.current - 1))
        if (key.downArrow || (key.ctrl && text === 'n')) pointAt(Math.max(0, Math.min(children.length - 1, agentRef.current + 1)))
        if (key.home) pointAt(0)
        if (key.end) pointAt(Math.max(0, children.length - 1))
        const child = children[agentRef.current]
        if (key.return && child?.inspectable === true) { openSheet(undefined); props.onInspectSubagent?.(child.id) }
        return
      }
      if (key.return) { openSheet(undefined); return }
      if (key.upArrow || (key.ctrl && text === 'p')) { setSheetFollowing(false); setSheetScroll(current => Math.max(0, Math.min(current, sheetMaxScroll) - 1)) }
      if (key.downArrow || (key.ctrl && text === 'n')) { setSheetFollowing(false); setSheetScroll(current => Math.min(sheetMaxScroll, current + 1)) }
      if (key.home) { setSheetFollowing(false); setSheetScroll(0) }
      if (key.end) { setSheetFollowing(false); setSheetScroll(Number.MAX_SAFE_INTEGER) }
      return
    }
    if (props.quitting) props.onQuitDismiss?.()
    const inputMenu = matchesFor(composer.value, composer.position)
    const choices = inputMenu?.entries
    const newline = isNewline(text, key)
    if (key.escape) {
      if (focusRef.current !== undefined) { focusOn(undefined); return }
      if (choices !== undefined) { updateMenu('', true); return }
      props.onCancel(); return
    }
    if (fullscreen && interaction === undefined && (!props.inputBlocked || props.inspectionParent !== undefined)
      && !composer.blocked && (key.pageUp || key.pageDown || (key.ctrl && (key.home || key.end)))) {
      scroll.current?.move(key.pageUp ? 'up' : key.pageDown ? 'down' : key.home ? 'start' : 'end')
      return
    }
    // Alt-Up sends steering now instead of at the next step: a typed draft is
    // submitted and sent, and with an empty draft the queued input is. It is
    // checked before the composer, which takes no other Meta key. A command
    // draft is left alone: it is not steering.
    if (key.meta && key.upArrow && interaction === undefined && props.inputBlocked !== true && props.inspection === undefined
      && !composer.blocked) {
      const draft = composer.value
      if (props.status === 'running' && draft.trim() !== '' && !draft.trimStart().startsWith('/')) {
        composer.type('\n')
        // Accepted at once, the draft is queued steering by now; a refused or
        // still-sending draft stays where it is and is not sent.
        if (!composer.blocked && composer.value === '') props.onSendPending?.()
        return
      }
      if (props.pending.length > 0) { props.onSendPending?.(); return }
    }
    // Alt-Enter is the one Meta key the composer takes: a line break.
    if (interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined || composer.blocked
      || (key.meta && !newline)) return
    if (key.ctrl && text === 'o' && props.goal !== undefined) { toggleSheet('goal'); return }
    if (key.ctrl && text === 't' && hasTasks) { toggleSheet('tasks'); return }
    if (key.ctrl && text === 'g' && hasSubagents) { toggleSheet('agents'); return }
    const focused = focusRef.current
    if (focused !== undefined && available(focused)) {
      if (focused === 'subagents') {
        if (key.upArrow || key.leftArrow) { focusOn(undefined); return }
        if (key.downArrow || key.rightArrow) return
        if (key.return && !newline) { showSheet('agents'); return }
      } else {
        // Above the composer the task row sits over the goal's header.
        if (key.upArrow) { if (focused === 'goal' && tasksShown) focusOn('tasks'); return }
        if (key.downArrow) { focusOn(focused === 'tasks' && props.goal !== undefined ? 'goal' : undefined); return }
        if (key.leftArrow) { focusOn(undefined); return }
        if (key.rightArrow) return
        if (key.return && !newline) { showSheet(focused); return }
      }
      // Any other key belongs to the composer again.
      focusOn(undefined)
    }
    if (key.downArrow && composer.value === '' && choices === undefined && hasSubagents) {
      focusOn('subagents'); return
    }
    // Before the menu and the composer, which both read a plain Tab.
    if (key.tab && key.shift) { props.onCycleThinking?.(); return }
    if (key.ctrl && (text === 'p' || text === 'n')) {
      composer.recall(text === 'p' ? 'older' : 'newer', entryRows); updateMenu('', true); return
    }
    // Before the Ctrl guard, which a CSI-u Ctrl-J would otherwise stop at. A
    // read of line feeds is Ctrl-J pressed, once or more.
    if (newline) { composer.paste(text.startsWith('\n') ? text : '\n'); return }
    if (composer.editKey(text, key)) return
    if (key.ctrl) return
    if (choices !== undefined && (key.upArrow || key.downArrow || key.tab)) {
      if (choices.length === 0) return
      const index = selectedIndex(choices, composer.value, composer.position)
      if (key.tab) composer.replace(choices[index]!.draft, choices[index]!.cursor)
      else updateMenu(choices[(index + (key.downArrow ? 1 : -1) + choices.length) % choices.length]!.name, false)
      return
    }
    if (key.upArrow || key.downArrow) {
      // Inside a draft of several rows the caret moves between the rows it
      // shows. From the first row Up walks back through history, and from the
      // last row Down walks forward; only past history's oldest entry does Up
      // reach the goal on the header, or the task row when there is no goal.
      // Leaving history restores the unsent draft rather than a stale entry
      // one Enter would rerun.
      if (composer.vertical(key.upArrow ? 'up' : 'down', rowWidth(composer.value))) return
      const above = props.goal !== undefined ? 'goal' : tasksShown ? 'tasks' : undefined
      if (!composer.recall(key.upArrow ? 'older' : 'newer', entryRows) && key.upArrow && above !== undefined) {
        composer.leave(); focusOn(above)
      }
      updateMenu('', true); return
    }
    if (key.return && inputMenu?.kind === 'argument') {
      const choice = choices?.[selectedIndex(choices, composer.value, composer.position)]
      if (choice === undefined) { composer.type('\n'); return }
      // A bare command with an optional argument runs as typed; choices are
      // offered, not forced, until the user starts typing one.
      const command = props.completion.entries.find(item => item.kind === 'command' && item.name === argument?.name)
      if (argument?.partial === '' && composer.value.slice(argument.end).trim() === ''
        && command !== undefined && !requiresInput(command)) { composer.type('\n'); return }
      if (choice.argumentRequiresInput !== true && argument?.partial === choice.name
        && composer.value.slice(argument.end).trim() === '') { composer.type('\n'); return }
      composer.replace(choice.draft, choice.cursor)
      return
    }
    if (key.return && inputMenu?.kind === 'slash') {
      const choice = choices?.[selectedIndex(choices, composer.value, composer.position)]
      if (choice === undefined) {
        if (composer.value !== '/') composer.type('\n')
        return
      }
      // A command that cannot run bare is filled in with its separator, and
      // its usage line takes the menu's place until the arguments are typed.
      if (requiresInput(choice)) { composer.replace(choice.draft, choice.cursor); return }
      const exact = composer.value === choice.name
      if (!exact) composer.replace(choice.draft.trimEnd())
      if (choice.kind === 'command' || exact) composer.type('\n')
      return
    }
    const parts = (key.return ? '\n' : text).split('\t')
    for (let index = 0; index < parts.length; index++) {
      if (index > 0) {
        const candidates = matchesFor(composer.value, composer.position)?.entries
        if (candidates === undefined) composer.type('\t')
        else if (candidates.length > 0) {
          const choice = candidates[selectedIndex(candidates, composer.value, composer.position)]!
          composer.replace(choice.draft, choice.cursor)
        }
      }
      composer.type(parts[index]!)
    }
  })
  const size = useWindowSize()
  const budget = useMemo(() => budgetFor(size, { fullscreen }), [size.columns, size.rows, fullscreen])
  const repainting = useRepaint(size, props.inspection?.sessionId ?? props.sessionId, props.inspectionParent !== undefined,
    sheet !== undefined, !fullscreen)
  const screenReader = useIsScreenReaderEnabled()
  const clock = screenReader ? undefined : props.clock
  const running = props.status === 'running'
  // `/compact`, or the running turn compacting its own context. A turn being
  // stopped says so instead.
  const compacting = props.compactPhase
    ?? (running && !props.stopping && props.autoCompacting === true ? 'summarizing' : undefined)
  const compactStarted = useRef<number | undefined>(undefined)
  if (compacting === undefined) compactStarted.current = undefined
  else compactStarted.current ??= clock?.now() ?? 0
  const animate = props.motion === false ? undefined : clock
  // Captured once when the turn starts and held until it ends, so the header
  // word and elapsed clock do not change on every commit inside the turn.
  const turn = useRef<{ readonly start: number, readonly startedAt: number, readonly word: string } | undefined>(undefined)
  // Last turn this surface watched end. Held until the next turn starts, and
  // used as the header text once nothing is running.
  const finished = useRef<{ readonly start: number, readonly elapsed: number | undefined } | undefined>(undefined)
  if (!running) {
    if (turn.current !== undefined) finished.current = {
      start: turn.current.start,
      elapsed: clock === undefined ? undefined : clock.now() - turn.current.startedAt,
    }
    turn.current = undefined
  } else if (turn.current === undefined) {
    finished.current = undefined
    turn.current = {
      start: props.committed.length,
      startedAt: clock?.now() ?? 0,
      word: activityWord(copy, `${props.sessionId}:${props.committed.length}`),
    }
  }
  const ended = finished.current
  // Newest ended turn of a resumed session, read once at mount. No clock
  // watched it run; the log still records how it ended and what it did.
  // Reading history again on every commit is the cost §1 rules out.
  const [replayed] = useState(() => running ? undefined : lastTurn(transcriptRows(props.committed)))
  // A turn this surface watched is read from where it started, and only its
  // newest ended turn when the stretch it watched recorded several.
  const summary = useMemo(() => {
    if (running) return undefined
    if (ended === undefined) return replayed === undefined ? undefined : turnSummary(replayed, copy, undefined)
    const watched = transcriptRows(props.committed, ended.start)
    return turnSummary(lastTurn(watched) ?? watched, copy, ended.elapsed)
  }, [running, ended, replayed, props.committed, copy])
  const lastCommitted = useMemo(
    () => transcriptRows(props.committed, Math.max(0, props.committed.length - 1)).at(-1), [props.committed])
  // Streaming reasoning is drawn whole under the running actions, and may take
  // every row the panels leave; once it commits it prints as its preview.
  const reasoning = running && interaction === undefined && props.live.at(-1)?.kind === 'reasoning'
  // The access boundary is read where the session opens, not on every frame.
  // A session without the welcome block names it on its heading instead.
  const access = props.permission === undefined ? '' : ` · ${copy.permission} ${props.permission}`
  const heading = props.inspectionParent === undefined ? `${copy.session}: ${props.sessionId}${access}`
    : `${copy.subagentParent}: ${props.inspectionParent} > ${props.sessionId}${access}`
  // Memoized so the committed transcript is not re-rendered on every frame.
  const result = useMemo<ResultBound>(
    () => ({ lines: props.resultLines, unit: copy.cardLines, single: copy.cardLine, more: copy.moreLines, failures: copy.summaryFailures, earlier: copy.earlierCalls, ...props.highlight === undefined ? {} : { code: props.highlight } }),
    [props.resultLines, props.highlight, copy])
  // Decided once per mount. A session with no history when it opens gets the
  // block, and keeps it in the stream while its first turn commits.
  const [opening] = useState<Opening | undefined>(() => props.version === undefined
    || props.inspectionParent !== undefined || props.committed.length > 0
    ? undefined
    : { kind: 'welcome', version: props.version, heading: `${copy.session}: ${props.sessionId}`, access: props.permission })
  const menuLimit = Math.min(props.completionLimit, budget.items)
  const liveWant = interaction === undefined ? budget.live : 1
  const controls = interaction === undefined ? budget.chrome.rows + budget.composer - 1 : 0
  let unclaimed = Math.max(0, budget.dynamic - controls)
  const claim = (rows: number): number => {
    const given = Math.max(0, Math.min(rows, unclaimed))
    unclaimed -= given
    return given
  }
  // An interaction replaces the chrome, so the blank that opens the chrome
  // opens the interaction instead and is charged here, not to the interaction.
  const openLimit = claim(interaction !== undefined && budget.chrome.gap ? 1 : 0)
  // A tall picker is a list worth scanning, such as every model: it takes up
  // to half the screen, and never less than a menu would.
  const interactionLimit = claim(interaction === undefined ? 0
    : interaction.kind === 'questions' ? budget.dynamic
      : interaction.kind === 'select' && interaction.tall === true
        ? Math.max(menuLimit, Math.min(budget.items, Math.floor(budget.dynamic / 2)))
        : menuLimit)
  // Under the input, with the chrome; an interaction replaces both. Claimed
  // before anything above the input, so the input's row never depends on
  // what streams over it.
  const subagentLimit = claim((hasSubagents || props.inspectionParent !== undefined) && interaction === undefined ? 1 : 0)
  // An open sheet takes every row the interaction leaves. Below five it
  // replaces the whole region, composer included, so it can still be read.
  const sheetColor = (kind: SheetKind): PaletteColor =>
    kind === 'goal' ? goalState(props.goal, copy)?.color ?? PALETTE.asking
      : kind === 'agents' && (props.subagents?.some(entry => entry.state === 'working') === true
        || props.workflows?.some(run => run.state === 'working') === true) ? PALETTE.running : PALETTE.asking
  const tabs: readonly SheetTab[] = showing.map(kind => ({
    label: kind === 'goal' ? copy.goalTitle : kind === 'tasks' ? taskTab(props.todos ?? [], copy)
      : subagentTab(props.subagents ?? [], copy, props.workflows),
    color: sheetColor(kind), current: kind === sheet,
  }))
  // The footer names what the keys do here. Cycling is named only when there is somewhere to go.
  const sheetKeys = (kind: SheetKind): string => [
    kind === 'agents' && (props.subagents?.length ?? 0) > 0 ? copy.sheetSelect : copy.sheetScroll,
    ...kind === 'agents' && (props.subagents?.length ?? 0) > 0 ? [copy.subagentsOpen] : [],
    ...showing.length > 1 ? [copy.sheetCycle] : [],
    copy.sheetClose,
  ].join(' \u00b7 ')
  const subagentFollow: { readonly follow?: SheetFollow } = sheetFollowing && (props.subagents?.length ?? 0) > 0
    ? { follow: subagentLine(agentIndex, props.workflows?.length) } : {}
  const sheetView: { readonly color: PaletteColor, readonly lines: readonly SheetLine[], readonly keys: string, readonly follow?: SheetFollow } | undefined =
    sheet === undefined || !showable(sheet) ? undefined
      : {
        color: sheetColor(sheet), keys: sheetKeys(sheet),
        ...sheet === 'goal' ? { lines: goalSheet(props.goal!, copy) }
          : sheet === 'tasks' ? { lines: taskSheet(props.todos!, copy) }
          : { lines: subagentSheet(props.subagents ?? [], agentIndex, copy, props.workflows), ...subagentFollow },
      }
  const sheetLimit = claim(sheetView === undefined ? 0 : unclaimed)
  const sheetStandalone = sheetView !== undefined && sheetLimit < 5
  const sheetViewLimit = sheetStandalone ? budget.dynamic : sheetLimit
  const sheetRowsShown = sheetPage(sheetViewLimit, size.columns)
  const sheetMaxScroll = sheetView === undefined ? 0
    : Math.max(0, sheetRows(sheetView.lines, sheetViewLimit, size.columns).length - sheetRowsShown)
  // Claimed before anything but the interaction. It is the answer to a key the
  // user has already pressed, and the next press ends the session.
  const quitLimit = claim(props.quitting ? 1 : 0)
  const sendingLimit = claim(composer.submitting ? 1 : 0)
  const menuStatusRows = matches === undefined ? 0
    : Number(matches.length === 0 && !visibleMenu?.loading)
      + Number(visibleMenu?.loading === true) + Number(visibleMenu?.error !== undefined)
  const completionLimit = claim(matches === undefined ? 0 : Math.max(1,
    Math.min(matches.length, props.completionLimit) + Number(matches.length > props.completionLimit) + menuStatusRows))
  // Usage stays visible while the command has no matching choices.
  const usage = matches !== undefined || interaction !== undefined || props.inputBlocked === true
    || props.inspection !== undefined || composer.blocked ? undefined : commandUsage(props.completion.entries, composer.text)
  const usageLimit = claim(usage === undefined ? 0 : 1)
  // Only while it has rows to draw. An empty live region draws nothing, and
  // reserving its window for a turn that has not spoken yet would starve the
  // panels below it of rows it never uses.
  const liveLimit = claim(!fullscreen && props.live.length > 0 ? liveWant : 0)
  const taskLimit = claim(tasksShown ? 1 : 0)
  const pendingLimit = claim(props.pending.length === 0 ? 0 : menuLimit)
  const attachmentLimit = claim((props.attachments?.length ?? 0) === 0 ? 0 : menuLimit)
  // One row, and the command itself may be long enough to wrap past it.
  // Compaction's progress is the header's to show, in place of the command.
  const commandLimit = claim(props.compactPhase === undefined && props.command !== undefined ? 1 : 0)
  const noticeLimit = claim(props.notice === undefined ? 0 : budget.notice)
  const installingLimit = claim(props.installing === undefined ? 0 : 1)
  // Claimed last, so the panels keep their rows and the thought fills the rest.
  const liveTotal = liveLimit + claim(reasoning && !fullscreen ? unclaimed : 0)
  const menuRows = Math.max(0, completionLimit - menuStatusRows)
  const menuWindow = selectionWindow(matches ?? [], selected, menuRows, props.completionLimit)
  const visibleMatches = menuWindow.shown
  const mixedKinds = new Set(visibleMatches.map(entry => entry.kind)).size > 1
  // Everything between the conversation and the input is one stack, opened by
  // the chrome's blank row. A notice or a list belongs to the input, not to
  // one more line of the answer above it.
  // Header text, in priority order. Compaction while it runs, in its own blue
  // and dough, otherwise the current turn, otherwise how the last turn ended.
  const activity: ActivityState | undefined = compacting !== undefined
    ? {
      kind: 'running', word: copy.compacting, startedAt: compactStarted.current ?? 0, color: PALETTE.compacting, spinner: 'fold',
      phase: { preparing: copy.compactPreparing, summarizing: copy.compactSummarizing, saving: copy.compactSaving }[compacting],
    }
    : turn.current !== undefined
      ? {
        kind: 'running', word: props.stopping ? copy.stopping : turn.current.word, startedAt: turn.current.startedAt,
        phase: props.stopping ? undefined : phaseLabel(phaseOf(props.live, lastCommitted), copy),
        color: props.stopping ? PALETTE.failed : PALETTE.running,
      }
      : summary === undefined ? undefined : { kind: 'ended', summary }
  // One grammar in every mode: the goal keeps its count and names its key
  // whatever else stands around the composer, and gives parts up only as
  // the header's width runs out.
  const goalStanding = goalState(props.goal, copy, { objective: props.goalObjective === true })
  const sheetBlock = sheetView === undefined ? null : <Sheet {...sheetView} tabs={tabs} columns={size.columns}
    limit={sheetViewLimit} offset={sheetScroll} frame={props.frame} />
  const panels = sheetView !== undefined && !sheetStandalone ? sheetBlock : <>
    {props.todos !== undefined && taskLimit > 0 && <Tasks todos={props.todos} copy={copy} columns={size.columns}
      focused={focus === 'tasks'} hint={copy.todoKey} />}
    <Panel
      title={copy.pending} color={PALETTE.waiting} limit={pendingLimit} more={copy.moreLines}
      items={props.pending.map(message => `${message.target === 'next-step' ? copy.nextStep : copy.nextTurn}: ${[message.text, ...(message.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n')}`)}
      footer={copy.pendingHelp}
    />
    <Panel
      title={`${copy.attachmentsTitle}: ${props.attachments?.length ?? 0}`} color={PALETTE.asking}
      limit={attachmentLimit} more={copy.moreLines}
      items={(props.attachments ?? []).map((item, index) => `${index + 1}. ${formatAttachment(item)}`)}
      footer={copy.attachmentsHelp}
    />
    {composer.submitting && sendingLimit > 0 && <Text color={PALETTE.waiting} wrap="truncate-end">{copy.attachmentsSending}</Text>}
    {interaction !== undefined && <InteractionView key={interaction.id} interaction={interaction} copy={copy} limit={interaction.kind === 'questions' ? menuLimit : interactionLimit} height={interactionLimit} columns={size.columns} onAnswer={props.onAnswer} />}
    {props.command !== undefined && commandLimit > 0 && <Box flexShrink={0} maxHeight={commandLimit} overflowY="hidden">
      <Text wrap="truncate-end">{copy.command}: {props.command}</Text>
    </Box>}
    {props.notice !== undefined && <Notice text={props.notice} limit={noticeLimit} more={copy.moreLines} />}
    {props.installing !== undefined && installingLimit > 0 && <Installing step={props.installing}
      glyphs={props.frame === 'classic' ? 'ascii' : 'unicode'} clock={animate} />}
    {props.quitting && quitLimit > 0 && <Text color={PALETTE.waiting} wrap="truncate-end">{copy.quit}</Text>}
    {matches !== undefined && interaction === undefined && completionLimit > 0 && <Box flexDirection="column" flexShrink={0} height={completionLimit} overflowY="hidden">
      {menuRows > 0 && <Completion
        items={visibleMatches.map(entry => ({
          name: entry.name,
          // The kind is dropped when every visible row shares one. Nine rows
          // reading `Command` say nothing that the panel itself does not.
          description: [mixedKinds ? copy[entry.kind] : '', entry.hint ?? '', entry.description].filter(part => part !== '').join('  '),
        }))}
        selected={menuWindow.selected}
        hidden={menuRows > 1 ? menuWindow.hidden : 0}
        more={`+${menuWindow.hidden} ${copy.moreMatches}`}
      />}
      {matches.length === 0 && !visibleMenu?.loading && <Status text={visibleMenu?.kind === 'file' ? copy.noFiles : copy.noCompletions} />}
      {visibleMenu?.loading === true && <Status text={visibleMenu?.kind === 'file' ? copy.filesLoading : copy.catalogLoading} />}
      {visibleMenu?.error !== undefined && <Status text={`${visibleMenu?.kind === 'file' ? copy.filesError : copy.catalogError}: ${visibleMenu.error}`} tone="error" />}
    </Box>}
    {usage !== undefined && usageLimit > 0 && <Status text={`/${usage.name} ${usage.hint}  ${usage.description}`} />}
    </>
  if (props.inspection !== undefined) {
    const child = props.inspection
    const { context: _context, usage: _usage, plan: _plan, goal: _goal, permission: _permission, thinkingLevel: _thinkingLevel,
      compactPhase: _compactPhase, autoCompacting: _autoCompacting, ...childProps } = props
    return <SessionView {...childProps} {...child} key={child.sessionId} inspection={undefined}
      inspectionParent={props.sessionId} inspectionLabel={child.label} inputBlocked={true} stopping={false}
      // The parent's children stay listed under the input, inert here, so the
      // input keeps its row while a child is open and after it closes.
      pending={[]} todos={undefined} subagents={props.subagents ?? []} attachments={[]} context={child.context}
      interaction={undefined} command={undefined} notice={undefined} />
  }
  const controlsView = sheetStandalone ? sheetBlock : <>
        {!fullscreen && sheet === undefined && <LiveRegion rows={props.live} budget={budget} limit={liveTotal} result={result} clock={animate} />}
        {/* The held rows: under the output, so what streams stays against the
            history it continues, and over the controls, which stay together. */}
        {!fullscreen && <Box flexGrow={1} />}
        {interaction === undefined
          ? (
            <Chrome
              // No state word. The header says what the session is doing.
              // The composer's placeholder and hint say whether it is idle.
              // One layout in every mode; the fields give way in their own
              // order as the row narrows (`status-line.ts`).
              status={statusFields({
                model: props.model, plan: props.plan, thinkingLevel: props.thinkingLevel, context: props.context,
                git: props.git, usage: props.usage, update: props.update, cwd: props.cwd,
                glyphs: props.frame === 'classic' ? 'ascii' : 'unicode',
              }, copy)}
              columns={size.columns}
              state={{ running: props.inspectionParent === undefined && props.status === 'running', asking: false, listing: matches !== undefined }}
              before={composer.before}
              after={composer.after}
              // Idle invites a prompt. While a turn runs, Enter steers instead of
              // sending, and the placeholder is the only text that says so. While
              // `/compact` runs, Enter queues a prompt to run once it is done.
              // While a session switch holds the input, it says why keys do nothing.
              placeholder={props.inspectionParent !== undefined ? copy.subagentBack : props.inputBlocked === true ? copy.sessionsBusy : props.compactPhase !== undefined ? copy.compactWait
                : props.status === 'running' ? copy.steering : [copy.prompt, copy.promptCommands, copy.promptFiles]}
              // The panel carries no key help of its own. The slot names the one key
              // that is not discoverable by pressing it.
              hints={hints}
              overflow={{ above: copy.composerAbove, below: copy.composerBelow }}
              maxRows={budget.composer}
              layout={budget.chrome}
              frame={props.frame}
              activity={activity}
              standing={goalStanding === undefined ? undefined : focus === 'goal'
                ? { ...goalStanding, details: [copy.goalOpen, goalStanding.details].filter(part => part !== '').join(' · ') }
                : { ...goalStanding, key: copy.goalKey }}
              standingFocused={focus === 'goal'}
              clock={clock}
              motion={animate !== undefined}
              compact={screenReader}
              {...subagentLimit === 0 ? {} : { footer: (columns: number) => props.inspectionParent === undefined
                ? <SubagentRow entries={props.subagents ?? []} workflows={props.workflows} copy={copy} columns={columns} focused={focus === 'subagents'}
                  hint={copy.subagentsKey} />
                : <InspectionBar label={props.inspectionLabel ?? props.sessionId} entries={props.subagents ?? []} id={props.sessionId}
                  working={props.status === 'running'} copy={copy} columns={columns} /> }}
            >
              {panels}
            </Chrome>
            )
          : (
            <Box flexDirection="column" flexShrink={0}>
              {openLimit > 0 && <Text> </Text>}
              {panels}
            </Box>
            )}
        </>
  return <Beat clock={clock}>{fullscreen
    ? <React.Suspense fallback={controlsView}><Fullscreen ref={scroll} transcript={props.committed} live={props.live} heading={heading} opening={opening}
        budget={budget} result={result} copy={copy} frame={props.frame} size={size} clock={animate}>{controlsView}</Fullscreen></React.Suspense>
    : <Scrollback transcript={props.committed} heading={heading} opening={opening} budget={budget} result={result}
        copy={copy} frame={props.frame} size={size} repainting={repainting}>{controlsView}</Scrollback>}
  </Beat>
}
