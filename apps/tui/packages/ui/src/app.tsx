/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, Text, useInput, useIsScreenReaderEnabled, usePaste, useWindowSize } from 'ink'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import { cacheHit, contextPercent, formatContext, formatTotals, type ContextUsage, type TokenTotals } from './format.ts'
import { useComposer, type Submit } from './composer.ts'
import { argumentQuery, commandUsage, completionMenu, requiresInput, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, selectionWindow, type Budget, type FrameStyle, type WindowSize } from './layout.ts'
import { compactModel, compactPath, present, type Highlight, type ResultBound } from './present.ts'
import { cacheTone, permissionTone, PALETTE, type PaletteColor } from './palette.ts'
import type { SubagentEntry } from './subagents.tsx'
import { goalSheet, goalState, type GoalEntry } from './goal.ts'
import { Sheet, sheetPage, sheetRows, type SheetLine } from './sheet.tsx'
import { Tasks, taskSheet, taskSheetTitle, tasksOpen, type TaskEntry } from './tasks.tsx'
import { Beat } from './beat.tsx'
import { Scrollback, type Opening } from './scrollback.tsx'
import { Chrome, Completion, Line, LiveRegion, Notice, Panel, Thinking, THINKING_GAP, wrappedRows, type ActivityState } from './line.tsx'
import { activityWord, phaseLabel, phaseOf, lastTurn, THINKING_ROWS, thinkingRows, turnSummary, type Clock } from './activity.ts'

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
  /** Suppress composer edits while the application prepares a session handoff. */
  readonly inputBlocked?: boolean
  readonly attachments?: readonly AttachmentSummary[]
  readonly committed: Transcript
  readonly live: readonly Row[]
  readonly pending: readonly PendingInput[]
  readonly status: AgentStatus
  readonly stopping: boolean
  readonly command: string | undefined
  /** Manual compaction progress from the matching command and session lifecycle events. */
  readonly compactPhase?: 'preparing' | 'summarizing' | 'saving'
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
  readonly onSubagents?: () => void
  /**
   * A newer Bake release for this install. `installed` means `current` already
   * names it, so a restart runs it; otherwise `/update` installs it. Named in
   * the status line, the bounded field that yields first. Absent, nothing is said.
   */
  readonly update?: { readonly version: string; readonly installed: boolean }
  /** Shift-Tab: step the selected model's reasoning effort. Absent, Shift-Tab does nothing. */
  readonly onCycleThinking?: () => void
  /** Harness plan projection; absent when this profile has no plan mode. */
  readonly plan?: { readonly active: boolean; readonly pending: boolean }
  /** Current goal from the Harness goal service; absent without one or before `/goal` sets it. */
  readonly goal?: GoalEntry | undefined
  /** Effective permission preset from the session projection; absent without that service. */
  readonly permission?: string
  /** Selected reasoning effort, or the model's advertised default when known. */
  readonly thinkingLevel?: string
  readonly model: string
  readonly cwd: string
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
  return <>{present(row, result, line => wrappedRows(line, budget)).map((line, index) => <Line key={index} line={line} budget={budget} />)}</>
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
 * that overflows the viewport. The caller overflows for the one frame this
 * returns true. The next frame no longer overflows, and it is cleared and
 * replayed too.
 *
 * Child inspection changes the mounted transcript and frame together. Replay
 * also reanchors that transition at the terminal bottom.
 * @param size - current terminal size.
 * @param view - displayed parent or child identity.
 * @param openingChild - whether the first frame must reanchor after a parent.
 * @returns true for one render after a resize or inspection transition.
 */
