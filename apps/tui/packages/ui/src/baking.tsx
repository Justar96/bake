/**
 * The row `/update` draws while it installs a release: steam rising over a
 * loaf that browns as the install advances, then what it is doing.
 *
 * @module @dsh-tui/ui/baking
 */
import React from 'react'
import { Box, Text } from 'ink'
import { useBeat } from './beat.tsx'
import { loafFrame, type LoafGlyphs } from './loaf.ts'
import type { Clock } from './activity.ts'

/**
 * One row, the width of the terminal.
 *
 * The steam moves on the surface's beat, and only with a clock; without
 * one, as under a screen reader, `NO_COLOR`, or in a test, it rests on its
 * first frame. The loaf's tone changes only with the progress.
 *
 * @param props.label - what the install is doing, cut to the row.
 * @param props.level - how brown the loaf is, from 0 to 1.
 * @param props.glyphs - `ascii` where the terminal draws the classic frame.
 * @param props.clock - time source for the steam.
 */
export function Baking({ label, level, glyphs, clock }: {
  readonly label: string
  readonly level: number
  readonly glyphs: LoafGlyphs
  readonly clock?: Clock | undefined
}): React.ReactElement {
  const now = useBeat(clock !== undefined, time => loafFrame(time, level, glyphs).steam) ?? 0
  const frame = loafFrame(now, level, glyphs)
  return (
    <Box flexShrink={0} height={1} overflowX="hidden">
      <Text wrap="truncate-end">
        <Text dimColor>{frame.steam}</Text>{' '}
        <Text color={frame.tone}>{frame.loaf}</Text>{'  '}
        {label}
      </Text>
    </Box>
  )
}
