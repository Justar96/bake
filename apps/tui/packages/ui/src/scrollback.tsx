/** Bounded admission of committed history into Ink's permanent terminal output. */
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Box, measureElement, Static, useApp, type DOMElement } from 'ink'
import type { TuiCopy } from './copy.ts'
import type { Budget, FrameStyle, WindowSize } from './layout.ts'
import { Line, lineHeight } from './line.tsx'
import { present, type PresentedLine, type ResultBound } from './present.ts'
import { ReplayCursor, type ReplayBatch } from './replay.ts'
import type { Transcript } from './transcript.ts'
import { Welcome } from './welcome.tsx'

/** Welcome and session heading captured when a fresh session mounts. */
export interface Opening { readonly kind: 'welcome', readonly version: string, readonly heading: string }

interface ScrollbackProps {
  readonly transcript: Transcript
  readonly heading: string
  readonly opening: Opening | undefined
  readonly budget: Budget
  readonly result: ResultBound
  readonly copy: TuiCopy
  readonly frame: FrameStyle
  readonly size: WindowSize
  readonly repainting: boolean
  /** Sheets must not make the composer retain their expanded height. */
  readonly recording: boolean
  readonly children: React.ReactNode
}

const EMPTY_BATCH: ReplayBatch = { start: 0, lines: [], height: 0 }
const LEAD_ITEMS = 1
type StaticItem = Opening | { readonly kind: 'heading', readonly text: string } | PresentedLine

/** Keep one Static instance, exposing only the count actually admitted this render. */
const Committed = memo(function Committed({ batch, heading, opening, budget, result, copy, frame, columns }: {
  readonly batch: ReplayBatch
  readonly heading: string
  readonly opening: Opening | undefined
  readonly budget: Budget
  readonly result: ResultBound
  readonly copy: TuiCopy
  readonly frame: FrameStyle
  readonly columns: number
}): React.ReactElement {
  // Static consumes length and slice(index). Remounting it clears Ink's saved
  // scrollback; exposing the source length would skip all unadmitted lines.
  const items = useMemo(() => ({
    length: LEAD_ITEMS + batch.start + batch.lines.length,
    slice(start = 0): StaticItem[] {
      const suffix = batch.lines.slice(Math.max(0, start - LEAD_ITEMS - batch.start))
      return start === 0 ? [opening ?? { kind: 'heading', text: heading }, ...suffix] : suffix
    },
  }) as StaticItem[], [batch, heading, opening])
  return <Static items={items}>
    {(item, index) => 'kind' in item
      ? item.kind === 'welcome'
        ? <Welcome key={index} version={item.version} heading={item.heading} copy={copy} frame={frame} columns={columns} />
        : <React.Fragment key={index}>{present({ kind: 'notice', tone: 'info', text: item.text }, result)
            .map((line, part) => <Line key={part} line={line} budget={budget} />)}</React.Fragment>
      : <Line key={index} line={item} budget={budget} />}
  </Static>
})

/**
 * Own replay and frame height for one visible session. Unmounting for child
 * inspection cancels admission; returning mounts a fresh cursor and Static.
 * @param props - immutable history, terminal geometry, and dynamic controls.
 * @returns permanent scrollback above a frame held against the terminal bottom.
 */
export function Scrollback(props: ScrollbackProps): React.ReactElement {
  const { transcript, budget, result } = props
  const { waitUntilRenderFlush, exit } = useApp()
  const [replay, setReplay] = useState(() => {
    const cursor = new ReplayCursor()
    return { cursor, batch: cursor.next(transcript, budget, result) ?? EMPTY_BATCH }
  })
  const source = useRef(transcript)
  const latest = useRef({ transcript, budget, result })
  useLayoutEffect(() => {
    latest.current = { transcript, budget, result }
    if (source.current === transcript) return
    source.current = transcript
    // A live commit replaces its preview in the same React flush. Admit its
    // first batch here only when older history has already been consumed.
    if (!replay.cursor.caughtUp) return
    const batch = replay.cursor.next(transcript, budget, result)
    if (batch !== undefined) setReplay({ cursor: replay.cursor, batch })
  }, [transcript, budget, result, replay])
  useEffect(() => {
    let active = true
    // A flush yields to input and waits for stdout before building another
    // React/Yoga tree. Never chain the whole backlog through synchronous effects.
    void waitUntilRenderFlush().then(() => {
      if (!active) return
      const current = latest.current
      const batch = replay.cursor.next(current.transcript, current.budget, current.result)
      if (batch !== undefined) setReplay({ cursor: replay.cursor, batch })
    }).catch((error: unknown) => { if (active) exit(error) })
    return () => { active = false }
  }, [replay, waitUntilRenderFlush, exit])
  const held = useHeldHeight(replay.batch, budget, props.repainting, props.recording)
  return <Box flexDirection="column">
    <Committed batch={replay.batch} heading={props.heading} opening={props.opening} budget={budget} result={result}
      copy={props.copy} frame={props.frame} columns={props.size.columns} />
    {props.repainting && <Box height={props.size.rows} flexShrink={0} />}
    <Box ref={held.frame} flexDirection="column" flexShrink={0} minHeight={held.floor} maxHeight={budget.dynamic} overflowY="hidden">
      {props.children}
    </Box>
  </Box>
}

/** Subtract only newly printed rows before layout, keeping shrinking controls anchored. */
function useHeldHeight(batch: ReplayBatch, budget: Budget, repainting: boolean, recording: boolean): {
  readonly floor: number
  readonly frame: React.RefObject<DOMElement | null>
} {
  const frame = useRef<DOMElement>(null)
  const held = useRef({ printed: 0, height: 0 })
  const printed = LEAD_ITEMS + batch.start + batch.lines.length
  let remaining = held.current.height
  if (held.current.printed < printed) {
    for (const line of batch.lines) {
      remaining -= lineHeight(line, budget)
      if (remaining <= 0) break
    }
  }
  remaining = Math.max(0, remaining)
  const floor = repainting ? 0 : Math.min(budget.dynamic, remaining)
  useLayoutEffect(() => {
    held.current = { printed, height: repainting ? 0 : recording && frame.current !== null
      ? measureElement(frame.current).height : remaining }
  })
  return { floor, frame }
}
