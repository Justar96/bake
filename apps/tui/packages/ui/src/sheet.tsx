/**
 * Views that take over the dynamic region for a moment: the complete task
 * list, the session's subagents, and the complete goal. Their compact forms
 * stay on the task row, the status line, and the header; a sheet is where the
 * rest goes. One key cycles through the views that have something to show,
 * so a tab strip across the top names them and marks the one open.
 */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import type { FrameStyle } from './layout.ts'
import type { PaletteColor } from './palette.ts'

/** One logical line of a sheet. It wraps to the sheet's width when drawn. */
export interface SheetLine {
  readonly text: string
  /** Leading shape. Wrapped rows hang past it, so the text keeps one edge. */
  readonly glyph?: string
  readonly glyphColor?: PaletteColor
  readonly color?: PaletteColor
  readonly bold?: boolean
  readonly dim?: boolean
  readonly strikethrough?: boolean
  /**
   * Whether this line is the list's selection. Present, true or false, only
   * on the lines of a view that selects; they share a pointer column.
   */
  readonly selected?: boolean
  /**
   * Runs of their own emphasis, drawn on one row in place of `text`, which a
   * bar or a label beside its status needs. Such a line is cut, never wrapped.
   */
  readonly parts?: readonly SheetPart[]
}

/** One run of a line drawn in parts. */
export interface SheetPart {
  readonly text: string
  readonly color?: PaletteColor
  readonly bold?: boolean
  readonly dim?: boolean
}

/** One view the cycle key reaches, as the tab strip names it. */
export interface SheetTab {
  readonly label: string
  readonly color: PaletteColor
  /** Whether this is the view drawn under the strip. */
  readonly current: boolean
}

/** A drawn row: a logical line's slice, with the glyph only on its first row. */
interface SheetRow extends SheetLine {
  readonly lead: string
  readonly first: boolean
  /** The logical line this row is part of. */
  readonly line: number
}

/** The pointer column of a view that selects. */
const POINTER = { on: '\u25b8 ', off: '  ' } as const

/**
 * A bar of heavy and light rules, filled in proportion.
 * @param done - how much is done.
 * @param total - how much there is; zero draws an empty bar.
 * @param cells - the bar's width.
 * @param color - the filled part's colour.
 */
export function sheetBar(done: number, total: number, cells: number, color: PaletteColor): readonly SheetPart[] {
  const filled = total <= 0 ? 0 : Math.max(0, Math.min(cells, Math.round(cells * done / total)))
  return [{ text: '\u2501'.repeat(filled), color }, { text: '\u2500'.repeat(cells - filled), dim: true }]
}

/** Widest a sheet grows. Past this, wrapped prose is harder to read than to scroll. */
const SHEET_WIDTH = 80

/** Below five rows a frame and title would leave no room for the content. */
const framed = (limit: number, columns: number): boolean => limit >= 5 && columns >= 4

/**
 * Content rows a sheet shows at once, after its frame, title, and key line.
 * @param limit - rows the sheet may draw.
 * @param columns - terminal width.
 */
export function sheetPage(limit: number, columns: number): number {
  return Math.max(1, limit - (framed(limit, columns) ? 4 : limit >= 3 ? 2 : limit >= 2 ? 1 : 0))
}

/**
 * The lines wrapped to the width a sheet with this many rows draws them at.
 * @returns every drawn row, so the caller can bound its scroll position.
 */
export function sheetRows(lines: readonly SheetLine[], limit: number, columns: number): readonly SheetRow[] {
  const width = Math.max(1, Math.min(columns, SHEET_WIDTH) - (framed(limit, columns) ? 4 : 0))
  return lines.flatMap((line, number) => {
    const pointer = line.selected === undefined ? '' : POINTER.off
    const lead = `${pointer}${line.glyph === undefined ? '' : `${line.glyph} `}`
    if (line.text === '' || line.parts !== undefined) return [{ ...line, lead, first: true, line: number }]
    return wrapAnsi(line.text, Math.max(1, width - stringWidth(lead)), { hard: true, trim: false }).split('\n')
      .map((text, index) => ({ ...line, text, first: index === 0, line: number, lead: index === 0 ? lead : ' '.repeat(stringWidth(lead)) }))
  })
}

