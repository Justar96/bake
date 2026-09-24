/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, measureElement, Static, Text, useInput, useIsScreenReaderEnabled, usePaste, useWindowSize, type DOMElement } from 'ink'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import { cacheHit, formatContext, formatTotals, type ContextUsage, type TokenTotals } from './format.ts'
import { useComposer, type Submit } from './composer.ts'
import { completionMenu, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, COLUMN, MARKER, selectionWindow, type Budget, type FrameStyle, type WindowSize } from './layout.ts'
import { compactModel, compactPath, present, type Highlight, type ResultBound } from './present.ts'
import { cacheTone, permissionTone, PALETTE } from './palette.ts'
import { Subagents, type SubagentEntry } from './subagents.tsx'
import { Beat } from './beat.tsx'
import { Welcome } from './welcome.tsx'
import { Chrome, Completion, Line, lineHeight, LiveRegion, Notice, Panel, Thinking, wrappedRows, type RuleState } from './line.tsx'
import { activityWord, phaseLabel, phaseOf, lastTurn, THINKING_ROWS, thinkingRows, turnSummary, type Clock } from './activity.ts'

/** Display-only projection of one pending inbox message. */
export interface PendingInput {
  readonly id: string
  readonly target: 'next-step' | 'next-turn'
  readonly text: string
  readonly attachments?: readonly AttachmentSummary[]
}

/**
 * Display-only projection of one entry in the agent's task list.
 *
 * The agent replaces the whole list on every write, so an entry needs no
 * identity: what it is and where it stands is all this surface shows.
 */
export interface TaskEntry {
  readonly text: string
  readonly status: 'pending' | 'in_progress' | 'completed'
}

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
   * Separate from `notice` because it is key state rather than feedback about
   * the session: it remains beside the composer, it must not be cleared
   * by a command result, and it must not clear one.
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
    readonly permission?: string
    readonly thinkingLevel?: string
  } | undefined
  readonly inspectionParent?: string
  readonly onSubagents?: () => void
  /** Harness plan projection; absent when this profile has no plan mode. */
  readonly plan?: { readonly active: boolean; readonly pending: boolean }
  /** Effective permission preset from the session projection; absent without that service. */
  readonly permission?: string
  /** Selected reasoning effort, or the model's advertised default when known. */
  readonly thinkingLevel?: string
  readonly model: string
  readonly cwd: string
  readonly sessionId: string
  /**
   * Running Bake version. Supplied, a session that mounts with no history
   * opens with the welcome block, which carries its own session line; absent,
   * it opens with the session heading alone.
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
  /** Maximum visible completion and picker rows, supplied by application configuration. */
  readonly completionLimit: number
  /**
   * Tool-result lines the transcript keeps under each outcome, the rest
   * counted. The full text is in the session log, and an unbounded listing
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
   * Time source for the rule's spinner, light, and elapsed time.
   *
   * Absent, the rule draws a resting glyph and no clock: the presentation
   * layer reads no time of its own, so a test or a static preview renders the
   * same frame every run. Ignored while a screen reader is active, which
   * would announce every frame.
   */
  readonly clock?: Clock
  /**
   * Whether the rule's glyph cycles, its light sweeps, and a running action's
   * marker pulses; defaults to true. Off — `NO_COLOR` — the clock still counts
   * the elapsed seconds, and nothing else on the surface moves by itself.
   */
  readonly motion?: boolean
  readonly onSubmit: Submit
  readonly onCancel: () => void
  readonly onInterrupt: () => void
  /**
   * Called for any key other than Ctrl-C while `quitting`: the user went on
   * with something else, so the quit prompt should go rather than wait out
   * its window, and a later Ctrl-C starts over instead of quitting.
   */
  readonly onQuitDismiss?: () => void
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}

/**
 * One dim line under the completion panel: what it is doing, or why it is empty.
 * @param props.text - locale-owned message.
 * @param props.tone - `error` to colour a failure.
 * @returns the message row, aligned with the panel's names.
 */