function useRepaint(size: WindowSize, view: string, openingChild: boolean): boolean {
  const painted = useRef(size)
  const paintedView = useRef<string | undefined>(openingChild ? undefined : view)
  const [, repaint] = useState(0)
  const repainting = view !== paintedView.current || size.columns < painted.current.columns || size.rows > painted.current.rows
  useLayoutEffect(() => {
    painted.current = size
    paintedView.current = view
    if (repainting) repaint(count => count + 1)
  })
  return repainting
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
type SheetKind = 'goal' | 'tasks'

function SessionView(props: AppProps): React.ReactElement {
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
  const openSheet = (next: SheetKind | undefined): void => {
    sheetRef.current = next
    setSheet(next)
    setSheetScroll(0)
  }
  const tasksShown = tasksOpen(props.todos)
  const hasTasks = (props.todos?.length ?? 0) > 0
  const hasSubagents = (props.subagents?.length ?? 0) > 0
  const available = (target: Focus): boolean =>
    target === 'goal' ? props.goal !== undefined : target === 'tasks' ? tasksShown : hasSubagents
  const inert = interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined
  useEffect(() => {
    if (focusRef.current !== undefined && (inert || !available(focusRef.current))) focusOn(undefined)
  }, [inert, props.goal === undefined, tasksShown, hasSubagents])
  useEffect(() => {
    if (sheetRef.current !== undefined && (inert || (sheetRef.current === 'goal' ? props.goal === undefined : !hasTasks))) openSheet(undefined)
  }, [inert, props.goal === undefined, hasTasks])
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
  usePaste(composer.paste, { isActive: sheet === undefined && interaction === undefined && props.inputBlocked !== true && props.inspection === undefined && !composer.submitting })
  useInput((text, key) => {
    if (props.inspection !== undefined) return
    if (key.ctrl && text === 'c') { props.onInterrupt(); return }
    if (sheetRef.current !== undefined) {
      if (key.escape || key.return) { openSheet(undefined); return }
      if (key.upArrow || (key.ctrl && text === 'p')) setSheetScroll(current => Math.max(0, Math.min(current, sheetMaxScroll) - 1))
      if (key.downArrow || (key.ctrl && text === 'n')) setSheetScroll(current => Math.min(sheetMaxScroll, current + 1))
      if (key.pageUp) setSheetScroll(current => Math.max(0, Math.min(current, sheetMaxScroll) - sheetRowsShown))
      if (key.pageDown) setSheetScroll(current => Math.min(sheetMaxScroll, current + sheetRowsShown))
      if (key.home) setSheetScroll(0)
      if (key.end) setSheetScroll(Number.MAX_SAFE_INTEGER)
      return
    }
    if (props.quitting) props.onQuitDismiss?.()
    const inputMenu = matchesFor(composer.value, composer.position)
    const choices = inputMenu?.entries
    if (key.escape) {
      if (focusRef.current !== undefined) { focusOn(undefined); return }
      if (choices !== undefined) { updateMenu('', true); return }
      props.onCancel(); return
    }
    if (interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined || composer.blocked || key.meta) return
    if (key.ctrl && text === 'o' && props.goal !== undefined) { focusOn(undefined); openSheet('goal'); return }
    if (key.ctrl && text === 't' && hasTasks) { focusOn(undefined); openSheet('tasks'); return }
    if (key.ctrl && text === 'g' && hasSubagents) { focusOn(undefined); props.onSubagents?.(); return }
    const focused = focusRef.current
    if (focused !== undefined && available(focused)) {
      if (focused === 'subagents') {
        if (key.upArrow || key.leftArrow) { focusOn(undefined); return }
        if (key.downArrow || key.rightArrow) return
        if (key.return) { focusOn(undefined); props.onSubagents?.(); return }
      } else {
        // Above the composer the task row sits over the goal's header.
        if (key.upArrow) { if (focused === 'goal' && tasksShown) focusOn('tasks'); return }
        if (key.downArrow) { focusOn(focused === 'tasks' && props.goal !== undefined ? 'goal' : undefined); return }
        if (key.leftArrow) { focusOn(undefined); return }
        if (key.rightArrow) return
        if (key.return) { focusOn(undefined); openSheet(focused); return }
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
      composer.recall(text === 'p' ? 'older' : 'newer'); updateMenu('', true); return
    }
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
      // Up walks back through history first; only past its oldest entry does
      // it reach the goal on the header, or the task row when there is no
      // goal. Leaving history restores the unsent draft rather than a stale
      // entry one Enter would rerun.
      const above = props.goal !== undefined ? 'goal' : tasksShown ? 'tasks' : undefined
      if (!composer.recall(key.upArrow ? 'older' : 'newer') && key.upArrow && above !== undefined) {
        composer.leave(); focusOn(above)
      }
      updateMenu('', true); return
    }
    if (key.shift && key.return) { composer.paste('\n'); return }
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
  const budget = useMemo(() => budgetFor(size), [size.columns, size.rows])
  const repainting = useRepaint(size, props.inspection?.sessionId ?? props.sessionId, props.inspectionParent !== undefined)
  const screenReader = useIsScreenReaderEnabled()
  const clock = screenReader ? undefined : props.clock
  const compactStarted = useRef<number | undefined>(undefined)
  if (props.compactPhase === undefined) compactStarted.current = undefined
  else compactStarted.current ??= clock?.now() ?? 0
  const running = props.status === 'running'
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
  // Reasoning is not part of the live region. The thinking window above the
  // header draws it. Drawn row by row in the live region, it arrives faster
  // than it can be read and scrolls the surface.
  const liveRows = useMemo(() => props.live.filter(row => row.kind !== 'reasoning'), [props.live])
  const thinking = useMemo(() => thinkingRows(props.live, budget.measure, THINKING_ROWS), [props.live, budget])
  const heading = props.inspectionParent === undefined ? `${copy.session}: ${props.sessionId}`
    : `${copy.subagentParent}: ${props.inspectionParent} > ${props.sessionId}`
  // Memoized so the committed transcript is not re-rendered on every frame.
  const result = useMemo<ResultBound>(
    () => ({ lines: props.resultLines, unit: copy.cardLines, single: copy.cardLine, more: copy.moreLines, failures: copy.summaryFailures, earlier: copy.earlierCalls, ...props.highlight === undefined ? {} : { code: props.highlight } }),
    [props.resultLines, props.highlight, copy])
  // Decided once per mount. A session with no history when it opens gets the
  // block, and keeps it in the stream while its first turn commits.
  const [opening] = useState<Opening | undefined>(() => props.version === undefined
    || props.inspectionParent !== undefined || props.committed.length > 0
    ? undefined
    : { kind: 'welcome', version: props.version, heading: `${copy.session}: ${props.sessionId}` })
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
  const interactionLimit = claim(interaction === undefined ? 0 : menuLimit)
  // An open sheet takes every row the interaction leaves. Below five it
  // replaces the whole region, composer included, so it can still be read.
  const sheetView: { readonly title: string, readonly color: PaletteColor, readonly lines: readonly SheetLine[] } | undefined =
    sheet === 'goal' && props.goal !== undefined
      ? { title: copy.goalTitle, color: goalState(props.goal, copy)!.color, lines: goalSheet(props.goal, copy) }
      : sheet === 'tasks' && props.todos !== undefined && hasTasks
        ? { title: taskSheetTitle(props.todos, copy), color: PALETTE.asking, lines: taskSheet(props.todos) }
        : undefined
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
  const liveLimit = claim(liveRows.length > 0 ? liveWant : 0)
  // After the output it summarizes, which it cannot outrank on a short
  // terminal; the header already says the turn is running.
  const thinkingLimit = claim(interaction !== undefined || !running || thinking.length === 0 ? 0 : thinking.length + THINKING_GAP)
  const taskLimit = claim(tasksShown ? 1 : 0)
  const pendingLimit = claim(props.pending.length === 0 ? 0 : menuLimit)
  const attachmentLimit = claim((props.attachments?.length ?? 0) === 0 ? 0 : menuLimit)
  // One row, and the command itself may be long enough to wrap past it.
  // Compaction's progress is the header's to show, in place of the command.
  const commandLimit = claim(props.compactPhase === undefined && props.command !== undefined ? 1 : 0)
  const noticeLimit = claim(props.notice === undefined ? 0 : budget.notice)
  const menuRows = Math.max(0, completionLimit - menuStatusRows)
  const menuWindow = selectionWindow(matches ?? [], selected, menuRows, props.completionLimit)
  const visibleMatches = menuWindow.shown
  const mixedKinds = new Set(visibleMatches.map(entry => entry.kind)).size > 1
  // Everything between the conversation and the input is one stack, opened by
  // the chrome's blank row. A notice or a list belongs to the input, not to
  // one more line of the answer above it.
  const hit = props.usage === undefined ? undefined : cacheHit(props.usage)
  // Header text, in priority order. Compaction while it runs, otherwise the
  // current turn, otherwise how the last turn ended.
  const activity: ActivityState | undefined = props.compactPhase !== undefined
    ? {
      kind: 'running', word: copy.compacting, startedAt: compactStarted.current ?? 0, color: PALETTE.running,
      phase: { preparing: copy.compactPreparing, summarizing: copy.compactSummarizing, saving: copy.compactSaving }[props.compactPhase],
    }
    : turn.current !== undefined
      ? {
        kind: 'running', word: props.stopping ? copy.stopping : turn.current.word, startedAt: turn.current.startedAt,
        phase: props.stopping ? undefined : phaseLabel(phaseOf(props.live, lastCommitted), copy),
        color: props.stopping ? PALETTE.failed : PALETTE.running,
      }
      : summary === undefined ? undefined : { kind: 'ended', summary }
  const goalStanding = goalState(props.goal, copy)
  const working = props.subagents?.filter(entry => entry.state === 'working').length ?? 0
  const sheetBlock = sheetView === undefined ? null : <Sheet {...sheetView} copy={copy} columns={size.columns}
    limit={sheetViewLimit} offset={sheetScroll} frame={props.frame} />
  const panels = sheetView !== undefined && !sheetStandalone ? sheetBlock : <>
    {turn.current !== undefined && interaction === undefined && <Thinking rows={thinking} limit={thinkingLimit} />}
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
    {interaction !== undefined && <InteractionView key={interaction.id} interaction={interaction} copy={copy} limit={interactionLimit} onAnswer={props.onAnswer} />}
    {props.command !== undefined && commandLimit > 0 && <Box flexShrink={0} maxHeight={commandLimit} overflowY="hidden">
      <Text wrap="truncate-end">{copy.command}: {props.command}</Text>
    </Box>}
    {props.notice !== undefined && <Notice text={props.notice} limit={noticeLimit} more={copy.moreLines} />}
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
      compactPhase: _compactPhase, ...childProps } = props
    return <SessionView {...childProps} {...child} key={child.sessionId} inspection={undefined}
      inspectionParent={props.sessionId} inputBlocked={true} stopping={false}
      pending={[]} todos={undefined} subagents={[]} attachments={[]} context={child.context}
      interaction={undefined} command={undefined}
      notice={`${child.label} · ${copy.subagentBack}`} />
  }
  return <Scrollback transcript={props.committed} heading={heading} opening={opening} budget={budget} result={result}
    copy={copy} frame={props.frame} size={size} repainting={repainting} recording={sheet === undefined}>
    <Beat clock={clock}>
        {sheetStandalone ? sheetBlock : <>
        {sheet === undefined && <LiveRegion rows={liveRows} budget={budget} limit={liveLimit} result={result} clock={animate} />}
        {/* The held rows: under the output, so what streams stays against the
            history it continues, and over the controls, which stay together. */}
        <Box flexGrow={1} />
        {interaction === undefined
          ? (
            <Chrome
              // No state word. The header says what the session is doing.
              // The composer's placeholder and hint say whether it is idle.
              left={[{ text: `${copy.model}: ${compactModel(props.model)}` },
                ...props.plan === undefined || (!props.plan.active && !props.plan.pending) ? []
                  : [props.plan.pending ? (props.plan.active ? copy.planExitPending : copy.planEntryPending) : copy.planActive]]}
              {...props.permission === undefined ? {} : { badge: {
                label: copy.permission, value: props.permission, color: permissionTone(props.permission),
              } }}
              {...props.thinkingLevel === undefined ? {} : { secondaryBadge: {
                label: copy.thinking, value: props.thinkingLevel, color: PALETTE.asking,
              } }}
              right={[
                // In priority order, because narrowing drops them from the end.
                ...(props.subagents?.length ?? 0) === 0 ? [] : [{
                  text: `${focus === 'subagents' ? '> ' : '↓ '}${copy.subagentsTitle}: ${props.subagents!.length}${working === 0 ? '' : ` · ${working} ${copy.subagentWorking}`}${focus === 'subagents' ? ` · ${copy.subagentsOpen}` : ''}`,
                  short: `${focus === 'subagents' ? '>' : '↓'} ${props.subagents!.length}`,
                  selected: focus === 'subagents',
                }],
                // Occupancy is what a user compacts on. The totals are what the
                // session cost. The path is last because it is the unbounded
                // field. The status line shortens it from the start and keeps
                // the tail, which names the workspace.
                ...props.context === undefined ? [] : [{ text: `${copy.context}: ${formatContext(props.context)}`,
                  short: `${copy.contextShort} ~${contextPercent(props.context)}%` }],
                ...props.usage === undefined ? [] : formatTotals(props.usage, { input: copy.tokensIn, output: copy.tokensOut }),
                ...hit === undefined ? [] : [{ label: copy.cacheHit, value: `${hit}%`, color: cacheTone(hit) }],
                // Last of the bounded fields: it drops before any reading of the session.
                ...props.update === undefined ? [] : [{
                  label: copy.updateLabel, color: PALETTE.waiting,
                  value: `v${props.update.version} · ${props.update.installed ? copy.updateRestart : '/update'}`,
                }],
                compactPath(props.cwd, process.env['HOME']),
              ]}
              columns={size.columns}
              state={{ running: props.inspectionParent === undefined && props.status === 'running', asking: false, listing: matches !== undefined }}
              before={composer.before}
              after={composer.after}
              // Idle invites a prompt. While a turn runs, Enter steers instead of
              // sending, and the placeholder is the only text that says so. While
              // a session switch holds the input, it says why keys do nothing.
              placeholder={props.inspectionParent !== undefined ? copy.subagentBack : props.inputBlocked === true ? copy.sessionsBusy : props.compactPhase !== undefined ? copy.compactWait
                : props.status === 'running' ? copy.steering : copy.prompt}
              // The panel carries no key help of its own. The slot names the one key
              // that is not discoverable by pressing it.
              hints={{ send: copy.send, interrupt: copy.interrupt, select: copy.tabCompletes, answer: copy.send }}
              maxRows={budget.composer}
              layout={budget.chrome}
              frame={props.frame}
              activity={activity}
              standing={goalStanding === undefined ? undefined : focus === 'goal'
                ? { ...goalStanding, details: `${copy.goalOpen} · ${goalStanding.details}` }
                : { ...goalStanding, key: copy.goalKey }}
              standingFocused={focus === 'goal'}
              clock={clock}
              motion={animate !== undefined}
              compact={screenReader}
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
        </>}
    </Beat>
  </Scrollback>
}