/**
 * First row a page starts at, keeping a followed line whole in view when it fits.
 * @param rows - every drawn row.
 * @param page - rows a page shows.
 * @param offset - the caller's scroll position.
 * @param follow - logical line to keep in view, such as a list's selection.
 */
function pageStart(rows: readonly SheetRow[], page: number, offset: number, follow: number | undefined): number {
  const last = Math.max(0, rows.length - page)
  if (follow === undefined) return Math.min(offset, last)
  const first = rows.findIndex(row => row.line === follow)
  if (first < 0) return Math.min(offset, last)
  const end = rows.findLastIndex(row => row.line === follow)
  return Math.max(0, Math.min(first, last, Math.max(offset, end - page + 1)))
}

/**
 * A sheet under its tab strip. The caller owns the scroll position, the
 * selection, and keyboard focus.
 * @param props.tabs - every view the cycle key reaches, the open one current.
 * @param props.keys - what the keys do in this view, for its footer.
 * @param props.offset - first content row shown; clamped to the last page.
 * @param props.follow - logical line kept in view, which a selecting view passes.
 */
export function Sheet({ tabs, color, lines, keys, columns, limit, offset, follow, frame }: {
  readonly tabs: readonly SheetTab[]
  readonly color: PaletteColor
  readonly lines: readonly SheetLine[]
  readonly keys: string
  readonly columns: number
  readonly limit: number
  readonly offset: number
  readonly follow?: number | undefined
  readonly frame: FrameStyle
}): React.ReactElement {
  const rows = sheetRows(lines, limit, columns)
  const page = sheetPage(limit, columns)
  const start = pageStart(rows, page, offset, follow)
  return <Box width={Math.min(columns, SHEET_WIDTH)} maxHeight={limit} flexDirection="column" flexShrink={0}
    {...framed(limit, columns) ? { borderStyle: frame, borderColor: color, paddingX: 1 } : {}}
    overflowY="hidden">
    {limit >= 3 && <Text wrap="truncate-end">
      {tabs.map((tab, index) => <React.Fragment key={tab.label}>
        {index === 0 ? null : <Text dimColor>{' '}</Text>}
        {tab.current
          ? <Text bold inverse color={tab.color}>{` ${tab.label} `}</Text>
          : <Text dimColor>{` ${tab.label} `}</Text>}
      </React.Fragment>)}
    </Text>}
    {rows.slice(start, start + page).map((row, index) => {
      const pointer = row.selected === undefined ? '' : row.first && row.selected ? POINTER.on : POINTER.off
      const glyph = row.lead.slice(pointer.length)
      return <Text key={start + index} wrap="truncate-end">
        {pointer === '' ? null : <Text bold color={color}>{pointer}</Text>}
        {glyph === '' ? null : <Text bold={row.first && row.glyphColor !== undefined}
          {...row.glyphColor === undefined ? { dimColor: true } : { color: row.glyphColor }}>{glyph}</Text>}
        {row.parts === undefined
          ? <Text bold={row.bold === true} dimColor={row.dim === true} strikethrough={row.strikethrough === true}
            inverse={row.first && row.selected === true}
            {...row.color === undefined ? {} : { color: row.color }}>{row.text || ' '}</Text>
          : row.parts.map((part, key) => <Text key={key} bold={part.bold === true} dimColor={part.dim === true}
            inverse={key === 0 && row.selected === true}
            {...part.color === undefined ? {} : { color: part.color }}>{part.text}</Text>)}
      </Text>
    })}
    {limit >= 2 && <Text dimColor wrap="truncate-end">
      {`${keys}  ${Math.min(rows.length, start + 1)}-${Math.min(rows.length, start + page)}/${rows.length}`}
    </Text>}
  </Box>
}
