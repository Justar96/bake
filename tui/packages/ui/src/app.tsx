/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, Static, Text, useInput, useIsScreenReaderEnabled, usePaste, useWindowSize } from 'ink'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import { formatContext, type ContextUsage } from './format.ts'
import { useComposer, type Submit } from './composer.ts'
import { completionMenu, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, MARKER, selectionWindow, type Budget, type FrameStyle } from './layout.ts'
import { compactModel, compactPath, present, type ResultBound } from './present.ts'
import { Activity, Chrome, Completion, Line, LiveRegion, Notice, Panel, Summary } from './line.tsx'
import { ACCENT, activityWord, phaseLabel, phaseOf, lastTurn, reasoningTicker, turnSummary, type Clock } from './activity.ts'

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
  /** Harness plan projection; absent when this profile has no plan mode. */
  readonly plan?: { readonly active: boolean; readonly pending: boolean }
  readonly model: string
  readonly cwd: string
  readonly sessionId: string
  /** Projected context occupancy from the harness meter, absent before a usage sample. */
  readonly context: ContextUsage | undefined
  readonly copy: TuiCopy
  /** Border style this terminal can draw, resolved by the application. */
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
   * Time source for the turn header's spinner and elapsed time.
   *
   * Absent, the header draws a resting glyph and no clock: the presentation
   * layer reads no time of its own, so a test or a static preview renders the
   * same frame every run. Ignored while a screen reader is active, which
   * would announce every frame.
   */
  readonly clock?: Clock
  /**
   * Whether the header's glyph cycles, its word glints, and a running action's
   * marker pulses; defaults to true. Off — `NO_COLOR` — the clock still counts
   * the elapsed seconds, and nothing else on the surface moves by itself.
   */
  readonly motion?: boolean
  readonly onSubmit: Submit
  readonly onCancel: () => void
  readonly onInterrupt: () => void
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
    <Text dimColor={tone === undefined} {...tone === 'error' ? { color: 'red' as const } : {}}>{text}</Text>
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
  return <>{present(row, result).map((line, index) => <Line key={index} line={line} budget={budget} />)}</>
}

/**
 * The agent's task list, as current state rather than as history.
 *
 * Every write replaces the list, so the transcript would show the same plan
 * several times with a different tick each time. A panel shows the one version
 * that is still true, and costs its rows only while a list exists.
 *
 * Finished work collapses into the header count: it is what the reader has
 * already watched happen, and the rows are needed by the work that has not.
 * The list is capped like every other panel, because the dynamic region shares
 * one budget and a long plan would spend the live region's share of it.
 *
 * @param props.todos - the current list, in the agent's own order.
 * @param props.copy - locale-owned labels.
 * @param props.limit - rows the panel may draw, header and footer included.
 * @returns the panel, or null when nothing is left to do or there is no room.
 */
function Tasks({ todos, copy, limit }: {
  readonly todos: readonly TaskEntry[]
  readonly copy: TuiCopy
  readonly limit: number
}): React.ReactElement | null {
  const done = todos.filter(item => item.status === 'completed').length
  const open = todos.filter(item => item.status !== 'completed')
  if (open.length === 0 || limit <= 0) return null
  // The header and the overflow footer are rows of the panel, not extras on
  // top of it: counting only the entries makes every claim against this panel
  // two rows short, and two rows is the whole chrome.
  const shown = open.slice(0, Math.max(0, limit - 2))
  const hidden = open.length - shown.length
  return <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
    <Text dimColor>{copy.todoTitle} {done}/{todos.length}</Text>
    {shown.map((item, index) => <Box key={index} flexDirection="row">
      <Box width={2} flexShrink={0}>
        <Text {...item.status === 'in_progress' ? { color: 'cyan' as const } : {}}>
          {item.status === 'in_progress' ? MARKER.selected : MARKER.none}
        </Text>
      </Box>
      <Text dimColor={item.status !== 'in_progress'}>{item.text}</Text>
    </Box>)}
    {hidden > 0 && <Text dimColor>  +{hidden} {copy.todoPending}</Text>}
  </Box>
}

/** Print each suffix once, retaining Ink's accumulated scrollback across resize. */
const CommittedTranscript = memo(function CommittedTranscript({ transcript, heading, budget, result }: {
  readonly transcript: Transcript
  readonly heading: string
  readonly budget: Budget
  readonly result: ResultBound
}): React.ReactElement {
  // Ink 7 Static consumes only length and slice(index). Adapt the persistent
  // transcript at this boundary so appends do not copy its entire prefix.
  // Static must keep its identity: remounting clears Ink's saved history,
  // leaving only the latest suffix when a terminal resize requires a replay.
  const items = useMemo(() => ({
    length: transcript.length + 1,
    slice(start = 0): Row[] {
      const suffix = transcriptRows(transcript, Math.max(0, start - 1))
      return start === 0 ? [{ kind: 'notice', tone: 'info', text: heading }, ...suffix] : suffix
    },
  }) as Row[], [transcript, heading])
  return <Static items={items}>
    {(row, index) => <RowView key={index} row={row} budget={budget} result={result} />}
  </Static>
})

