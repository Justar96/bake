/** Alternate-screen transcript and controls, constrained to the terminal rectangle. */
import React, { useImperativeHandle, useLayoutEffect, useRef, useState } from 'react'
import { Box, Text, measureElement, type DOMElement } from 'ink'
import type { Clock } from './activity.ts'
import type { TuiCopy } from './copy.ts'
import type { Budget, FrameStyle, WindowSize } from './layout.ts'
import { Line } from './line.tsx'
import type { ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import type { Opening } from './scrollback.tsx'
import type { Transcript } from './transcript.ts'
import { Viewport, type ReadingAnchor } from './viewport.ts'
import { Welcome } from './welcome.tsx'

/** Scroll actions routed after modal keys and before composer editing. */
export interface TranscriptScroll { move(direction: 'up' | 'down' | 'start' | 'end'): void }

/**
 * Keep controls at the bottom while rendering only the visible transcript.
 * An undefined reading position follows output; paging up holds the source row
 * through appends and resize. The session owner remounts this on navigation.
 */
export function Fullscreen({ transcript, live, heading, opening, budget, result, size, frame, copy, clock, children, ref }: {
  readonly transcript: Transcript
  readonly live: readonly Row[]
  readonly heading: string
  readonly opening: Opening | undefined
  readonly budget: Budget
  readonly result: ResultBound
  readonly size: WindowSize
  readonly frame: FrameStyle
  readonly copy: TuiCopy
  readonly clock: Clock | undefined
  readonly children: React.ReactNode
  readonly ref: React.Ref<TranscriptScroll>
}): React.ReactElement {
  const [viewport] = useState(() => new Viewport(heading))
  const [position, setPosition] = useState<ReadingAnchor | undefined>(undefined)
  const body = useRef<DOMElement>(null)
  const [height, setHeight] = useState(Math.max(0, size.rows - budget.chrome.rows))
  viewport.update(transcript, live)
  viewport.configure(budget, result)
  useLayoutEffect(() => {
    const measured = body.current === null ? 0 : measureElement(body.current).height
    if (measured !== height) setHeight(measured)
  })
  const hint = height > 1 ? 1 : 0
  const room = Math.max(0, height - hint)
  const bottom = viewport.move(viewport.end, -room, budget, result)
  const top = position === undefined ? bottom : viewport.locate(position, budget, result)
  // A settled streaming prefix can become several committed rows. Rebase the
  // held anchor after that transition, before another fragment is appended.
  useLayoutEffect(() => {
    if (position === undefined) return
    const rebased = viewport.anchor(top, budget, result)
    if (rebased.source !== position.source) setPosition(current => current === position ? rebased : current)
    else if (position.columns !== budget.columns) {
      setPosition(current => current === position ? { ...position, ...top, columns: budget.columns } : current)
    }
  })
  useImperativeHandle(ref, () => ({
    move(direction) {
      setPosition(previous => {
        const current = previous === undefined ? bottom : viewport.locate(previous, budget, result)
        const next = direction === 'end' ? undefined : direction === 'start' ? { row: 0, offset: 0 }
          : viewport.move(current, (direction === 'up' ? -1 : 1) * Math.max(1, Math.floor(room / 2)), budget, result)
        const following = next === undefined || next.row > bottom.row || (next.row === bottom.row && next.offset >= bottom.offset)
        return following ? undefined : viewport.anchor(next!, budget, result)
      })
    },
  }))
  const lines = viewport.window(top, room, budget, result)
  const welcome = opening !== undefined && transcript.length === 0 && live.length === 0
  return <Box flexDirection="column" width={size.columns} height={size.rows} overflow="hidden">
    <Box ref={body} flexDirection="column" flexGrow={1} flexBasis={0} minHeight={0} overflow="hidden">
      <Box flexDirection="column" height={room} flexShrink={0} overflow="hidden">
        {welcome
          ? <Welcome {...opening} copy={copy} frame={frame} columns={size.columns} />
          : lines.map(item => <Line key={item.key} line={item.line} budget={budget} clock={clock}
              window={{ offset: item.offset, height: item.height }} />)}
      </Box>
      {hint > 0 && <Text dimColor wrap="truncate-end">{position === undefined ? copy.transcriptScroll : copy.transcriptPaused}</Text>}
    </Box>
    <Box flexDirection="column" flexShrink={0} maxHeight={size.rows} overflow="hidden">{children}</Box>
  </Box>
}