function Status({ text, tone }: { readonly text: string, readonly tone?: 'error' }): React.ReactElement {
  return <Box flexDirection="row">
    <Box width={2} flexShrink={0}><Text> </Text></Box>
    <Text dimColor={tone === undefined} {...tone === 'error' ? { color: PALETTE.failed } : {}}>{text}</Text>
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

/** Terminal rows a committed row prints, measured as {@link RowView} draws it. */
function rowHeight(row: Row, budget: Budget, result: ResultBound): number {
  return present(row, result, line => wrappedRows(line, budget)).reduce((rows, line) => rows + lineHeight(line, budget), 0)
}

/**
 * The agent's task list, as current state rather than as history.
 *
 * Every write replaces the list, so the transcript would show the same plan
 * several times with a different tick each time. A panel shows the one version
 * that is still true, and costs its rows only while a list exists.
 *
 * Drawn as a checklist rather than a tree, so it never reads as the subagent
 * panel beside it: a heading with a progress bar and a count, then each task
 * on its own row, indented under the heading, with a box that fills as the
 * agent works. The task in progress points in ocean blue, finished ones are ticked
 * green and struck through, and the ones still waiting hold an empty box, so
 * `NO_COLOR` still reads every state by shape. The bar is drawn in heavy and
 * light rules for the same reason.
 *
 * The list keeps the agent's order, so a finished task stays where the plan
 * put it. When rows run short, finished tasks give up theirs first, oldest
 * first: they are what the reader has already watched happen, and the rows are
 * needed by the work that has not. The list is capped like every other panel,
 * because the dynamic region shares one budget and a long plan would spend
 * the live region's share of it; each task is one row, truncated, so the cap
 * is exact.
 *
 * @param props.todos - the current list, in the agent's own order.
 * @param props.copy - locale-owned labels.
 * @param props.limit - rows the panel may draw, head and overflow included.
 * @returns the panel, or null when nothing is left to do or there is no room.
 */
export function Tasks({ todos, copy, limit }: {
  readonly todos: readonly TaskEntry[]
  readonly copy: TuiCopy
  readonly limit: number
}): React.ReactElement | null {
  const done = todos.filter(item => item.status === 'completed').length
  if (done === todos.length || limit <= 0) return null
  // The head and the overflow count are rows of the panel, not extras on top
  // of it: counting only the entries makes every claim against this panel
  // short, and two rows is the whole chrome.
  const room = limit - 1
  let shown = [...todos]
  while (shown.length > room) {
    const finished = shown.findIndex(item => item.status === 'completed')
    if (finished < 0) break
    shown.splice(finished, 1)
  }
  if (shown.length > room) shown = shown.slice(0, Math.max(0, room - 1))
  const hidden = todos.length - done - shown.filter(item => item.status !== 'completed').length
  const filled = Math.round(TASK_BAR * done / todos.length)
  return <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
    <Text wrap="truncate-end">
      <Text bold>{copy.todoTitle}</Text>
      {'  '}
      <Text color={PALETTE.asking}>{TASK_GLYPH.filled.repeat(filled)}</Text>
      <Text dimColor>{TASK_GLYPH.empty.repeat(TASK_BAR - filled)}</Text>
      <Text dimColor>{`  ${done}/${todos.length} ${copy.todoDone}`}</Text>
    </Text>
    {shown.map((item, index) => <Task key={index} status={item.status} text={item.text} />)}
    {hidden > 0 && room > 0 && <Box paddingLeft={COLUMN.rail + 2} flexShrink={0}>
      <Text dimColor wrap="truncate-end">+{hidden} {copy.todoPending}</Text>
    </Box>}
  </Box>
}

/** Cells of the task panel's progress bar: enough to move on every task of a short plan. */
const TASK_BAR = 12

/** The task panel's own shapes, apart from the markers the subagent panel uses. */
const TASK_GLYPH = {
  done: '\u2713',
  active: MARKER.selected,
  pending: '\u25a1',
  filled: '\u2501',
  empty: '\u2500',
} as const

/** One task: its box in the rail's width past the heading's indent, and its text truncated to one row. */
function Task({ status, text }: { readonly status: TaskEntry['status'], readonly text: string }): React.ReactElement {
  const glyph = status === 'completed' ? TASK_GLYPH.done : status === 'in_progress' ? TASK_GLYPH.active : TASK_GLYPH.pending
  const color = status === 'completed' ? PALETTE.done : status === 'in_progress' ? PALETTE.asking : undefined
  return <Box flexDirection="row" flexShrink={0} paddingLeft={COLUMN.rail}>
    <Box width={2} flexShrink={0}>
      <Text bold={color !== undefined} {...color === undefined ? { dimColor: true } : { color }}>{glyph}</Text>
    </Box>
    <Text wrap="truncate-end" bold={status === 'in_progress'} dimColor={status !== 'in_progress'}
      strikethrough={status === 'completed'}>{text}</Text>
  </Box>
}

/** The welcome block as the first committed item: the version and session it opens. */
interface Opening { readonly kind: 'welcome', readonly version: string, readonly heading: string }

/**
 * Items `Static` prints ahead of the transcript.
 *
 * One either way: a session that opens with the welcome block folds the
 * session line into it, and one that opens with history prints the heading
 * alone, so the block never adds a row to the stream.
 */
const LEAD_ITEMS = 1

/** Print each suffix once, retaining Ink's accumulated scrollback across resize. */
const CommittedTranscript = memo(function CommittedTranscript({ transcript, heading, opening, budget, result, copy, frame, columns }: {
  readonly transcript: Transcript
  readonly heading: string
  readonly opening: Opening | undefined
  readonly budget: Budget
  readonly result: ResultBound
  readonly copy: TuiCopy
  readonly frame: FrameStyle
  readonly columns: number
}): React.ReactElement {
  // Ink 7 Static consumes only length and slice(index). Adapt the persistent
  // transcript at this boundary so appends do not copy its entire prefix.
  // Static must keep its identity: remounting clears Ink's saved history,
  // leaving only the latest suffix when a terminal resize requires a replay.
  const items = useMemo(() => ({
    length: transcript.length + LEAD_ITEMS,
    slice(start = 0): (Row | Opening)[] {
      // The block is a whole item, not the first of several. At `start < 1` it
      // still belongs to the suffix, so slice it as the one lead item it is.
      const suffix = transcriptRows(transcript, Math.max(0, start - LEAD_ITEMS))
      const head: (Row | Opening)[] = opening === undefined
        ? [{ kind: 'notice', tone: 'info', text: heading }]
        : [opening]
      return start === 0 ? [...head, ...suffix] : start < LEAD_ITEMS ? [...head.slice(start), ...suffix] : suffix
    },
  }) as (Row | Opening)[], [transcript, heading, opening])
  return <Static items={items}>
    {(item, index) => item.kind === 'welcome'
      ? <Welcome key={index} version={item.version} heading={item.heading} copy={copy} frame={frame} columns={columns} />
      : <RowView key={index} row={item} budget={budget} result={result} />}
  </Static>
})

/**
 * Whether the terminal has just become narrower or taller than the last painted frame.
 *
 * Ink erases the previous frame by counting its lines, but a terminal that
 * reflows on resize has already re-wrapped each full-width row of it — the
 * composer surface's rows — onto two, so a narrowing leaves those rows on screen.
 * A terminal that grows taller adds its rows under the frame, unless it pulls
 * history down from scrollback, and leaves the composer off the bottom row.
 * Ink clears the terminal and replays history only for a frame that overflows
 * the viewport; the caller overflows for the one frame this returns true, and
 * the frame after it, overflowing no longer, is cleared and replayed too.
 * @param size - current terminal size.
 * Child inspection changes the mounted transcript and frame together; replay
 * also reanchors that transition at the terminal bottom.
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
 * The height the dynamic frame holds, so the composer never rises off the bottom row.
 *
 * The runner starts the frame on the terminal's bottom row. A frame drawn
 * there stays there while it and the history printed above it in the same
 * render fill at least the rows the previous frame did; a shorter one — a
 * menu closing, a notice clearing, a task finishing — would leave the
 * composer that many rows up. So the frame keeps its previous height less
 * what prints, and the rows its content does not fill stay blank above the
 * controls until printed history takes them.
 *
 * What prints is measured the way {@link RowView} draws it, before the render
 * that prints it, because a floor corrected after the frame was written would
 * already have moved the composer once.
 *
 * @param committed - the transcript `Static` prints.
 * @param lead - items `Static` prints ahead of the transcript.
 * @param budget - budgets for the current terminal size.
 * @param result - the preview bound committed rows print with.
 * @param repainting - whether this render repaints the screen, which
 *   replays history and the frame together and so holds nothing.
 * @returns the frame's minimum height, and the ref that measures the frame.
 */
function useHeldHeight(committed: Transcript, lead: number, budget: Budget, result: ResultBound, repainting: boolean): {
  readonly floor: number
  readonly frame: React.RefObject<DOMElement | null>
} {
  const frame = useRef<DOMElement>(null)
  // Items `Static` has printed, counting the session line whether it prints
  // alone or inside the welcome block, and the frame's height as last laid out.
  const held = useRef({ printed: 0, height: 0 })
  const items = committed.length + lead
  let floor = 0
  if (!repainting && held.current.height > 0) {
    let remaining = held.current.height
    if (held.current.printed < items) {
      for (const row of transcriptRows(committed, Math.max(0, held.current.printed - lead))) {
        remaining -= rowHeight(row, budget, result)
        if (remaining <= 0) break
      }
    }
    floor = Math.min(budget.dynamic, Math.max(0, remaining))
  }
  useLayoutEffect(() => {
    held.current = { printed: items, height: repainting || frame.current === null ? 0 : measureElement(frame.current).height }
  })
  return { floor, frame }
}

/**
 * Render the terminal session. Ink owns key decoding and bracketed-paste mode.
 * @param props - harness state, localized copy, and application callbacks.
 * @returns transcript, active interaction, status, and composer.
 */
export function App(props: AppProps): React.ReactElement {
  return <SessionView key={props.sessionId} {...props} />
}

function SessionView(props: AppProps): React.ReactElement {
  const composer = useComposer(props.onSubmit, () => inputHistory(props.committed, props.pending), (props.attachments?.length ?? 0) > 0)
  const { copy, interaction } = props
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
  const selected = matches === undefined ? 0 : selectedIndex(matches, composer.text, composer.cursor)
  usePaste(composer.paste, { isActive: interaction === undefined && props.inputBlocked !== true && props.inspection === undefined && !composer.submitting })
  useInput((text, key) => {
    if (props.inspection !== undefined) return
    if (key.ctrl && text === 'c') { props.onInterrupt(); return }
    if (props.quitting) props.onQuitDismiss?.()
    const inputMenu = matchesFor(composer.value, composer.position)
    const choices = inputMenu?.entries
    if (key.escape) {
      if (choices !== undefined) { updateMenu('', true); return }
      props.onCancel(); return
    }
    if (interaction !== undefined || props.inputBlocked === true || props.inspection !== undefined || composer.blocked || key.meta) return
    if (key.ctrl && text === 'g' && (props.subagents?.length ?? 0) > 0) { props.onSubagents?.(); return }
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
      composer.recall(key.upArrow ? 'older' : 'newer'); updateMenu('', true); return
    }
    if (key.shift && key.return) { composer.paste('\n'); return }
    if (key.return && inputMenu?.kind === 'slash') {
      const choice = choices?.[selectedIndex(choices, composer.value, composer.position)]
      if (choice === undefined) {
        if (composer.value !== '/') composer.type('\n')
        return
      }
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
  // Captured once when the turn starts and held until it ends, so the rule's
  // word and clock do not change with every commit inside the turn.
  const turn = useRef<{ readonly start: number, readonly startedAt: number, readonly word: string } | undefined>(undefined)
  // The turn this surface last watched end, held until the next one starts:
  // what the rule says once there is no turn left to describe.
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
  // A resumed session's newest ended turn, read once when the surface mounts:
  // no clock watched it run, but the log says how it ended and what it did.
  // Once, because reading history again on every commit is the cost §1 rules out.
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
  // Reasoning is drawn by the thinking window over the rule instead: drawn row by
  // row it arrives faster than it can be read and scrolls the surface with it.
  const liveRows = useMemo(() => props.live.filter(row => row.kind !== 'reasoning'), [props.live])
  const thinking = useMemo(() => thinkingRows(props.live, budget.measure, THINKING_ROWS), [props.live, budget])
  const heading = props.inspectionParent === undefined ? `${copy.session}: ${props.sessionId}`
    : `${copy.subagentParent}: ${props.inspectionParent} > ${props.sessionId}`
  // Memoized so the committed transcript is not re-rendered on every frame.
  const result = useMemo<ResultBound>(
    () => ({ lines: props.resultLines, unit: copy.cardLines, single: copy.cardLine, more: copy.moreLines, failures: copy.summaryFailures, ...props.highlight === undefined ? {} : { code: props.highlight } }),
    [props.resultLines, props.highlight, copy])
  // Decided once per mount: a session with no history when it opens gets the
  // block, and keeps it in the stream while its first turn commits.
  const [opening] = useState<Opening | undefined>(() => props.version === undefined
    || props.inspectionParent !== undefined || props.committed.length > 0
    ? undefined
    : { kind: 'welcome', version: props.version, heading: `${copy.session}: ${props.sessionId}` })
  const held = useHeldHeight(props.committed, LEAD_ITEMS, budget, result, repainting)
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
  // opens the interaction instead and is charged here rather than to it.
  const openLimit = claim(interaction !== undefined && budget.chrome.gap ? 1 : 0)
  const interactionLimit = claim(interaction === undefined ? 0 : menuLimit)
  // Claimed before anything but the interaction: it is the answer to a key the
  // user has already pressed, and the next press ends the session.
  const quitLimit = claim(props.quitting ? 1 : 0)
  const sendingLimit = claim(composer.submitting ? 1 : 0)
  const menuStatusRows = matches === undefined ? 0
    : Number(matches.length === 0 && !visibleMenu?.loading)
      + Number(visibleMenu?.loading === true) + Number(visibleMenu?.error !== undefined)
  const completionLimit = claim(matches === undefined ? 0 : Math.max(1,
    Math.min(matches.length, props.completionLimit) + Number(matches.length > props.completionLimit) + menuStatusRows))
  // Only while it has rows to draw: an empty live region draws nothing, and
  // reserving its window for a turn that has not spoken yet would starve the
  // panels below it of rows it never uses.
  const liveLimit = claim(liveRows.length > 0 ? liveWant : 0)
  // After the output it summarizes, which it cannot outrank on a short
  // terminal; the rule already says the turn is running.
  const thinkingLimit = claim(interaction !== undefined || !running ? 0 : thinking.length)
  const taskLimit = claim(props.todos === undefined ? 0 : menuLimit)
  const subagentLimit = claim((props.subagents?.length ?? 0) === 0 ? 0 : Math.min(menuLimit, (props.subagents?.length ?? 0) + 2))
  const pendingLimit = claim(props.pending.length === 0 ? 0 : menuLimit)
  const attachmentLimit = claim((props.attachments?.length ?? 0) === 0 ? 0 : menuLimit)
  // One row, and the command itself may be long enough to wrap past it.
  // Compaction's progress is the rule's to show, in place of the command.
  const commandLimit = claim(props.compactPhase === undefined && props.command !== undefined ? 1 : 0)
  const noticeLimit = claim(props.notice === undefined ? 0 : budget.notice)
  const menuRows = Math.max(0, completionLimit - menuStatusRows)
  const menuWindow = selectionWindow(matches ?? [], selected, menuRows, props.completionLimit)
  const visibleMatches = menuWindow.shown
  const mixedKinds = new Set(visibleMatches.map(entry => entry.kind)).size > 1
  // Everything between the conversation and the input is one stack, opened by
  // the chrome's blank row, so a notice or a list reads as part of the input
  // rather than as one more line of the answer above it.
  const hit = props.usage === undefined ? undefined : cacheHit(props.usage)
  // What the rule over the input says: compaction while it runs, else the
  // turn while it runs, else how the last one ended.
  const light: RuleState | undefined = props.compactPhase !== undefined
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
  const panels = <>
    {turn.current !== undefined && interaction === undefined && <Thinking rows={thinking} limit={thinkingLimit} />}
    {props.todos !== undefined && <Tasks todos={props.todos} copy={copy} limit={taskLimit} />}
    <Subagents entries={props.subagents ?? []} copy={copy} limit={subagentLimit} />
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
          // The kind is dropped when every visible row shares one: nine rows
          // reading `Command` say nothing that the panel itself does not.
          description: [mixedKinds ? copy[entry.kind] : '', entry.description].filter(part => part !== '').join('  '),
        }))}
        selected={menuWindow.selected}
        hidden={menuRows > 1 ? menuWindow.hidden : 0}
        more={`+${menuWindow.hidden} ${copy.moreMatches}`}
      />}
      {matches.length === 0 && !visibleMenu?.loading && <Status text={visibleMenu?.kind === 'file' ? copy.noFiles : copy.noCompletions} />}
      {visibleMenu?.loading === true && <Status text={visibleMenu?.kind === 'file' ? copy.filesLoading : copy.catalogLoading} />}
      {visibleMenu?.error !== undefined && <Status text={`${visibleMenu?.kind === 'file' ? copy.filesError : copy.catalogError}: ${visibleMenu.error}`} tone="error" />}
    </Box>}
  </>
  if (props.inspection !== undefined) {
    const child = props.inspection
    const { usage: _usage, plan: _plan, permission: _permission, thinkingLevel: _thinkingLevel,
      compactPhase: _compactPhase, ...childProps } = props
    return <SessionView {...childProps} {...child} key={child.sessionId} inspection={undefined}
      inspectionParent={props.sessionId} inputBlocked={true} stopping={false}
      pending={[]} todos={undefined} subagents={[]} attachments={[]} context={undefined}
      interaction={undefined} command={undefined}
      notice={`${child.label} · ${copy.subagentBack}`} />
  }
  return <Box flexDirection="column">
    <CommittedTranscript transcript={props.committed} heading={heading} opening={opening} budget={budget} result={result}
      copy={copy} frame={props.frame} columns={size.columns} />
    {/* Above the controls, so the one overflowing frame leaves them at the
        bottom where the replayed history is about to put them. */}
    {repainting && <Box height={size.rows} flexShrink={0} />}
    {/* One beat for everything that moves below: the rule and the
        markers of running actions redraw together, and only when they change. */}
    <Beat clock={clock}>
      {/* Sized to its content or the height it holds, whichever is taller, so
          the composer stays on the bottom row the runner started it on.
          Capped because the claims above are what keep Ink off its
          screen-clearing path, and this is the guard if one of them is wrong. */}
      <Box ref={held.frame} flexDirection="column" flexShrink={0} minHeight={held.floor} maxHeight={budget.dynamic} overflowY="hidden">
        <LiveRegion rows={liveRows} budget={budget} limit={liveLimit} result={result} clock={animate} />
        {/* The held rows: under the output, so what streams stays against the
            history it continues, and over the controls, which stay together. */}
        <Box flexGrow={1} />
        {interaction === undefined
          ? (
            <Chrome
              // No state word: the rule says what the session is doing,
              // and the composer's placeholder and hint say whether it is idle.
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
                // In priority order, since narrowing drops them from the end:
                // occupancy is what a user compacts on, the totals are what the
                // session cost. The path is last because it is the unbounded
                // field: the one the status line shortens, keeping its tail,
                // which names the workspace.
                ...props.context === undefined ? [] : [{ text: `${copy.context}: ${formatContext(props.context)}` }],
                ...props.usage === undefined ? [] : formatTotals(props.usage, { input: copy.tokensIn, output: copy.tokensOut }),
                ...hit === undefined ? [] : [{ label: copy.cacheHit, value: `${hit}%`, color: cacheTone(hit) }],
                compactPath(props.cwd, process.env['HOME']),
              ]}
              columns={size.columns}
              state={{ running: props.inspectionParent === undefined && props.status === 'running', asking: false, listing: matches !== undefined }}
              before={composer.before}
              after={composer.after}
              // Idle invites a prompt. While a turn runs, Enter steers instead of
              // sending, and the placeholder is the only text that says so; while
              // a session switch holds the input, it says why keys do nothing.
              placeholder={props.inspectionParent !== undefined ? copy.subagentBack : props.inputBlocked === true ? copy.sessionsBusy : props.compactPhase !== undefined ? copy.compactWait
                : props.status === 'running' ? copy.steering : copy.prompt}
              // The panel carries no key help of its own, so the slot names the one key
              // that is not discoverable by pressing it.
              hints={{ send: copy.send, interrupt: copy.interrupt, select: copy.tabCompletes, answer: copy.send }}
              maxRows={budget.composer}
              layout={budget.chrome}
              frame={props.frame}
              light={light}
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
      </Box>
    </Beat>
  </Box>
}
