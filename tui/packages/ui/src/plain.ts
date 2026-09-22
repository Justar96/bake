/**
 * Plain-text row rendering for surfaces without a component renderer: the
 * pre-render startup report, `--json`-free diagnostics, and test assertions.
 * Pure, so it runs under `bun test` alongside the projection.
 *
 * @module @dsh-tui/ui/plain
 */

import { formatAttachment, type CardLine, type Row } from './rows.ts'

/** Left gutter marking each row kind, wide enough to align in a fixed-width terminal. */
const GUTTER: Record<Row['kind'], string> = {
  'user': '>',
  'command': '/',
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
    case 'user':
      return `${gutter} ${oneLine([row.text, ...(row.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n'))}`
    case 'command':
      return `${gutter} ${row.name}${oneLine(row.args)}`
    case 'tool-call':
      return `${gutter} ${row.tool}(${oneLine(row.input)})${cardText(row.detail)}`
    case 'tool-result':
      // A card leaves `text` empty, so its lines are the whole result here.
      return `${gutter} ${row.ok ? '' : 'error '}${oneLine(row.text)}${cardText(row.detail)}`
    default:
      return `${gutter} ${oneLine(row.text)}`
  }
}

/**
 * A tool card's lines, collapsed onto the row's single line.
 *
 * Diff emphasis is carried by the `-`/`+` the card already wrote into the
 * text, because this surface has no colour to carry it instead.
 *
 * @param detail - the card's lines, absent when the tool declared no card.
 * @returns the lines joined behind a separator, empty when there is no card.
 */
function cardText(detail: readonly CardLine[] | undefined): string {
  if (detail === undefined || detail.length === 0) return ''
  return ` ${detail.map(line => oneLine(line.text)).filter(text => text !== '').join(' · ')}`
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
