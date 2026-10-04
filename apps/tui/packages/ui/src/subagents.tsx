/** Connected child identities and activity come from the application; their row and sheet are drawn here. */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { ICON } from './icons.ts'
import { COLUMN, MARKER, TREE } from './layout.ts'
import { tailFits } from './line.tsx'
import { agentTone, PALETTE, type PaletteColor } from './palette.ts'
import { SHEET_WIDTH, sheetBar, type SheetLine, type SheetPart } from './sheet.tsx'
import { toolText } from './tool-output.ts'

/** Recorded selection of the child's effective model; supplied by the parent's routing projection. */
export interface SubagentRouting {
  readonly source: 'explicit' | 'default' | 'auto' | 'fallback'
  readonly route?: {
    readonly provider: string
    readonly model: string
    readonly reasoningEffort?: string
  }
  readonly router?: {
    readonly reason: string
    readonly fallback: boolean
    readonly assessment?: {
      readonly policy: string
      readonly status: 'normal' | 'cautious' | 'needs_context' | 'fallback'
      readonly difficulty: number
      readonly reasons: readonly string[]
    }
  }
}

export interface SubagentEntry {
  readonly id: string
  readonly label: string
  readonly state: 'working' | 'live' | 'saved' | 'issue'
  /** Latest recorded child turn outcome; absent during a new run or without a terminal record. */
  readonly outcome?: 'completed' | 'failed' | 'stopped'
  readonly detail: string
  readonly inspectable: boolean
  /** A durable routing decision. Absent for historical children without one. */
  readonly routing?: SubagentRouting
}

const STATE = {
  working: 'subagentWorking', live: 'subagentLive', saved: 'subagentSaved', issue: 'subagentIssue',
} as const satisfies Record<SubagentEntry['state'], keyof TuiCopy>

const OUTCOME = {
  completed: 'subagentCompleted', failed: 'subagentFailed', stopped: 'subagentStopped',
} as const satisfies Record<NonNullable<SubagentEntry['outcome']>, keyof TuiCopy>

/**
 * A child's status in two parts: how it ended, when it has and is not
 * working again, and whether its session is resident.
 */
function statusParts(entry: SubagentEntry, copy: TuiCopy): { readonly outcome?: string, readonly activity: string } {
  const activity = copy[STATE[entry.state]]
  return entry.state === 'working' || entry.outcome === undefined ? { activity } : { outcome: copy[OUTCOME[entry.outcome]], activity }
}

/** Keep the recorded outcome separate from whether the session is resident. */
export function subagentStatus(entry: SubagentEntry, copy: TuiCopy): string {
  const { outcome, activity } = statusParts(entry, copy)
  return outcome === undefined ? activity : `${outcome} · ${activity}`
}

/** A child's shape and its colour, absent for the terminal's dim foreground. */
interface Shape {
  readonly glyph: string
  readonly color?: PaletteColor
}

/**
 * Each child's shape, so `NO_COLOR` still tells a working child from one
 * that finished, failed, or stopped. A child at rest takes its outcome's
 * glyph, the same marks the header gives a finished turn; without one,
 * a resident child is a filled dot and a saved one a hollow dot. An entry
 * the catalog could not read is a question, not a failure.
 */
function shapeOf(entry: SubagentEntry): Shape {
  if (entry.state === 'working') return { glyph: MARKER.turn, color: PALETTE.running }
  if (entry.state === 'issue') return { glyph: '?', color: PALETTE.waiting }
  if (entry.outcome === 'completed') return { glyph: '\u2713', color: PALETTE.done }
  if (entry.outcome === 'failed') return { glyph: '\u2717', color: PALETTE.failed }
  if (entry.outcome === 'stopped') return { glyph: '\u25a0', color: PALETTE.waiting }
  return entry.state === 'live' ? { glyph: MARKER.turn, color: PALETTE.asking } : { glyph: MARKER.waiting }
}

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

/** One count as a row draws it, and whether it is the failure count, drawn red. */
interface Count {
  readonly text: string
  readonly failed?: true
}

