/** Alternate-screen transcript and controls, constrained to the terminal rectangle. */
import React, { useEffect, useImperativeHandle, useLayoutEffect, useRef, useState } from 'react'
import { Box, Text, measureElement, type DOMElement } from 'ink'
import type { Clock } from './activity.ts'
import type { TuiCopy } from './copy.ts'
import type { Budget, FrameStyle, WindowSize } from './layout.ts'
import { Line, rowText } from './line.tsx'
import { PALETTE } from './palette.ts'
import type { ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import type { Opening } from './scrollback.tsx'
import type { Transcript } from './transcript.ts'
import { CLICK_MS, compareRows, ordered, rowColumns, sliceCells, wordAt, type SelectionGranularity, type SelectionPoint, type SelectionRange } from './selection.ts'
import { Viewport, type PhysicalRow, type Position, type ReadingAnchor } from './viewport.ts'
import { Welcome } from './welcome.tsx'

/** Scroll actions routed after modal keys and before composer editing. */
export interface TranscriptScroll {
  /** Page by a viewport less {@link PAGE_OVERLAP} rows, or reach either end; `end` follows output again. */
  move(direction: 'up' | 'down' | 'start' | 'end'): void
  /**
   * Bring the previous or next user prompt to the top. Past the first prompt
   * this reaches the beginning; past the last it follows output again.
   */
  prompt(direction: -1 | 1): void
  /** Move by terminal rows, negative toward older output, as a mouse wheel does. */
  scroll(rows: number): void
  /**
   * The primary button pressed, dragged, or released at a zero-based screen
   * cell. A press on the jump-to-latest row follows output; one in the
   * transcript starts a selection, by word on a double click and by row on a
   * triple. Released, a selection is copied.
   */
  pointer(kind: 'press' | 'drag' | 'release', column: number, row: number): void
  /** Drop the selection. @returns whether there was one, so Escape does nothing else. */
  deselect(): boolean
}

/** Milliseconds between rows while a drag held at the top or bottom edge scrolls the selection on. */
const EDGE_SCROLL_MS = 50

/** Milliseconds the hint row says a selection was copied. */
const COPIED_MS = 1500

/** A press in progress: what it extends by, and the range a double or triple click started with. */
interface Gesture {
  readonly granularity: SelectionGranularity
  readonly initial: SelectionRange | undefined
  dragged: boolean
}

/** The last press, for counting a double or triple click on the same word. */
interface Click {
  readonly at: number
  readonly point: SelectionPoint
  readonly word: { readonly from: number, readonly to: number } | undefined
  readonly count: number
}

/**
 * Rows a page keeps from the one before it, so reading continues from text
 * already seen. A short viewport still moves at least half its height.
 */
const PAGE_OVERLAP = 4

/**
 * @param room - transcript rows on screen.
 * @returns the rows PgUp and PgDn move.
 */
const pageRows = (room: number): number => Math.max(1, Math.ceil(room / 2), room - PAGE_OVERLAP)

/**
 * Keep controls at the bottom while rendering only the visible transcript.
 * An undefined reading position follows output; paging up holds the source row
 * through appends and resize. While reading history, the hint row offers the
 * way back to the newest output and says when output arrived below the view.
 * The session owner remounts this on navigation.
 *
 * The terminal reports the mouse here, so its own selection is gone and this
 * draws one: a drag selects cells, a double click a word, a triple click a
 * row, reversed in place and copied through `onCopy` on release. A selection
 * names transcript rows rather than screen rows, so it stays on its text while
 * the view scrolls; a drag held at either edge scrolls on through
 * `timers`. A resize drops it, since its rows wrap anew.
 */
export function Fullscreen({ transcript, live, heading, opening, budget, result, size, frame, copy, clock, timers = clock, onCopy, children, ref }: {
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
  /**
   * Times double clicks, the edge scroll, and the copy notice; defaults to
   * `clock`, which is absent while motion is off.
   */
  readonly timers?: Clock | undefined
  /** Put selected text on the clipboard; resolves whether it got there. Absent, a selection is drawn but not copied. */
  readonly onCopy?: ((text: string) => Promise<boolean>) | undefined
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
  // The newest row when reading began. Output arriving after it is unseen
  // until the reader follows again.
  const seen = useRef<{ readonly end: number, readonly latest: Row | undefined }>(undefined)
  useLayoutEffect(() => {
    if (position === undefined) seen.current = undefined
    else seen.current ??= { end: viewport.end.row, latest: viewport.latest }
  })
  const unseen = position !== undefined && seen.current !== undefined
    && (seen.current.end !== viewport.end.row || seen.current.latest !== viewport.latest)
  const go = (target: (current: Position) => Position | undefined): void => setPosition(previous => {
    const current = previous === undefined ? bottom : viewport.locate(previous, budget, result)
    const next = target(current)
    const following = next === undefined || next.row > bottom.row || (next.row === bottom.row && next.offset >= bottom.offset)
    return following ? undefined : viewport.anchor(next, budget, result)
  })
  const welcome = opening !== undefined && transcript.length === 0 && live.length === 0
  const lines = viewport.window(top, room, budget, result)
  /** The rows on screen from a top position, one per terminal row, as `lines` draws them. */
  const rowsFrom = (from: Position): readonly PhysicalRow[] => welcome || room === 0 ? []
    : viewport.between(from, viewport.move(from, room - 1, budget, result), budget, result).slice(0, room)
  const screen = rowsFrom(top)
  const textOf = (entry: PhysicalRow): string => rowText(entry.line, budget, frame, entry.offset)

  // The selection, mirrored in a ref so the reports of one read see each other's changes.
  const [selection, setSelection] = useState<{ readonly anchor: SelectionPoint, readonly focus: SelectionPoint } | undefined>(undefined)
  const selected = useRef(selection)
  const select = (next: typeof selection): void => {
    selected.current = next
    setSelection(next)
  }
  const gesture = useRef<Gesture | undefined>(undefined)
  const lastClick = useRef<Click | undefined>(undefined)
  const [flash, setFlash] = useState<boolean | undefined>(undefined)
  const edge = useRef<{ readonly column: number, readonly row: number, readonly direction: -1 | 1 } | undefined>(undefined)
  const stopEdge = useRef<(() => void) | undefined>(undefined)
  const stopFlash = useRef<(() => void) | undefined>(undefined)
  const mounted = useRef(true)
  const halt = (): void => {
    stopEdge.current?.()
    stopEdge.current = undefined
    edge.current = undefined
  }
  // No tick and no copy result lands after unmount.
  useEffect(() => () => {
    mounted.current = false
    halt()
    stopFlash.current?.()
  }, [])
  // Rows wrap anew at another width, so a selection no longer names its text.
  useEffect(() => {
    if (selected.current === undefined && gesture.current === undefined) return
    gesture.current = undefined
    halt()
    select(undefined)
  }, [budget.columns])

  /** The cell under the pointer; above the rows is their start, below them their end. */
  const pointAt = (column: number, row: number, rows: readonly PhysicalRow[]): SelectionPoint | undefined => {
    const last = rows.at(-1)
    if (last === undefined) return undefined
    if (row < 0) return { ...rows[0]!.position, column: 0 }
    if (row >= rows.length) return { ...last.position, column: budget.columns, boundary: true }
    return { ...rows[row]!.position, column: Math.max(0, Math.min(budget.columns - 1, column)) }
  }
  /** The word or whole row around a point, as a double or triple click takes it. */
  const rangeAt = (point: SelectionPoint, granularity: SelectionGranularity, text: string): SelectionRange | undefined => {
    if (granularity === 'line') return { start: { ...point, column: 0 }, end: { ...point, column: budget.columns, boundary: true } }
    const word = wordAt(text, point.column)
    return word === undefined ? undefined : { start: { ...point, column: word.from }, end: { ...point, column: word.to, boundary: true } }
  }
  /** Move the selection's free end to a point, a word or row at a time after a double or triple click. */
  const extend = (point: SelectionPoint, rows: readonly PhysicalRow[]): void => {
    const held = gesture.current
    const current = selected.current
    if (held === undefined || current === undefined) return
    const entry = rows.find(row => compareRows(row.position, point) === 0)
    const range = held.initial === undefined || entry === undefined ? undefined : rangeAt(point, held.granularity, textOf(entry))
    if (held.initial === undefined || range === undefined) {
      select({ anchor: held.initial?.start ?? current.anchor, focus: point })
      return
    }
    const before = compareRows(range.start, held.initial.start) < 0
      || (compareRows(range.start, held.initial.start) === 0 && range.start.column < held.initial.start.column)
    select(before ? { anchor: held.initial.end, focus: range.start } : { anchor: held.initial.start, focus: range.end })
  }
  /** Copy the selection's text, each row as drawn and trimmed at its end, and say whether it reached the clipboard. */
  const copySelection = (): void => {
    const range = ordered(selected.current?.anchor, selected.current?.focus)
    if (range === undefined || onCopy === undefined) return
    const text = viewport.between(range.start, range.end, budget, result).map(entry => {
      const row = textOf(entry)
      const cells = rowColumns(range, entry.position, row)
      return cells === undefined ? '' : sliceCells(row, cells.from, cells.to).trimEnd()
    }).join('\n')
    if (text.trim() === '') return
    void onCopy(text).then(ok => ok, () => false).then(ok => {
      // Without timers nothing would take the notice down again.
      if (!mounted.current || timers === undefined) return
      stopFlash.current?.()
      setFlash(ok)
      stopFlash.current = timers.every(COPIED_MS, () => {
        stopFlash.current?.()
        stopFlash.current = undefined
        setFlash(undefined)
      })
    })
  }
  // The top shown, moved by each edge tick at once, since ticks can outrun renders.
  const shown = useRef(top)
  shown.current = top
  // The edge tick reads this render's view, so it is replaced on each one.
  const edgeTick = useRef<() => void>(() => {})
  edgeTick.current = () => {
    const held = edge.current
    const from = shown.current
    if (held === undefined) return
    // At either end there is nothing left to scroll into view.
    if (held.direction < 0 ? from.row === 0 && from.offset === 0 : compareRows(from, bottom) >= 0) { halt(); return }
    const next = viewport.move(from, held.direction, budget, result)
    shown.current = next
    go(() => next)
    const point = pointAt(held.column, held.row, rowsFrom(next))
    if (point !== undefined) extend(point, rowsFrom(next))
  }

  const press = (column: number, row: number): void => {
    if (hint > 0 && row === room) {
      if (position !== undefined) setPosition(undefined)
      return
    }
    halt()
    const entry = screen[row]
    const point = row < room ? pointAt(column, row, screen) : undefined
    if (point === undefined) {
      gesture.current = undefined
      if (selected.current !== undefined) select(undefined)
      return
    }
    const text = entry === undefined ? '' : textOf(entry)
    const word = entry === undefined ? undefined : wordAt(text, point.column)
    const now = timers?.now()
    const previous = lastClick.current
    const count = now !== undefined && word !== undefined && previous?.word !== undefined && now - previous.at <= CLICK_MS
      && compareRows(previous.point, point) === 0 && previous.word.from === word.from && previous.word.to === word.to
      ? previous.count % 3 + 1 : 1
    lastClick.current = now === undefined || word === undefined ? undefined : { at: now, point, word, count }
    const granularity: SelectionGranularity = count === 2 ? 'word' : count === 3 ? 'line' : 'character'
    const initial = granularity === 'character' ? undefined : rangeAt(point, granularity, text)
    gesture.current = { granularity: initial === undefined ? 'character' : granularity, initial, dragged: false }
    select(initial === undefined ? { anchor: point, focus: point } : { anchor: initial.start, focus: initial.end })
  }
  const drag = (column: number, row: number): void => {
    const held = gesture.current
    if (held === undefined) return
    held.dragged = true
    lastClick.current = undefined
    const pinned = Math.max(0, Math.min(room - 1, row))
    const point = pointAt(column, row >= room ? room : pinned, screen)
    if (point !== undefined) extend(point, screen)
    // Held on the top or bottom row, or past it, the view scrolls the selection on.
    const direction = row <= 0 ? -1 : row >= room - 1 ? 1 : 0
    if (direction === 0 || timers === undefined) { halt(); return }
    edge.current = { column, row: pinned, direction }
    stopEdge.current ??= timers.every(EDGE_SCROLL_MS, () => { edgeTick.current() })
  }
  const release = (): void => {
    const held = gesture.current
    if (held === undefined) return
    gesture.current = undefined
    halt()
    // A click selects nothing; it only clears what was selected.
    if (held.granularity === 'character' && !held.dragged) { select(undefined); return }
    copySelection()
  }

  useImperativeHandle(ref, () => ({
    move(direction) {
      go(current => direction === 'end' ? undefined : direction === 'start' ? { row: 0, offset: 0 }
        : viewport.move(current, (direction === 'up' ? -1 : 1) * pageRows(room), budget, result))
    },
    prompt(direction) {
      go(current => viewport.prompt(current, direction) ?? (direction < 0 ? { row: 0, offset: 0 } : undefined))
    },
    scroll(rows) { go(current => viewport.move(current, rows, budget, result)) },
    pointer(kind, column, row) {
      if (kind === 'press') press(column, row)
      else if (kind === 'drag') drag(column, row)
      else release()
    },
    deselect() {
      const had = selected.current !== undefined
      gesture.current = undefined
      halt()
      if (had) select(undefined)
      return had
    },
  }))
  const range = ordered(selection?.anchor, selection?.focus)
  // A line a selection touches is drawn a row at a time, each with the cells it covers.
  let at = 0
  const drawn = lines.flatMap(item => {
    const rows = screen.slice(at, at + item.height)
    at += item.height
    const marks = range === undefined ? [] : rows.map(entry => rowColumns(range, entry.position, textOf(entry)))
    if (marks.every(mark => mark === undefined)) {
      return [<Line key={item.key} line={item.line} budget={budget} frame={frame} clock={clock}
        window={{ offset: item.offset, height: item.height }} />]
    }
    return rows.map((entry, index) => <Line key={`${item.key}:${entry.offset}`} line={item.line} budget={budget} frame={frame} clock={clock}
      window={{ offset: entry.offset, height: 1 }} selected={marks[index]} />)
  })
  return <Box flexDirection="column" width={size.columns} height={size.rows} overflow="hidden">
    <Box ref={body} flexDirection="column" flexGrow={1} flexBasis={0} minHeight={0} overflow="hidden">
      <Box flexDirection="column" height={room} flexShrink={0} overflow="hidden">
        {welcome ? <Welcome {...opening} copy={copy} frame={frame} columns={size.columns} /> : drawn}
      </Box>
      {hint > 0 && flash !== undefined
        ? <Box justifyContent="flex-end" flexShrink={0}>
            <Text bold color={flash ? PALETTE.done : PALETTE.failed}>{flash ? copy.selectionCopied : copy.selectionCopyFailed}</Text>
          </Box>
        : hint > 0 && (position === undefined
        // Set to the right, apart from the transcript's left edge, so it does
        // not read as the last line of output.
        ? <Box justifyContent="flex-end" flexShrink={0}><Text wrap="truncate-end"><KeyHints text={copy.transcriptScroll} /></Text></Box>
        // The way back leads, so a narrow row truncates the keys instead. One
        // unpadded row: a full-width one would rewrap when the terminal narrows.
        : <Text wrap="truncate-end">
            <Text bold color={unseen ? PALETTE.waiting : PALETTE.asking}>{unseen ? copy.transcriptUnseen : copy.transcriptLatest}</Text>
            <Text>{'  '}</Text><KeyHints text={copy.transcriptPaused} />
          </Text>)}
    </Box>
    <Box flexDirection="column" flexShrink={0} maxHeight={size.rows} overflow="hidden">{children}</Box>
  </Box>
}

/**
 * A `·`-separated hint line with each key at full brightness and what it does
 * dimmed, so the keys stand out from their descriptions. A part's key is its
 * first word; a part of one word is all description.
 */
function KeyHints({ text }: { readonly text: string }): React.ReactElement {
  return <>{text.split(' \u00b7 ').map((part, index) => {
    const space = part.indexOf(' ')
    return <React.Fragment key={index}>
      {index === 0 ? null : <Text dimColor>{' \u00b7 '}</Text>}
      {space < 0 ? <Text dimColor>{part}</Text>
        : <><Text>{part.slice(0, space)}</Text><Text dimColor>{part.slice(space)}</Text></>}
    </React.Fragment>
  })}</>
}
