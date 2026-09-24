/** Connected child rows; identities and activity come from the application. */
import React from 'react'
import { Box, Text } from 'ink'
import type { TuiCopy } from './copy.ts'
import { MARKER } from './layout.ts'
import { Branch, SectionHead } from './line.tsx'
import { PALETTE, type PaletteColor } from './palette.ts'

export interface SubagentEntry {
  readonly id: string
  readonly label: string
  readonly state: 'working' | 'live' | 'saved' | 'issue'
  /** Latest recorded child turn outcome; absent during a new run or without a terminal record. */
  readonly outcome?: 'completed' | 'failed' | 'stopped'
  readonly detail: string
  readonly inspectable: boolean
}

/** Glyph, colour, and locale-owned word for each state a child can be in. */
const STATE = {
  working: { glyph: MARKER.action, color: PALETTE.running, word: 'subagentWorking' },
  live: { glyph: MARKER.action, color: PALETTE.asking, word: 'subagentLive' },
  saved: { glyph: MARKER.waiting, color: undefined, word: 'subagentSaved' },
  issue: { glyph: '!', color: PALETTE.failed, word: 'subagentIssue' },
} as const satisfies Record<SubagentEntry['state'], { readonly glyph: string, readonly color: PaletteColor | undefined, readonly word: keyof TuiCopy }>

/** Keep the recorded outcome separate from whether the session is resident. */
export function subagentStatus(entry: SubagentEntry, copy: TuiCopy): string {
  const activity = copy[STATE[entry.state].word]
  if (entry.state === 'working' || entry.outcome === undefined) return activity
  return `${copy[entry.outcome === 'completed' ? 'subagentCompleted' : entry.outcome === 'failed' ? 'subagentFailed' : 'subagentStopped']} · ${activity}`
}

/**
 * A bounded branch list that keeps settled children available for inspection.
 *
 * Drawn as the task list is: a head naming the block and counting how many
 * children are working, each child hanging from it on a branch with its
 * state's glyph and word, and the keys that open one under the tree. The head
 * is the row kept when rows run short, since it says what the block is; the
 * keys go before any child does.
 *
 * @param props.entries - the connected children, in the order the application lists them.
 * @param props.copy - locale-owned labels.
 * @param props.limit - rows the panel may draw, head and keys included.
 * @returns the panel, or null with no children or no room.
 */
export function Subagents({ entries, copy, limit }: {
  readonly entries: readonly SubagentEntry[]
  readonly copy: TuiCopy
  readonly limit: number
}): React.ReactElement | null {
  if (entries.length === 0 || limit <= 0) return null
  const working = entries.filter(entry => entry.state === 'working').length
  // The keys only once a child fits beside them: a hint for a list the panel
  // cannot show opens nothing the reader can see.
  const hint = limit >= 3 ? 1 : 0
  const room = Math.max(0, limit - 1 - hint)
  const shown = entries.length <= room ? entries : entries.slice(0, Math.max(0, room - 1))
  const hidden = entries.length - shown.length
  return <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
    <SectionHead color={working > 0 ? PALETTE.running : undefined} title={copy.subagentsTitle}
      detail={`${entries.length} \u00b7 ${working} ${copy.subagentWorking}`} />
    {shown.map((entry, index) => {
      const state = STATE[entry.state]
      const outcome = entry.state === 'working' ? undefined : entry.outcome
      const color = outcome === 'completed' ? PALETTE.done : outcome === 'failed' ? PALETTE.failed : outcome === 'stopped' ? PALETTE.waiting : state.color
      const glyph = outcome === 'completed' ? '✓' : outcome === 'failed' ? '✗' : outcome === 'stopped' ? '■' : state.glyph
      return <Branch key={entry.id} last={index === shown.length - 1 && hidden === 0} glyph={glyph} color={color}>
        {entry.label}<Text dimColor>{`  ${subagentStatus(entry, copy)}`}</Text>
      </Branch>
    })}
    {hidden > 0 && room > 0 && <Branch last><Text dimColor>{`+${hidden} ${copy.pickerMore}`}</Text></Branch>}
    {hint > 0 && <Text dimColor wrap="truncate-end">{`  ${copy.subagentsHint}`}</Text>}
  </Box>
}
