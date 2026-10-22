/**
 * Plain-text row rendering for surfaces without a component renderer. The
 * pre-render startup report, `--json`-free diagnostics, and test assertions.
 * Pure, so it runs under `bun test` alongside the projection.
 *
 * @module bake-tui-ui/plain
 */

import { PAST, VERB } from './layout.ts'
import { formatAttachment, type CardChanges, type CardLine, type Row } from './rows.ts'

/** Left gutter marking each row kind, wide enough to align in a fixed-width terminal. */
const GUTTER: Record<Row['kind'], string> = {
  'user': '>',
  'command': '/',
  'assistant': ' ',
  'reasoning': '·',
  'rate': '~',
  'tool-call': '⚙',
  'tool-group': '⚙',
  'job-done': '◌',
  'tool-result': '←',
  'notice': '!',
}

/**
 * Render one row as a single plain-text line, with no ANSI styling. A step's
 * group of calls renders a line per call.
 *
 * @param row - the row to render.
 * @returns the line, or the group's lines, without a trailing newline.
 */
export function formatRow(row: Row): string {
  const gutter = GUTTER[row.kind]
  switch (row.kind) {
    case 'user':
      return `${gutter} ${oneLine([row.text, ...(row.attachments ?? []).map(formatAttachment)].filter(Boolean).join('\n'))}`
    case 'command':
      return `${gutter} ${row.name}${oneLine(row.args)}`
    case 'tool-call': {
      const call = `${gutter} ${row.tool} [${row.callId}](${oneLine(row.input)})${cardText(row.detail)}`
      const nested = (row.dispatches ?? []).map(child => `  ${formatRow(child)}`).join('\n')
      const ended = row.result === undefined ? call
        : `${call} ${GUTTER['tool-result']} ${outcomeText(row.result.ok, row.result.title, row.result.text, row.result.detail, row.result.changes)}`.trimEnd()
      return nested === '' ? ended : `${ended}\n${nested}`
    }
    case 'tool-group':
      return row.calls.map(formatRow).join('\n')
    case 'job-done':
      // A call's grammar, its bracket naming the job instead of the call.
      return `${gutter} ${row.tool}${row.id === undefined ? '' : ` [${row.id}]`}(${oneLine(row.label)}) ${row.status}`
    case 'rate':
      return `${gutter} ${row.tokens} tokens · ${row.ms} ms`
    case 'tool-result':
      // A card leaves `text` empty, so its title and lines are the whole
      // result here. This surface is one line already, so it needs no bound.
      return `${gutter} ${outcomeText(row.ok, row.title, row.text, row.detail, row.changes, row.callId)}`.trimEnd()
    default:
      return `${gutter} ${oneLine(row.text)}`
  }
}

/**
 * A result's outcome and output on one line.
 * @param ok - whether the call succeeded.
 * @param title - the card's headline, when it declared one.
 * @param text - raw output, empty when a card replaced it.
 * @param detail - the card's lines.
 * @param changes - the files a command changed, if any.
 * @param callId - the call it answers, named when it stands apart from it.
 * @returns the outcome text.
 */
function outcomeText(ok: boolean, title: string | undefined, text: string, detail: readonly CardLine[] | undefined,
  changes: CardChanges | undefined, callId?: string): string {
  const parts = [ok ? 'done' : 'error', callId === undefined ? undefined : `[${callId}]`, [title, text]
    .map(part => oneLine(part ?? '')).filter(part => part !== '').join(' ')]
  return `${parts.filter(part => part !== undefined && part !== '').join(' ')}${cardText([...detail ?? [], ...changeLines(changes)])}`
}

/**
 * A command's changed files as card lines. Each path after `edited`, with
 * how it changed and, when it has no lines, its size; then its lines and the
 * section's caveats. This surface has no bound, so it has no count of files
 * left out by one, and no words for those the producer left out.
 *
 * @param changes - the files a command changed, if any.
 * @returns the lines, empty when there are none.
 */
function changeLines(changes: CardChanges | undefined): readonly CardLine[] {
  if (changes === undefined) return []
  const size = (added: number, removed: number): string =>
    [added === 0 ? '' : `+${added}`, removed === 0 ? '' : `\u2212${removed}`].filter(part => part !== '').join(' ')
  return [
    ...changes.files.flatMap(file => [
      { text: [`${PAST[VERB.edit]} ${file.path}`, file.status ?? '', file.lines.length === 0 ? size(file.added, file.removed) : '']
        .filter(part => part !== '').join('  ') },
      ...file.lines,
    ]),
    ...(changes.notes ?? []).map(text => ({ text })),
  ]
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
