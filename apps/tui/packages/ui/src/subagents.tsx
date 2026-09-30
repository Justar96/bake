/** Connected child identities and activity come from the application; their row and sheet are drawn here. */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { ICON } from './icons.ts'
import { COLUMN, HINT_MIN_COLUMNS, MARKER } from './layout.ts'
import { agentTone, PALETTE, type PaletteColor } from './palette.ts'
import { sheetBar, type SheetLine } from './sheet.tsx'

export interface SubagentEntry {
  readonly id: string
  readonly label: string
  readonly state: 'working' | 'live' | 'saved' | 'issue'
  /** Latest recorded child turn outcome; absent during a new run or without a terminal record. */
  readonly outcome?: 'completed' | 'failed' | 'stopped'
  readonly detail: string
  readonly inspectable: boolean
  /** Workflow name for a recorded member; absent for direct delegation. */
  readonly workflow?: string
}

/** Display fields derived from recorded workflow runs and the owning agent's activity. */
export interface WorkflowEntry {
  readonly id: string
  readonly name: string
  readonly state: 'working' | 'unfinished' | 'completed' | 'failed' | 'stopped'
  readonly total: number
  readonly completed: number
}

/** Keep active orchestration visible even before it publishes its first child. */
function delegationTitle(entries: readonly SubagentEntry[], workflows: readonly WorkflowEntry[], copy: TuiCopy): string {
  const active = workflows.filter(run => run.state === 'working')
  if (active.length === 1) return `${copy.workflowTitle} ${active[0]!.name}`
  if (active.length > 1 || entries.length === 0 && workflows.length > 0) return `${copy.workflowsTitle} ${workflows.length}`
  return `${copy.subagentsTitle} ${entries.length}`
}

function workflowStatus(run: WorkflowEntry, copy: TuiCopy): string {
  return copy[run.state === 'working' ? 'subagentWorking' : run.state === 'completed' ? 'subagentCompleted'
    : run.state === 'failed' ? 'subagentFailed' : run.state === 'stopped' ? 'subagentStopped' : 'workflowUnfinished']
}

const STATE = {
  working: 'subagentWorking', live: 'subagentLive', saved: 'subagentSaved', issue: 'subagentIssue',
} as const satisfies Record<SubagentEntry['state'], keyof TuiCopy>

/** Keep the recorded outcome separate from whether the session is resident. */
export function subagentStatus(entry: SubagentEntry, copy: TuiCopy): string {
  const activity = copy[STATE[entry.state]]
  if (entry.state === 'working' || entry.outcome === undefined) return activity
  return `${copy[entry.outcome === 'completed' ? 'subagentCompleted' : entry.outcome === 'failed' ? 'subagentFailed' : 'subagentStopped']} · ${activity}`
}

/** Each activity's shape, so `NO_COLOR` still tells a working child from one at rest. */
const GLYPH = {
  working: { glyph: MARKER.turn, color: PALETTE.running },
  live: { glyph: MARKER.turn, color: PALETTE.asking },
  saved: { glyph: MARKER.waiting },
  issue: { glyph: '✗', color: PALETTE.failed },
} as const satisfies Record<SubagentEntry['state'], { readonly glyph: string, readonly color?: PaletteColor }>

/** The colour a child's status is read in: its outcome once there is one, else its activity. */
function statusColor(entry: SubagentEntry): PaletteColor | undefined {
  if (entry.state === 'working') return PALETTE.running
  if (entry.state === 'issue' || entry.outcome === 'failed') return PALETTE.failed
  if (entry.outcome === 'completed') return PALETTE.done
  return entry.outcome === 'stopped' ? PALETTE.waiting : undefined
}

/** How the children divide by what they are doing or how they ended; each child is in exactly one. */
export interface SubagentCounts {
  readonly working: number
  /** Finished with a completed turn. */
  readonly done: number
  readonly failed: number
  readonly stopped: number
  readonly unreadable: number
}

/** Count children by their state, a child that is working being counted only as working. */
export function subagentCounts(entries: readonly SubagentEntry[]): SubagentCounts {
  const counts = { working: 0, done: 0, failed: 0, stopped: 0, unreadable: 0 }
  for (const entry of entries) {
    if (entry.state === 'working') counts.working++
    else if (entry.state === 'issue') counts.unreadable++
    else if (entry.outcome === 'completed') counts.done++
    else if (entry.outcome === 'failed') counts.failed++
    else if (entry.outcome === 'stopped') counts.stopped++
  }
  return counts
}

/** The counts a standing row and the tab show, in lowercase: what is working, then what is done. */
function shortCounts(counts: SubagentCounts, copy: TuiCopy): readonly string[] {
  return [counts.working > 0 ? `${counts.working} ${copy.subagentCountWorking}` : '',
    counts.done > 0 ? `${counts.done} ${copy.subagentCountDone}` : ''].filter(Boolean)
}

