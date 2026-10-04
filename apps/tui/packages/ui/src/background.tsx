/** Background work the application lists from the job registry; its row under the input is drawn here. */
import React from 'react'
import { Box, Text } from 'ink'
import type { TuiCopy } from './copy.ts'
import { ICON } from './icons.ts'
import { COLUMN } from './layout.ts'
import { PALETTE } from './palette.ts'
import { toolText } from './tool-output.ts'

/** One background job the session's agent started, as the registry last reported it. */
export interface BackgroundEntry {
  /** The registry's `<kind>-N` id, which the model passes to `job_output` and `job_kill`. */
  readonly id: string
  /** The producer kind, a tool name such as `bash`. */
  readonly tool: string
  /** The producer's one-line label, such as the command. */
  readonly label: string
  /** Whether it still runs; a stopping job is still running until it settles. */
  readonly running: boolean
}

/**
 * The session's running background work as one row under the input, below
 * the subagents row and above the status line:
 * `◌ Background · 2 running · bash-3 npm run dev`.
 *
 * It is there only while something runs, because finished work has already
 * said how it ended in the transcript, on the row that closes its job. It
 * shares the grammar of every standing row: the icon the transcript gives
 * background work in the rail, the name, then the count, in lowercase. The
 * rail is in the running colour, which is the row's one claim on attention;
 * the rest is dim, ending with the newest job's id and label, since that is
 * usually the one a reader is waiting on and the id is what the model and
 * `job_kill` name it by. The row is not focusable, and cut at the right edge.
 *
 * @param props.entries - the session's jobs, oldest first.
 * @param props.columns - row width.
 * @returns the row, or null when nothing runs.
 */
export function BackgroundRow({ entries, copy, columns }: {
  readonly entries: readonly BackgroundEntry[]
  readonly copy: TuiCopy
  readonly columns: number
}): React.ReactElement | null {
  const running = entries.filter(entry => entry.running)
  const newest = running.at(-1)
  if (newest === undefined || columns <= 0) return null
  const rail = Math.min(COLUMN.rail, columns)
  const label = toolText(newest.label).replace(/\s+/g, ' ').trim()
  return <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
    <Box width={rail} flexShrink={0}><Text bold color={PALETTE.running}>{ICON.background}</Text></Box>
    <Box flexGrow={1} flexShrink={1}>
      <Text wrap="truncate-end" dimColor>
        {`${copy.backgroundTitle} · ${running.length} ${copy.backgroundRunning} · ${newest.id}${label === '' ? '' : ` ${label}`}`}
      </Text>
    </Box>
  </Box>
}
