/** Terminal view over committed history, live presentation, and harness-owned state. */
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, Static, Text, useInput, usePaste, useWindowSize } from 'ink'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import { formatAttachment, type AttachmentSummary, type Row } from './rows.ts'
import { transcriptRows, type Transcript } from './transcript.ts'
import type { TuiCopy } from './copy.ts'
import { formatContext, type ContextUsage } from './format.ts'
import { useComposer, type Submit } from './composer.ts'
import { completionMenu, type CompletionCatalog, type CompletionChoice, type FileCatalog } from './completion.ts'
import { inputHistory } from './history.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'
import { budgetFor, type Budget } from './layout.ts'
import { compactModel, compactPath, present } from './present.ts'
import { Chrome, Completion, Line } from './line.tsx'

/** Display-only projection of one pending inbox message. */
export interface PendingInput {
  readonly id: string
  readonly target: 'next-step' | 'next-turn'
  readonly text: string
  readonly attachments?: readonly AttachmentSummary[]
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

/** @deprecated Superseded by RowView; kept until the snapshot fixtures are re-recorded. */
export function LegacyRowView({ row }: { readonly row: Row }): React.ReactElement {
  switch (row.kind) {
    case 'tool-call': return <Text color="yellow">{'⚙ '}{row.tool}({row.input})</Text>
    case 'tool-result': return <Text color={row.ok ? 'gray' : 'red'}>{'← '}{row.text}</Text>
    case 'notice': return <Text color={row.tone === 'error' ? 'red' : row.tone === 'warn' ? 'yellow' : 'cyan'}>{row.tone === 'info' ? '· ' : '! '}{row.text}</Text>
    case 'user': return <Text color="cyan">{'> '}{[row.text, ...(row.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n')}</Text>
    case 'reasoning': return <Text dimColor>{'· '}{row.text}</Text>
    case 'assistant': return <Text>{'  '}{row.text}</Text>
    default: return assertNever(row)
  }
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
  return <Box flexDirection="column">
    <CommittedTranscript transcript={props.committed} heading={`${copy.session}: ${props.sessionId}`} budget={budget} />
    {props.live.map((row, index) => <RowView key={index} row={row} budget={budget} />)}
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
    {props.notice !== undefined && <Text color="yellow">{props.notice}</Text>}
    {matches !== undefined && <Box flexDirection="column">
      <Text dimColor>{visibleMenu?.kind === 'file' ? copy.filesTitle : copy.completionTitle}</Text>
      <Completion
        items={matches.slice(start, start + props.completionLimit).map(entry => ({
          name: entry.name,
          description: entry.description === '' ? copy[entry.kind] : `${copy[entry.kind]}  ${entry.description}`,
        }))}
        selected={selected - start}
        hidden={0}
        more=""
      />
      {matches.length === 0 && !visibleMenu?.loading && <Text dimColor>{visibleMenu?.kind === 'file' ? copy.noFiles : copy.noCompletions}</Text>}
      {visibleMenu?.loading && <Text dimColor>{visibleMenu?.kind === 'file' ? copy.filesLoading : copy.catalogLoading}</Text>}
      {visibleMenu?.error !== undefined && <Text color="yellow">{visibleMenu?.kind === 'file' ? copy.filesError : copy.catalogError}{': '}{visibleMenu.error}</Text>}
      <Text dimColor>{copy.completionHelp}{' · '}{props.status === 'running' ? copy.steering : copy.send}{matches.length === 0 ? '' : ` · ${selected + 1}/${matches.length}`}</Text>
    </Box>}
    {interaction === undefined && <Chrome
      left={[status, compactModel(props.model), compactPath(props.cwd, process.env['HOME'])]}
      right={props.context === undefined ? [] : [`${copy.context}: ${formatContext(props.context)}`]}
      columns={size.columns}
      color={props.stopping || props.inputBlocked === true ? 'red' : props.status === 'running' ? 'yellow' : 'green'}
      state={{ running: props.status === 'running', asking: false, listing: matches !== undefined }}
      before={composer.before}
      after={composer.after}
      placeholder={copy.help}
      // No hint while the menu is open: it prints the same keys above, with a
      // position counter the slot has no room for.
      hints={{ send: copy.send, interrupt: copy.stopping, select: '', answer: copy.send }}
    />}
  </Box>
}
