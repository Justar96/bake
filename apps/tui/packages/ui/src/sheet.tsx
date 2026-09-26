/**
 * Scrollable read-only views that take over the dynamic region for a moment:
 * the complete goal and the complete task list. Their compact forms stay on
 * the header and the task row; a sheet is where the rest of the text goes.
 */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import type { TuiCopy } from './copy.ts'
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
}

/** A drawn row: a logical line's slice, with the glyph only on its first row. */
interface SheetRow extends SheetLine {
  readonly lead: string
  readonly first: boolean
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
  return lines.flatMap(line => {
    const lead = line.glyph === undefined ? '' : `${line.glyph} `
    if (line.text === '') return [{ ...line, lead, first: true }]
    return wrapAnsi(line.text, Math.max(1, width - stringWidth(lead)), { hard: true, trim: false }).split('\n')
      .map((text, index) => ({ ...line, text, first: index === 0, lead: index === 0 ? lead : ' '.repeat(stringWidth(lead)) }))
  })
}

/**
 * A read-only sheet. The caller owns the scroll position and keyboard focus.
 * @param props.offset - first content row shown; clamped to the last page.
 */
export function Sheet({ title, color, lines, copy, columns, limit, offset, frame }: {
  readonly title: string
  readonly color: PaletteColor
  readonly lines: readonly SheetLine[]
  readonly copy: TuiCopy
  readonly columns: number
  readonly limit: number
  readonly offset: number
  readonly frame: FrameStyle
}): React.ReactElement {
  const rows = sheetRows(lines, limit, columns)
  const page = sheetPage(limit, columns)
  const start = Math.min(offset, Math.max(0, rows.length - page))
  return <Box width={Math.min(columns, SHEET_WIDTH)} maxHeight={limit} flexDirection="column" flexShrink={0}
    {...framed(limit, columns) ? { borderStyle: frame, borderColor: color, paddingX: 1 } : {}}
    overflowY="hidden">
    {limit >= 3 && <Text bold wrap="truncate-end">{title}</Text>}
    {rows.slice(start, start + page).map((row, index) => <Text key={start + index} wrap="truncate-end">
      {row.lead === '' ? null : <Text bold={row.first && row.glyphColor !== undefined}
        {...row.glyphColor === undefined ? { dimColor: true } : { color: row.glyphColor }}>{row.lead}</Text>}
      <Text bold={row.bold === true} dimColor={row.dim === true} strikethrough={row.strikethrough === true}
        {...row.color === undefined ? {} : { color: row.color }}>{row.text || ' '}</Text>
    </Text>)}
    {limit >= 2 && <Text dimColor wrap="truncate-end">
      {`${copy.sheetScroll} · ${copy.sheetClose}  ${Math.min(rows.length, start + 1)}-${Math.min(rows.length, start + page)}/${rows.length}`}
    </Text>}
  </Box>
}
