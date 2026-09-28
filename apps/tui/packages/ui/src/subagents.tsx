/** Connected child identities and activity come from the application; their row and sheet are drawn here. */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { ICON } from './icons.ts'
import { COLUMN, HINT_MIN_COLUMNS, MARKER } from './layout.ts'
import { agentTone, PALETTE, type PaletteColor } from './palette.ts'
import type { SheetLine } from './sheet.tsx'

export interface SubagentEntry {
  readonly id: string
  readonly label: string
  readonly state: 'working' | 'live' | 'saved' | 'issue'
  /** Latest recorded child turn outcome; absent during a new run or without a terminal record. */
  readonly outcome?: 'completed' | 'failed' | 'stopped'
  readonly detail: string
  readonly inspectable: boolean
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

/** The list's tab: its name, the number of children, and how many are working. */
export function subagentTab(entries: readonly SubagentEntry[], copy: TuiCopy): string {
  const working = entries.filter(entry => entry.state === 'working').length
  return `${copy.subagentsTitle} ${entries.length}${working === 0 ? '' : ` · ${working} ${copy.subagentCountWorking}`}`
}

/**
 * The session's children as one compact row under the input, above the status line.
 *
 * It sits below the composer because Down from an empty composer selects it.
 * It shares the grammar of every standing row: the transcript's `↳` in the
 * rail, the name and the total, then how many children are working and how
 * many cannot be read, when any are, in lowercase: `↳ Subagents 3 · 2 working`.
 * Names are left to the subagent sheet, so the row stays short however many
 * children there are. Dim, so it stays supporting material beside the draft.
 * `hint` names the key that opens the sheet at the right edge, given up below
 * {@link HINT_MIN_COLUMNS} as the composer's hint is. Focused, the rail holds
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
  const working = entries.filter(entry => entry.state === 'working').length
  const issues = entries.filter(entry => entry.state === 'issue').length
  const summary = [working > 0 ? `${working} ${copy.subagentCountWorking}` : '',
    issues > 0 ? `${issues} ${copy.subagentUnreadable}` : ''].filter(Boolean).join(' · ')
  const rail = Math.min(COLUMN.rail, columns)
  const head = `${copy.subagentsTitle} ${entries.length}`
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

/** Lines before the first child in {@link subagentSheet}. */
const SUBAGENT_LEAD = 2

/** Lines each child takes in {@link subagentSheet}. */
const SUBAGENT_LINES = 2

/** The logical line a selected child starts at, which the sheet keeps in view. */
export const subagentLine = (index: number): number => SUBAGENT_LEAD + index * SUBAGENT_LINES

/**
 * Every child of the session as a list to choose from. Each is its activity's
 * glyph, its name in bold beside its status in that status's colour, and a
 * dim line of how it runs and its id under the name. A child without a local
 * transcript stays in the list, dimmed, since Enter cannot open it.
 *
 * @param entries - the children, in the catalog's order.
 * @param selected - index of the child under the pointer.
 */
export function subagentSheet(entries: readonly SubagentEntry[], selected: number, copy: TuiCopy): readonly SheetLine[] {
  return [
    { text: copy.subagentChoose, dim: true },
    { text: '' },
    ...entries.flatMap((entry, index): SheetLine[] => {
      const shape = GLYPH[entry.state]
      const color = statusColor(entry)
      return [
        { text: '', selected: index === selected, glyph: shape.glyph, ...'color' in shape ? { glyphColor: shape.color } : {},
          parts: [{ text: entry.label, bold: entry.inspectable, dim: !entry.inspectable, color: agentTone(index) },
            { text: `  ${subagentStatus(entry, copy)}`, ...color === undefined ? { dim: true } : { color } }] },
        { text: `${entry.detail} · ${entry.id}${entry.inspectable ? '' : ` · ${copy.subagentNoTranscript}`}`,
          selected: false, glyph: ' ', dim: true },
      ]
    }),
  ]
}