/** The list's tab: its name, the number of children, and how many are working and done. */
export function subagentTab(entries: readonly SubagentEntry[], copy: TuiCopy, workflows: readonly WorkflowEntry[] = []): string {
  return [delegationTitle(entries, workflows, copy), ...shortCounts(subagentCounts(entries), copy)].join(' · ')
}

/**
 * The session's children as one compact row under the input, above the status line.
 *
 * It sits below the composer because Down from an empty composer selects it.
 * It shares the grammar of every standing row: the transcript's `↳` in the
 * rail, the name and the total, then how many children are working, how many
 * are done, and how many cannot be read, when any are, in lowercase:
 * `↳ Subagents 5 · 2 working · 2 done`.
 * An active workflow replaces the title with its name; member names stay in
 * the sheet. Dim, so it stays supporting material beside the draft.
 * `hint` names the key that opens the sheet at the right edge, given up below
 * {@link HINT_MIN_COLUMNS} as the composer's hint is. Focused, the rail holds
 * `>`, the row is drawn at full strength, and the hint says what Enter does.
 *
 * @param props.entries - the children, in the catalog's order.
 * @param props.workflows - recorded run progress, including runs without children.
 * @param props.columns - row width.
 * @param props.focused - whether arrow-key focus is on the row.
 * @param props.hint - the key that opens the sheet.
 * @returns the row, or null when there are no children or workflows.
 */
export function SubagentRow({ entries, workflows = [], copy, columns, focused = false, hint }: {
  readonly entries: readonly SubagentEntry[]
  readonly workflows?: readonly WorkflowEntry[] | undefined
  readonly copy: TuiCopy
  readonly columns: number
  readonly focused?: boolean
  readonly hint?: string | undefined
}): React.ReactElement | null {
  if (entries.length === 0 && workflows.length === 0 || columns <= 0) return null
  const counts = subagentCounts(entries)
  const summary = [...shortCounts(counts, copy),
    counts.unreadable > 0 ? `${counts.unreadable} ${copy.subagentUnreadable}` : ''].filter(Boolean).join(' · ')
  const rail = Math.min(COLUMN.rail, columns)
  const head = delegationTitle(entries, workflows, copy)
  const tail = focused ? copy.subagentsOpen : hint
  // The key goes before the head or the counts would be cut.
  const text = `${head}${summary === '' ? '' : ` · ${summary}`}`
  const showTail = tail !== undefined && columns >= HINT_MIN_COLUMNS && rail + stringWidth(text) + 2 + stringWidth(tail) <= columns
  return <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
    <Box width={rail} flexShrink={0}><Text bold={focused} dimColor={!focused}>{focused ? '>' : ICON.spawn}</Text></Box>
    <Box flexGrow={1} flexShrink={1}>
      <Text wrap="truncate-end" dimColor={!focused}>
        <Text inverse={focused}>{head}</Text>
        {summary === '' ? '' : ` · ${summary}`}
      </Text>
    </Box>
    {showTail && <Box flexShrink={0} marginLeft={2}><Text dimColor>{tail}</Text></Box>}
  </Box>
}

/**
 * The bar under the input while a child's session is open, in the slot the
 * subagents row holds in the parent: `↳ Review 2/5 · Working · Read-only` at
 * the left and `Esc back to parent` at the right edge.
 *
 * It is drawn at full strength, since it is what tells this view from the
 * parent's: the rail glyph and name take the child's identity tone from the
 * sheet, and the status its outcome's colour. The way back is the one part
 * that never gives way. On a narrow row the name and status are cut from the
 * end, and below the width of the key alone the key is cut instead, so the
 * row is one line at any width. The hint is not hidden below
 * {@link HINT_MIN_COLUMNS} as other standing rows' keys are: here it is the
 * only visible way out.
 *
 * @param props.label - the child's name, from the sheet's entry when there is one.
 * @param props.entries - the parent's children, in the catalog's order.
 * @param props.id - the open child's session id.
 * @param props.working - whether the child's agent is running, for a child the catalog does not list.
 * @param props.columns - row width.
 * @returns the row, or null when there is no width.
 */
