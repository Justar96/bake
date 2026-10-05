/**
 * The row `/update` draws while it installs a release: the orbit, what the
 * install is doing, and its meter, as the installers draw their live row.
 *
 * @module bake-tui-ui/installing
 */
import React from 'react'
import { Box, Text, useWindowSize } from 'ink'
import { useBeat } from './beat.tsx'
import { liveRow, type InstallStep, type ProgressGlyphs, type Run } from './install-progress.ts'
import type { Clock } from './activity.ts'

/** What a row draws, as a key that differs when the drawing does. */
const keyOf = (runs: readonly Run[]): string => runs.map(run => `${run.color ?? ''}${run.text}`).join('')

/**
 * One row, the width of the terminal.
 *
 * The orbit, the glint, and the comet move on the surface's beat, and only
 * with a clock; without one, as under a screen reader or reduced motion,
 * the row rests on its first frame and the meter still fills with progress.
 *
 * @param props.step - what the install is doing, and how far when that is known.
 * @param props.glyphs - `ascii` where the terminal draws the classic frame.
 * @param props.clock - time source for the motion.
 */
export function Installing({ step, glyphs, clock }: {
  readonly step: InstallStep
  readonly glyphs: ProgressGlyphs
  readonly clock?: Clock | undefined
}): React.ReactElement {
  const { columns } = useWindowSize()
  const row = (time: number): readonly Run[] => liveRow(step, { time, width: columns, glyphs })
  const now = useBeat(clock !== undefined, time => keyOf(row(time))) ?? 0
  return (
    <Box flexShrink={0} height={1} overflowX="hidden">
      <Text wrap="truncate-end">
        {row(now).map((run, index) => <Text key={index} {...run.color === undefined ? {} : { color: run.color }}
          {...run.dim === true ? { dimColor: true } : {}} {...run.bold === true ? { bold: true } : {}}>{run.text}</Text>)}
      </Text>
    </Box>
  )
}
