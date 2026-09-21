/**
 * Plain-text row rendering for surfaces without a component renderer: the
 * pre-render startup report, `--json`-free diagnostics, and test assertions.
 * Pure, so it runs under `bun test` alongside the projection.
 *
 * @module @dsh-tui/ui/plain
 */

import type { Row } from './rows.ts'

/** Left gutter marking each row kind, wide enough to align in a fixed-width terminal. */
const GUTTER: Record<Row['kind'], string> = {
  'user': '>',
  'assistant': ' ',
  'reasoning': '·',
  'tool-call': '⚙',
  'tool-result': '←',
  'notice': '!',
}

/**
 * Render one row as a single plain-text line, with no ANSI styling.
 *
 * @param row - the row to render.
 * @returns the line, without a trailing newline.
 */
export function formatRow(row: Row): string {
  const gutter = GUTTER[row.kind]
  switch (row.kind) {
    case 'tool-call':
      return `${gutter} ${row.tool}(${oneLine(row.input)})`
    case 'tool-result':
      return `${gutter} ${row.ok ? '' : 'error '}${oneLine(row.text)}`
    default:
      return `${gutter} ${oneLine(row.text)}`
  }
}

/**
 * Collapse a value to one line for a single-line surface.
 *
 * @param text - the text to collapse.
 * @returns the text with newlines replaced by spaces and edges trimmed.
 */
function oneLine(text: string): string {
  return text.replace(/\s*\n\s*/g, ' ').trim()
}