export function InspectionBar({ label, entries, id, working, copy, columns }: {
  readonly label: string
  readonly entries: readonly SubagentEntry[]
  readonly id: string
  readonly working: boolean
  readonly copy: TuiCopy
  readonly columns: number
}): React.ReactElement | null {
  if (columns <= 0) return null
  const index = entries.findIndex(entry => entry.id === id)
  const entry = index < 0 ? undefined : entries[index]
  const tone = agentTone(Math.max(0, index))
  const status = entry === undefined ? working ? copy.subagentWorking : undefined : subagentStatus(entry, copy)
  const color = entry === undefined ? working ? PALETTE.running : undefined : statusColor(entry)
  const rail = Math.min(COLUMN.rail, columns)
  const key = copy.subagentBackKey
  const keyWidth = stringWidth(key)
  const showLead = rail + keyWidth + 2 <= columns
  return <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
    {showLead && <Box width={rail} flexShrink={0}><Text bold color={tone}>{ICON.spawn}</Text></Box>}
    {showLead && <Box flexGrow={1} flexShrink={1}>
      <Text wrap="truncate-end">
        <Text bold color={tone}>{label}</Text>
        {index < 0 ? '' : <Text dimColor>{` ${index + 1}/${entries.length}`}</Text>}
        {status === undefined ? '' : <Text {...color === undefined ? { dimColor: true } : { color }}>{` · ${status}`}</Text>}
        <Text dimColor>{` · ${copy.subagentReadOnly}`}</Text>
      </Text>
    </Box>}
    <Box flexShrink={0} marginLeft={showLead ? 2 : 0}><Text bold wrap="truncate-end">{key}</Text></Box>
  </Box>
}

/** Lines before the first child in {@link subagentSheet}: the summary and the instruction. */
const SUBAGENT_LEAD = 3

/** Cells of the summary's progress bar. */
const SUBAGENT_BAR = 24

/**
 * The logical lines the child under the pointer takes, first and last: its row
 * and the detail line under it. The sheet keeps both in view.
 */
export const subagentLine = (index: number, workflowCount = 0): readonly [number, number] =>
  [SUBAGENT_LEAD + workflowCount + index, SUBAGENT_LEAD + workflowCount + index + 1]

/** The summary's counts, largest concern first: those still working, then how the rest ended. */
function subagentSummary(entries: readonly SubagentEntry[], copy: TuiCopy): string {
  const counts = subagentCounts(entries)
  return [`${counts.done}/${entries.length} ${copy.subagentCountDone}`,
    ...([['working', copy.subagentCountWorking], ['failed', copy.subagentCountFailed], ['stopped', copy.subagentCountStopped],
      ['unreadable', copy.subagentUnreadable]] as const)
      .filter(([key]) => counts[key] > 0).map(([key, word]) => `${counts[key]} ${word}`),
  ].join(' \u00b7 ')
}

/**
 * Every child of the session as a list to choose from, under a bar of how many
 * are done and the counts by state. Each child is one row: its activity's
 * glyph, its name beside its status in that status's colour. Only the child
 * under the pointer takes a second, dim line of how it runs and its id, so a
 * session of many children stays scannable. A child that finished cleanly
 * recedes, dimmed, until the pointer reaches it; one without a local
 * transcript is dimmed too, since Enter cannot open it.
 *
 * @param entries - the children, in the catalog's order.
 * @param selected - index of the child under the pointer.
 */
export function subagentSheet(entries: readonly SubagentEntry[], selected: number, copy: TuiCopy,
  workflows: readonly WorkflowEntry[] = []): readonly SheetLine[] {
  const counts = subagentCounts(entries)
  return [
    { text: '', parts: [...sheetBar(counts.done, entries.length, SUBAGENT_BAR, PALETTE.done),
      { text: `  ${subagentSummary(entries, copy)}`, dim: true }] },
    ...workflows.map((run): SheetLine => ({
      text: `${copy.workflowTitle} ${run.name} · ${workflowStatus(run, copy)} · ${run.completed}/${run.total} ${copy.subagentCountDone}`,
      ...run.state === 'working' ? { color: PALETTE.running } : run.state === 'failed' ? { color: PALETTE.failed } : {},
      dim: run.state === 'completed',
    })),
    { text: entries.length === 0 ? copy.workflowNoChildren : copy.subagentChoose, dim: true },
    { text: '' },
    ...entries.flatMap((entry, index): SheetLine[] => {
      const shape = GLYPH[entry.state]
      const color = statusColor(entry)
      const current = index === selected
      const receded = !current && entry.state !== 'working' && entry.outcome === 'completed'
      return [
        { text: '', selected: current, glyph: shape.glyph, ...'color' in shape ? { glyphColor: shape.color } : {},
          parts: [{ text: entry.label, bold: entry.inspectable && !receded, dim: !entry.inspectable || receded, color: agentTone(index) },
            { text: `  ${subagentStatus(entry, copy)}`, ...color === undefined || receded ? { dim: true } : { color } },
            ...workflows.length === 0 ? [] : [{ text: ` · ${entry.workflow === undefined ? copy.subagentDirect : `${copy.workflowTitle} ${entry.workflow}`}`, dim: true }] ] },
        ...current ? [{ text: `${entry.detail} · ${entry.id}${entry.inspectable ? '' : ` · ${copy.subagentNoTranscript}`}`,
          selected: false, glyph: ' ', dim: true }] : [],
      ]
    }),
  ]
}