/** The counts a standing row and the tab show, in lowercase: what is working, what is done, then what failed. */
function shortCounts(counts: SubagentCounts, copy: TuiCopy): readonly Count[] {
  return [
    ...counts.working > 0 ? [{ text: `${counts.working} ${copy.subagentCountWorking}` }] : [],
    ...counts.done > 0 ? [{ text: `${counts.done} ${copy.subagentCountDone}` }] : [],
    ...counts.failed > 0 ? [{ text: `${counts.failed} ${copy.subagentCountFailed}`, failed: true as const }] : [],
  ]
}

/**
 * The standing row's rail colour: running while any child works, red once
 * one failed with none working, otherwise none, and the row stays dim.
 */
function railColor(counts: SubagentCounts): PaletteColor | undefined {
  return counts.working > 0 ? PALETTE.running : counts.failed > 0 ? PALETTE.failed : undefined
}

/** The list's tab: its name, the number of children, and how many are working, done, and failed. */
export function subagentTab(entries: readonly SubagentEntry[], copy: TuiCopy): string {
  return [`${copy.subagentsTitle} ${entries.length}`, ...shortCounts(subagentCounts(entries), copy).map(count => count.text)].join(' · ')
}

/**
 * The session's children as one compact row under the input, above the status line.
 *
 * It sits below the composer because Down from an empty composer selects it.
 * It shares the grammar of every standing row: the transcript's `↳` in the
 * rail, the name and the total, then how many children are working, how many
 * are done, how many failed, and how many cannot be read, when any are, in
 * lowercase: `↳ Subagents 5 · 2 working · 2 done · 1 failed`.
 * Dim, so it stays supporting material beside the draft. Only the rail says
 * more: it is in the running colour while a child works, and red once one
 * has failed, so the row reads at a glance without its words. The failed
 * count is red too, since it is the one count that asks for a look.
 * `hint` names the key that opens the sheet at the right edge, given up as
 * {@link tailFits} decides, as the composer's hint is. Focused, the rail holds
 * `>`, the row is drawn at full strength, and the hint says what Enter does.
 *
 * @param props.entries - the children, in the catalog's order.
 * @param props.columns - row width.
 * @param props.focused - whether arrow-key focus is on the row.
 * @param props.hint - the key that opens the sheet.
 * @returns the row, or null when there are no children.
 */