/**
 * Whether the terminal has just become narrower than the last painted frame.
 *
 * Ink erases the previous frame by counting its lines, but a terminal that
 * reflows on resize has already re-wrapped each full-width row of it — the
 * composer's borders — onto two, so a narrowing leaves those rows on screen.
 * Ink clears the terminal and replays history only for a frame that overflows
 * the viewport; the caller overflows for the one frame this returns true, and
 * the frame after it, overflowing no longer, is cleared and replayed too.
 * @param columns - current terminal width.
 * @returns true for the single render following a narrowing.
 */
function useNarrowed(columns: number): boolean {
  const painted = useRef(columns)
  const [, repaint] = useState(0)
  const narrowed = columns < painted.current
  useLayoutEffect(() => {
    painted.current = columns
    if (narrowed) repaint(count => count + 1)
  })
  return narrowed
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
  const matchesFor = (draft: string, cursor: number) => interaction !== undefined || props.inputBlocked === true || composer.blocked
    || (samePosition(draft, cursor) && currentMenu.current.dismissed)
    ? undefined : completionMenu(props.completion, props.files, draft, cursor)
  const selectedIndex = (matches: readonly CompletionChoice[], draft: string, cursor: number): number =>
    samePosition(draft, cursor) ? Math.max(0, matches.findIndex(item => item.name === currentMenu.current.selected)) : 0
  const visibleMenu = matchesFor(composer.text, composer.cursor)
  const matches = visibleMenu?.entries
  const query = visibleMenu?.query
  useEffect(() => { props.onReferenceQuery(query) }, [props.onReferenceQuery, query])
  const selected = matches === undefined ? 0 : selectedIndex(matches, composer.text, composer.cursor)
  usePaste(composer.paste, { isActive: interaction === undefined && props.inputBlocked !== true && !composer.submitting })
  useInput((text, key) => {
    if (key.ctrl && text === 'c') { props.onInterrupt(); return }
    const choices = matchesFor(composer.value, composer.position)?.entries
    if (key.escape) {
      if (choices !== undefined) { updateMenu('', true); return }
      props.onCancel(); return
    }
    if (interaction !== undefined || props.inputBlocked === true || composer.blocked || key.meta) return
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
  const status = props.inputBlocked === true ? copy.sessionsBusy : props.stopping ? copy.stopping : props.status === 'running' ? copy.working : copy.ready
  const size = useWindowSize()
  const budget = useMemo(() => budgetFor(size), [size.columns, size.rows])
  const narrowed = useNarrowed(size.columns)
  const screenReader = useIsScreenReaderEnabled()
  const clock = screenReader ? undefined : props.clock
  const running = props.status === 'running'
  const animate = props.motion === false ? undefined : clock
  // Captured once when the turn starts and held until it ends, so the header's
  // word and clock do not change with every commit inside the turn.
  const turn = useRef<{ readonly start: number, readonly startedAt: number, readonly word: string } | undefined>(undefined)
  // The turn this surface last watched end, held until the next one starts:
  // what the header's row says once there is no turn left to describe.
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
  // Reasoning is summarized by the header's ticker instead: drawn row by row
  // it arrives faster than it can be read and scrolls the surface with it.
  const liveRows = useMemo(() => props.live.filter(row => row.kind !== 'reasoning'), [props.live])
  const ticker = reasoningTicker(props.live)
  const heading = `${copy.session}: ${props.sessionId}`
  // Memoized so the committed transcript is not re-rendered on every frame.
  const result = useMemo<ResultBound>(
    () => ({ lines: props.resultLines, unit: copy.cardLines, more: copy.moreLines }), [props.resultLines, copy])
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
  // After the output it describes, which it cannot outrank on a short
  // terminal: the status line already says a turn is running. Before the
  // panels, because it is the one row that says the turn is still alive.
  // Once the turn ends, the same row holds its summary until the next begins.
  const activityLimit = claim(interaction !== undefined ? 0 : running ? ticker === undefined ? 1 : 2 : summary === undefined ? 0 : 1)
  const taskLimit = claim(props.todos === undefined ? 0 : menuLimit)
  const pendingLimit = claim(props.pending.length === 0 ? 0 : menuLimit)
  const attachmentLimit = claim((props.attachments?.length ?? 0) === 0 ? 0 : menuLimit)
  // One row, and the command itself may be long enough to wrap past it.
  const commandLimit = claim(props.command === undefined ? 0 : 1)
  const noticeLimit = claim(props.notice === undefined ? 0 : budget.notice)
  const menuRows = Math.max(0, completionLimit - menuStatusRows)
  const menuWindow = selectionWindow(matches ?? [], selected, menuRows, props.completionLimit)
  const visibleMatches = menuWindow.shown
  const mixedKinds = new Set(visibleMatches.map(entry => entry.kind)).size > 1
  // Everything between the conversation and the input is one stack, opened by
  // the chrome's blank row, so a notice or a list reads as part of the input
  // rather than as one more line of the answer above it.
  const panels = <>
    {turn.current !== undefined && interaction === undefined && <Activity
      word={props.stopping ? copy.stopping : turn.current.word}
      phase={props.stopping ? undefined : phaseLabel(phaseOf(props.live, lastCommitted), copy)}
      ticker={ticker}
      startedAt={turn.current.startedAt}
      clock={clock}
      motion={animate !== undefined}
      color={props.stopping ? 'red' : ACCENT.base}
      limit={activityLimit}
    />}
    {turn.current === undefined && summary !== undefined && interaction === undefined && <Summary summary={summary} limit={activityLimit} />}
    {props.todos !== undefined && <Tasks todos={props.todos} copy={copy} limit={taskLimit} />}
    <Panel
      title={copy.pending} color="yellow" limit={pendingLimit} more={copy.moreLines}
      items={props.pending.map(message => `${message.target === 'next-step' ? copy.nextStep : copy.nextTurn}: ${[message.text, ...(message.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n')}`)}
      footer={copy.pendingHelp}
    />
    <Panel
      title={`${copy.attachmentsTitle}: ${props.attachments?.length ?? 0}`} color="cyan"
      limit={attachmentLimit} more={copy.moreLines}
      items={(props.attachments ?? []).map((item, index) => `${index + 1}. ${formatAttachment(item)}`)}
      footer={copy.attachmentsHelp}
    />
    {composer.submitting && sendingLimit > 0 && <Text color="yellow" wrap="truncate-end">{copy.attachmentsSending}</Text>}
    {interaction !== undefined && <InteractionView key={interaction.id} interaction={interaction} copy={copy} limit={interactionLimit} onAnswer={props.onAnswer} />}
    {props.command !== undefined && commandLimit > 0 && <Box flexShrink={0} maxHeight={commandLimit} overflowY="hidden">
      <Text wrap="truncate-end">{copy.command}: {props.command}</Text>
    </Box>}
    {props.notice !== undefined && <Notice text={props.notice} limit={noticeLimit} more={copy.moreLines} />}
    {props.quitting && quitLimit > 0 && <Text color="yellow" wrap="truncate-end">{copy.quit}</Text>}
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
  return <Box flexDirection="column">
    <CommittedTranscript transcript={props.committed} heading={heading} budget={budget} result={result} />
    {/* Above the controls, so the one overflowing frame leaves them at the
        bottom where the replayed history is about to put them. */}
    {narrowed && <Box height={size.rows} flexShrink={0} />}
    {/* Sized to its content, so the input sits directly under the newest line
        until the screen fills; the terminal then scrolls history above it.
        Capped because the claims above are what keep Ink off its
        screen-clearing path, and this is the guard if one of them is wrong. */}
    <Box flexDirection="column" flexShrink={0} maxHeight={budget.dynamic} overflowY="hidden">
      <LiveRegion rows={liveRows} budget={budget} limit={liveLimit} result={result} clock={animate} />
      {interaction === undefined
        ? (
          <Chrome
            left={[status,
              ...props.plan === undefined || (!props.plan.active && !props.plan.pending) ? []
                : [props.plan.pending ? (props.plan.active ? copy.planExitPending : copy.planEntryPending) : copy.planActive],
              compactModel(props.model)]}
            right={[
              // The path is last because it is the unbounded field: it is the one the
              // status line shortens, and it keeps its tail, which names the workspace.
              ...props.context === undefined ? [] : [`${copy.context}: ${formatContext(props.context)}`],
              compactPath(props.cwd, process.env['HOME']),
            ]}
            columns={size.columns}
            color={props.stopping || props.inputBlocked === true ? 'red' : props.status === 'running' ? 'yellow' : 'green'}
            state={{ running: props.status === 'running', asking: false, listing: matches !== undefined }}
            before={composer.before}
            after={composer.after}
            // Idle invites a prompt. While a turn runs, Enter steers instead of
            // sending, and the placeholder is the only text that says so.
            placeholder={props.status === 'running' ? copy.steering : copy.prompt}
            // The panel carries no key help of its own, so the slot names the one key
            // that is not discoverable by pressing it.
            hints={{ send: copy.send, interrupt: copy.interrupt, select: copy.tabCompletes, answer: copy.send }}
            maxRows={budget.composer}
            layout={budget.chrome}
            frame={props.frame}
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
  </Box>
}
