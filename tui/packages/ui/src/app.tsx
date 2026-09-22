/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, Static, Text, useInput, usePaste, useWindowSize } from 'ink'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import { formatContext, type ContextUsage } from './format.ts'
import { useComposer, type Submit } from './composer.ts'
import { completionMenu, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, MARKER, type Budget } from './layout.ts'
import { compactModel, compactPath, present } from './present.ts'
import { Chrome, Completion, Line, LiveRegion, Notice } from './line.tsx'

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
  readonly interaction: Interaction | undefined
  /** The agent's current task list, absent until it writes one. */
  readonly todos: readonly TaskEntry[] | undefined
  readonly model: string
  readonly cwd: string
  readonly sessionId: string
  /** Projected context occupancy from the harness meter, absent before a usage sample. */
  readonly context: ContextUsage | undefined
  readonly copy: TuiCopy
  readonly completion: CompletionCatalog
  readonly files: FileCatalog
  readonly onReferenceQuery: (query: string | undefined) => void
  /** Maximum visible completion and picker rows, supplied by application configuration. */
  readonly completionLimit: number
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
export function RowView({ row, budget }: {
  readonly row: Row
  readonly budget: Budget
}): React.ReactElement {
  return <>{present(row).map((line, index) => <Line key={index} line={line} budget={budget} />)}</>
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
 * @param props.limit - rows the list may draw before it must summarize.
 * @returns the panel, or null when nothing is left to do.
 */
function Tasks({ todos, copy, limit }: {
  readonly todos: readonly TaskEntry[]
  readonly copy: TuiCopy
  readonly limit: number
}): React.ReactElement | null {
  const done = todos.filter(item => item.status === 'completed').length
  const open = todos.filter(item => item.status !== 'completed')
  if (open.length === 0) return null
  const shown = open.slice(0, limit)
  const hidden = open.length - shown.length
  return <Box flexDirection="column">
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

/** Print each committed suffix once; live output and composer updates do not visit history. */
const CommittedTranscript = memo(function CommittedTranscript({ transcript, heading, budget }: { readonly transcript: Transcript; readonly heading: string; readonly budget: Budget }): React.ReactElement {
  const printed = useRef(-1)
  const rows = useMemo(() => {
    const suffix = transcriptRows(transcript, Math.max(0, printed.current))
    return printed.current < 0 ? [{ kind: 'notice' as const, tone: 'info' as const, text: heading }, ...suffix] : suffix
  }, [transcript, heading])
  useLayoutEffect(() => { printed.current = transcript.length }, [transcript])
  return <Static key={transcript.length} items={rows}>
    {(row, index) => <RowView key={index} row={row} budget={budget} />}
  </Static>
})

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
  const start = Math.max(0, selected - props.completionLimit + 1)
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
  const budget = budgetFor(size)
  // Configuration asks for a number of rows; the terminal decides whether they
  // exist. A panel past the viewport makes Ink clear the screen and replay the
  // whole transcript on every keystroke.
  const menuLimit = Math.min(props.completionLimit, budget.items)
  // An open question outranks the live region: a user deciding whether to allow
  // `rm -rf` needs the command and the choices, not concurrent token streaming.
  // Collapsing to one line also keeps the reservation below the interaction
  // affordable on a short window, where the two together would not fit.
  const liveLimit = interaction === undefined ? budget.live : 1
  // Reserved while running, released at rest: the calm state stays compact and
  // the busy state stays still, and the one transition happens at a moment the
  // user already expects the display to change.
  const liveHold = props.status === 'running' ? liveLimit : undefined
  const visibleMatches = matches?.slice(start, start + menuLimit) ?? []
  const mixedKinds = new Set(visibleMatches.map(entry => entry.kind)).size > 1
  return <Box flexDirection="column">
    <CommittedTranscript transcript={props.committed} heading={`${copy.session}: ${props.sessionId}`} budget={budget} />
    <LiveRegion rows={props.live} budget={budget} limit={liveLimit} hold={liveHold} />
    {props.todos !== undefined && <Tasks todos={props.todos} copy={copy} limit={menuLimit} />}
    {props.pending.length > 0 && <Box flexDirection="column">
      <Text color="yellow">{copy.pending}</Text>
      {props.pending.map(message => <Text key={message.id} dimColor>{message.target === 'next-step' ? copy.nextStep : copy.nextTurn}: {[message.text, ...(message.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n')}</Text>)}
      <Text dimColor>{copy.pendingHelp}</Text>
    </Box>}
    {(props.attachments?.length ?? 0) > 0 && <Box flexDirection="column">
      <Text color="cyan">{copy.attachmentsTitle}{': '}{props.attachments!.length}</Text>
      {props.attachments!.slice(0, props.completionLimit).map((item, index) => <Text key={index} dimColor>{index + 1}. {formatAttachment(item)}</Text>)}
      <Text dimColor>{copy.attachmentsHelp}</Text>
    </Box>}
    {composer.submitting && <Text color="yellow">{copy.attachmentsSending}</Text>}
    {interaction !== undefined && <InteractionView key={interaction.id} interaction={interaction} copy={copy} limit={props.completionLimit} onAnswer={props.onAnswer} />}
    {props.command !== undefined && <Text>{copy.command}: {props.command}</Text>}
    {props.notice !== undefined && <Notice text={props.notice} limit={budget.notice} more={copy.moreLines} />}
    {interaction === undefined && <Chrome
      left={[status, compactModel(props.model)]}
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
    />}
    {/* Below the composer, never above it: matches change on every keystroke, and
        a panel above the input would move the line being typed. */}
    {matches !== undefined && interaction === undefined && <Box flexDirection="column">
      <Completion
        items={visibleMatches.map(entry => ({
          name: entry.name,
          // The kind is dropped when every visible row shares one: nine rows
          // reading `Command` say nothing that the panel itself does not.
          description: [mixedKinds ? copy[entry.kind] : '', entry.description].filter(part => part !== '').join('  '),
        }))}
        selected={selected - start}
        hidden={Math.max(0, matches.length - (start + menuLimit))}
        more={`+${Math.max(0, matches.length - (start + menuLimit))} ${copy.moreMatches}`}
      />
      {matches.length === 0 && !visibleMenu?.loading && <Status text={visibleMenu?.kind === 'file' ? copy.noFiles : copy.noCompletions} />}
      {visibleMenu?.loading === true && <Status text={visibleMenu?.kind === 'file' ? copy.filesLoading : copy.catalogLoading} />}
      {visibleMenu?.error !== undefined && <Status text={`${visibleMenu?.kind === 'file' ? copy.filesError : copy.catalogError}: ${visibleMenu.error}`} tone="error" />}
    </Box>}
  </Box>
}