export function SubagentRow({ entries, copy, columns, focused = false, hint }: {
  readonly entries: readonly SubagentEntry[]
  readonly copy: TuiCopy
  readonly columns: number
  readonly focused?: boolean
  readonly hint?: string | undefined
}): React.ReactElement | null {
  if (entries.length === 0 || columns <= 0) return null
  const counts = subagentCounts(entries)
  const parts: readonly Count[] = [...shortCounts(counts, copy),
    ...counts.unreadable > 0 ? [{ text: `${counts.unreadable} ${copy.subagentUnreadable}` }] : []]
  const rail = Math.min(COLUMN.rail, columns)
  const head = `${copy.subagentsTitle} ${entries.length}`
  const tail = focused ? copy.subagentsOpen : hint
  // The key goes before the head or the counts would be cut.
  const text = [head, ...parts.map(part => part.text)].join(' · ')
  const showTail = tailFits(columns, rail + stringWidth(text), tail)
  const color = railColor(counts)
  return <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
    <Box width={rail} flexShrink={0}>
      {focused ? <Text bold>{'>'}</Text>
        : color === undefined ? <Text dimColor>{ICON.spawn}</Text> : <Text bold color={color}>{ICON.spawn}</Text>}
    </Box>
    <Box flexGrow={1} flexShrink={1}>
      <Text wrap="truncate-end" dimColor={!focused}>
        <Text inverse={focused}>{head}</Text>
        {parts.map(part => <React.Fragment key={part.text}>
          {' · '}
          {part.failed === true ? <Text color={PALETTE.failed}>{part.text}</Text> : part.text}
        </React.Fragment>)}
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
 * row is one line at any width. The hint is not hidden as other standing
 * rows' keys are by {@link tailFits}: here it is the only visible way out.
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
const SUBAGENT_BAR = 16

/** Widest a name's column grows; a longer name pushes its own status along instead. */
const SUBAGENT_NAME = 24

/**
 * The logical lines the child under the pointer takes, first and last: its row
 * and its detail lines. The sheet keeps them together when they fit; longer
 * routing evidence remains reachable with the sheet's page keys.
 */
export const subagentLine = (index: number, routing?: SubagentRouting): readonly [number, number] => {
  const first = SUBAGENT_LEAD + index
  const detail = routingContent(routing)
  return [first, first + 1 + Number(detail.route !== undefined) + Number(detail.difficulty !== undefined) + detail.notes.length]
}

/** Router and provider strings are data: controls cannot style the sheet or create extra logical rows. */
const routingText = (text: string): string => toolText(text).replace(/\n/g, ' ').trim()

/** Selection provenance stays separate from the child's running/completed state. */
function routingBadge(routing: SubagentRouting, copy: TuiCopy): { readonly text: string, readonly caution: boolean } {
  const status = routing.router?.assessment?.status
  const fallback = routing.source === 'fallback' || routing.router?.fallback === true || status === 'fallback'
  // The recorded action owns provenance; optional router metadata may only qualify it.
  const label = routing.source === 'auto' ? copy.subagentRoutingAuto
    : routing.source === 'explicit' ? copy.subagentRoutingSelected : copy.subagentRoutingDefault
  const note = status === 'needs_context' ? copy.subagentRoutingNeedsContext
    : status === 'cautious' ? copy.subagentRoutingCautious : fallback ? copy.subagentRoutingFallback : undefined
  return { text: note === undefined ? label : `${label} · ${note}`, caution: fallback || note !== undefined }
}

/** The same content determines both the drawn details and the range kept under the sheet's pointer. */
function routingContent(routing: SubagentRouting | undefined): {
  readonly route: SubagentRouting['route']
  readonly difficulty: number | undefined
  readonly notes: readonly string[]
} {
  const assessment = routing?.router?.assessment
  const difficulty = assessment?.difficulty
  return {
    route: routing?.route,
    difficulty: difficulty !== undefined && Number.isFinite(difficulty) && difficulty >= 0 && difficulty <= 1 ? difficulty : undefined,
    notes: [...new Set([routing?.router?.reason ?? '', ...assessment?.reasons ?? []].map(routingText).filter(Boolean))],
  }
}

/** Exact effective route and readable policy evidence for only the child under the pointer. */
function routingDetails(routing: SubagentRouting, copy: TuiCopy): readonly SheetLine[] {
  const content = routingContent(routing)
  const badge = routingBadge(routing, copy)
  return [
    ...content.route === undefined ? [] : [{
      text: `${routingText(content.route.provider)}/${routingText(content.route.model)}`
        + (content.route.reasoningEffort === undefined ? '' : ` · ${copy.think} ${routingText(content.route.reasoningEffort)}`),
      selected: false,
    }],
    ...content.difficulty === undefined ? [] : [{
      text: `${copy.subagentRoutingDifficulty} ${content.difficulty.toFixed(2)} · ${badge.text}`,
      selected: false, ...badge.caution ? { color: PALETTE.waiting } : { dim: true },
    }],
    ...content.notes.map(text => ({ text, selected: false, dim: true })),
  ]
}

/**
 * The summary's counts, largest concern first: how many are done, those
 * still working, then how the rest ended, each in its state's colour.
 */
function subagentSummary(entries: readonly SubagentEntry[], copy: TuiCopy): readonly SheetPart[] {
  const counts = subagentCounts(entries)
  const tinted: readonly SheetPart[] = [
    { text: `${counts.done}/${entries.length} ${copy.subagentCountDone}`, ...counts.done > 0 ? { color: PALETTE.done } : { dim: true } },
    ...([['working', copy.subagentCountWorking, PALETTE.running], ['failed', copy.subagentCountFailed, PALETTE.failed],
      ['stopped', copy.subagentCountStopped, PALETTE.waiting], ['unreadable', copy.subagentUnreadable, undefined]] as const)
      .filter(([key]) => counts[key] > 0)
      .map(([key, word, color]) => ({ text: `${counts[key]} ${word}`, ...color === undefined ? { dim: true } : { color } })),
  ]
  return tinted.flatMap((part, index) => index === 0 ? [part] : [{ text: ' \u00b7 ', dim: true }, part])
}

/**
 * Hang a child's detail lines from its glyph on the tree, as a step's calls
 * hang from their head, so they read as one child's and not as the next row.
 */
function hung(lines: readonly SheetLine[]): readonly SheetLine[] {
  return lines.map((line, index) => ({ ...line, glyph: index === lines.length - 1 ? TREE.corner : TREE.stem }))
}

/**
 * Every child of the session as a list to choose from, under a bar of how many
 * are done and the counts by state, each in its state's colour. Each child is
 * one row: its glyph, which takes its outcome's mark once it has one, its
 * name, and its status in a column of its own, so the statuses of a long list
 * line up and read down as one. The status leads with how the child ended,
 * in that outcome's colour, and then, dim, whether its session is resident.
 * Only the child under the pointer takes detail lines, hung from its glyph on
 * the tree: how it runs and its id, then its route and the router's evidence.
 * A session of many children stays scannable. A child that finished cleanly
 * recedes, dimmed, until the pointer reaches it; one without a local
 * transcript is dimmed too, since Enter cannot open it.
 *
 * @param entries - the children, in the catalog's order.
 * @param selected - index of the child under the pointer.
 * @param columns - terminal width; the name column keeps to a third of the
 *   sheet's, so a narrow sheet's statuses are not pushed off it. Absent, the
 *   widest sheet is assumed.
 */
export function subagentSheet(entries: readonly SubagentEntry[], selected: number, copy: TuiCopy, columns = SHEET_WIDTH): readonly SheetLine[] {
  const counts = subagentCounts(entries)
  // The frame, its padding, the pointer, and the glyph take eight cells.
  const room = Math.floor((Math.min(columns, SHEET_WIDTH) - 8) / 3)
  const column = Math.max(0, Math.min(SUBAGENT_NAME, room, Math.max(0, ...entries.map(entry => stringWidth(entry.label)))))
  return [
    { text: '', parts: [...sheetBar(counts.done, entries.length, SUBAGENT_BAR, PALETTE.done), { text: '  ' }, ...subagentSummary(entries, copy)] },
    { text: copy.subagentChoose, dim: true },
    { text: '' },
    ...entries.flatMap((entry, index): SheetLine[] => {
      const shape = shapeOf(entry)
      const color = statusColor(entry)
      const current = index === selected
      const receded = !current && entry.state !== 'working' && entry.outcome === 'completed'
      const routing = entry.routing === undefined ? undefined : routingBadge(entry.routing, copy)
      const status = statusParts(entry, copy)
      const gap = ' '.repeat(Math.max(0, column - stringWidth(entry.label)) + 2)
      return [
        { text: '', selected: current, glyph: shape.glyph, ...shape.color === undefined ? {} : { glyphColor: shape.color },
          parts: [{ text: entry.label, bold: entry.inspectable && !receded, dim: !entry.inspectable || receded, color: agentTone(index) },
            { text: `${gap}${status.outcome ?? status.activity}`, ...color === undefined || receded ? { dim: true } : { color } },
            ...status.outcome === undefined ? [] : [{ text: ` · ${status.activity}`, dim: true }],
            ...routing === undefined ? [] : [{ text: ` · ${routing.text}`, ...routing.caution ? { color: PALETTE.waiting } : { dim: true } }] ] },
        ...current ? hung([
          { text: `${entry.detail} · ${entry.id}${entry.inspectable ? '' : ` · ${copy.subagentNoTranscript}`}`, selected: false, dim: true },
          ...entry.routing === undefined ? [] : routingDetails(entry.routing, copy),
        ]) : [],
      ]
    }),
  ]
}
