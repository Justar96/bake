/** Markdown to terminal text and styled runs. No ANSI, I/O, or renderer state. */
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import { fromMarkdown } from 'mdast-util-from-markdown'
import { gfmFromMarkdown } from 'mdast-util-gfm'
import { gfm } from 'micromark-extension-gfm'
import type { Nodes, Root } from 'mdast'
import type { Highlight, Span, Tone } from './present.ts'
import { PALETTE } from './palette.ts'

export interface MarkdownLine {
  readonly text: string
  /** Source offset of the table row, shared across its grid and stacked layouts. */
  readonly tableRow?: number
  readonly spans?: readonly Span[]
  /** Code whitespace must not be rewritten as prose soft breaks. */
  readonly literal?: boolean
}

const parse = (source: string): Root => fromMarkdown(source, {
  // References can be defined after their use. Keep definitions literal so a
  // later chunk cannot restyle a paragraph that has already printed.
  extensions: [gfm(), { disable: { null: ['definition', 'gfmFootnoteDefinition', 'gfmFootnoteCall'] } }],
  mdastExtensions: [gfmFromMarkdown()],
})

/**
 * Offset where a prefix's Markdown blocks can no longer be extended by another delta.
 *
 * The final block waits for a successor. A paragraph also settles after a
 * blank line; a fenced code block settles after its closing line ends. Lists
 * and tables stay together, including their blank lines. Reference syntax stays
 * literal, so a later definition cannot change
 * text already printed. Offsets refer to the original, unsanitized source.
 */
export function finishedMarkdown(source: string): number {
  const nodes = parse(source).children
  const last = nodes.at(-1)
  if (last === undefined) return 0
  const end = last.position!.end.offset!
  const tail = source.slice(end)
  const fenced = last.type === 'code' ? source.slice(last.position!.start.offset!, end).split(/\r?\n/) : []
  const opening = /^ {0,3}(`{3,}|~{3,})/.exec(fenced[0] ?? '')?.[1]
  const closing = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(fenced.at(-1) ?? '')?.[1]
  const closedFence = fenced.length > 1 && opening !== undefined && closing !== undefined
    && opening[0] === closing[0] && closing.length >= opening.length
  const settled = (last.type === 'paragraph' && /^(?:\r?\n)[ \t]*(?:\r?\n)/.test(tail))
    || (closedFence && /^\r?\n/.test(tail))
    || ((last.type === 'heading' || last.type === 'thematicBreak') && /^\r?\n/.test(tail))
  const node = settled ? last : nodes.at(-2)
  if (node === undefined) return 0
  const stop = node.position!.end.offset!
  // Consume only the newline that ends this block. Paragraph spacing belongs
  // to the next chunk, as it does when the complete message is replayed.
  return stop + (source.slice(stop).startsWith('\r\n') ? 2 : source[stop] === '\n' ? 1 : 0)
}

/** Keep provider text from emitting terminal controls, including decoded entities. */
const safe = (text: string): string => text.replace(/\r\n?/g, '\n').replace(/\t/g, '    ')
  .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g, char => `\\x${char.charCodeAt(0).toString(16).padStart(2, '0')}`)

type Emphasis = Omit<Span, 'length'>

/**
 * Render CommonMark and GFM without terminal escapes.
 *
 * Links keep their target. HTML and references stay literal. Tables use
 * aligned columns when width permits, otherwise stacked labeled cells.
 * A wide value stays readable in a narrow terminal.
 * Unfinished syntax stays readable while the live region waits for the rest
 * of its block.
 */
export function markdownLines(source: string, tone: Tone = 'plain', code?: Highlight, width?: number): readonly MarkdownLine[] {
  if (source.trim() === '') return []
  const tree = parse(source)
  const lines: MarkdownLine[] = []
  let text = ''
  let spans: Span[] = []
  const base: Emphasis = { tone }
  const quiet: Emphasis = { tone: tone === 'thought' ? tone : 'quiet' }
  const labelStyle: Emphasis = { ...base, bold: true, ...tone === 'thought' ? {} : { color: PALETTE.reference } }
  const raw = (node: Nodes): string => source.slice(node.position?.start.offset, node.position?.end.offset)
  const push = (literal = false): void => {
    const styled = spans.some(span => span.tone !== tone || span.bold || span.italic || span.underline || span.strikethrough || span.color)
    lines.push({ text, ...styled ? { spans } : {}, ...literal ? { literal: true } : {} })
    text = ''; spans = []
  }
  const write = (value: string, style: Emphasis = base): void => {
    for (const [index, part] of safe(value).split('\n').entries()) {
      if (index > 0) push()
      if (part === '') continue
      text += part
      spans.push({ ...style, length: part.length })
    }
  }
  const labelText = (node: Nodes, depth = 0): string => {
    if (depth > 64) return raw(node)
    if (node.type === 'image') {
      const alt = node.alt || '[]'
      return safe(alt) === safe(node.url) ? alt : `${alt} (${node.url})`
    }
    if ('children' in node) return node.children.map(child => labelText(child, depth + 1)).join('')
    return 'value' in node ? node.value : raw(node)
  }
  const inline = (node: Nodes, style: Emphasis = base, depth = 0): void => {
    if (depth > 64) { write(raw(node), style); return }
    switch (node.type) {
      case 'strong': style = { ...style, bold: true }; break
      case 'emphasis': style = { ...style, italic: true }; break
      case 'delete': style = { ...style, strikethrough: true }; break
      case 'inlineCode': write(node.value, { ...style, bold: true, ...tone === 'thought' ? {} : { color: PALETTE.code } }); return
      case 'break': push(); return
      case 'link': {
        const reference = tone === 'thought' ? style : { ...style, color: PALETTE.reference }
        for (const child of node.children) inline(child, { ...reference, underline: true }, depth + 1)
        const label = node.children.map(child => labelText(child)).join('')
        if (safe(label) !== safe(node.url)) write(` (${node.url})`, quiet)
        return
      }
      case 'image': {
        const reference = tone === 'thought' ? style : { ...style, color: PALETTE.reference }
        const alt = node.alt || '[]'
        write(alt, reference)
        if (safe(alt) !== safe(node.url)) write(` (${node.url})`, quiet)
        return
      }
      case 'linkReference': case 'imageReference': write(raw(node), style); return
    }
    if ('children' in node) for (const child of node.children) inline(child, style, depth + 1)
    else if ('value' in node) write(node.value, style)
    else write(raw(node), style)
  }
  const prefix = (from: number, first: string, rest = first, firstSpans: readonly Span[] = [{ ...quiet, length: first.length }]): void => {
    for (let at = from; at < lines.length; at++) {
      const line = lines[at]!
      const lead = at === from ? first : rest
      lines[at] = { ...line, text: lead + line.text,
        spans: [...at === from ? firstSpans : [{ ...quiet, length: lead.length }],
          ...line.spans ?? [{ ...base, length: line.text.length }]] }
    }
  }
  const blocks = (nodes: readonly Nodes[], depth = 0): void => {
    let previous: Nodes | undefined
    for (const node of nodes) {
      // Keep the source's paragraph spacing. Do not draw a box around every block.
      if (previous !== undefined && node.position!.start.line > previous.position!.end.line + 1) lines.push({ text: '' })
      block(node, depth)
      previous = node
    }
  }
  const block = (node: Nodes, depth: number): void => {
    if (depth > 64) { write(raw(node)); push(); return }
    switch (node.type) {
      case 'paragraph': case 'heading':
        inline(node, node.type === 'heading'
          ? labelStyle : base)
        push(); return
      case 'code': {
        const sourceLines = safe(node.value).split('\n')
        const language = node.lang?.split(/\s/)[0]
        if (language) { write(language, { ...quiet, bold: true }); push() }
        const tokens = language ? code?.(sourceLines, language) : undefined
        for (const [index, value] of sourceLines.entries()) {
          write('  ')
          let offset = 0
          for (const token of tokens?.[index] ?? []) {
            write(value.slice(offset, offset + token.length), { tone,
              ...token.color === undefined ? {} : { color: token.color },
              ...token.italic ? { italic: true } : {} })
            offset += token.length
          }
          write(value.slice(offset)); push(true)
        }
        return
      }
      case 'blockquote': {
        const from = lines.length
        blocks(node.children, depth + 1); prefix(from, '> '); return
      }
      case 'list':
        for (const [index, item] of node.children.entries()) {
          if (index > 0 && node.spread) lines.push({ text: '' })
          const from = lines.length
          blocks(item.children, depth + 1)
          if (from === lines.length) lines.push({ text: '' })
          const marker = node.ordered ? `${(node.start ?? 1) + index}. ` : '- '
          const check = item.checked === null || item.checked === undefined ? '' : item.checked ? '[x] ' : '[ ] '
          prefix(from, marker + check, ' '.repeat(marker.length + check.length), [
            { ...quiet, length: marker.length },
            ...check === '' ? [] : [{ ...quiet, ...item.checked && tone !== 'thought' ? { tone, color: PALETTE.done } : {}, length: check.length }],
          ])
        }
        return
      case 'table': {
        const [header, ...rows] = node.children
        if (header === undefined) return
        const rectangular = depth === 0 && width !== undefined
          && node.children.every(row => row.children.length === header.children.length)
          && header.children.every(cell => labelText(cell).trim() !== '')
        if (rectangular) {
          const cells = node.children.map((row, index) => row.children.map(cell => {
            const from = lines.length
            inline(cell, index === 0 ? labelStyle : base); push()
            return lines.splice(from)
          }))
          const grid = tableGrid(cells, node.align ?? [], width, tone)
          if (grid !== undefined) {
            for (const [index, row] of grid.entries()) {
              lines.push(...row.map(line => ({ ...line, tableRow: node.children[index]!.position!.start.offset! })))
            }
            return
          }
        }
        for (const [index, row] of rows.entries()) {
          if (index > 0) lines.push({ text: '' })
          const from = lines.length
          for (let column = 0; column < Math.max(header.children.length, row.children.length); column++) {
            const label = header.children[column]
            const cell = row.children[column]
            if (label) inline(label, labelStyle)
            write(': ')
            if (cell) inline(cell)
            push()
          }
          for (let at = from; at < lines.length; at++) lines[at] = { ...lines[at]!, tableRow: row.position!.start.offset! }
        }
        if (rows.length === 0) {
          for (const [index, cell] of header.children.entries()) {
            if (index > 0) write(' | ', quiet)
            inline(cell, labelStyle)
          }
          push()
          lines[lines.length - 1] = { ...lines.at(-1)!, tableRow: header.position!.start.offset! }
        }
        return
      }
      case 'thematicBreak': write('---'); push(); return
      default: write(raw(node)); push()
    }
  }
  // A continued chunk starts with the separator `finishedMarkdown` left behind.
  if (/^(?:[ \t]*\r?\n)/.test(source)) lines.push({ text: '' })
  blocks(tree.children)
  return lines
}

/** Slice UTF-16 runs with their text when a reasoning preview wraps or clips. */
export function sliceSpans(spans: readonly Span[], from: number, to: number): readonly Span[] {
  const kept: Span[] = []
  let offset = 0
  for (const span of spans) {
    const end = offset + span.length
    const length = Math.min(to, end) - Math.max(from, offset)
    if (length > 0) kept.push({ ...span, length })
    offset = end
    if (offset >= to) break
  }
  return kept
}

/** Fit cells before composing physical lines, so padding never changes their style offsets. */
function tableGrid(
  rows: readonly (readonly (readonly MarkdownLine[])[])[],
  align: readonly ('left' | 'right' | 'center' | null)[],
  width: number,
  tone: Tone,
): readonly (readonly MarkdownLine[])[] | undefined {
  const columns = rows[0]?.length ?? 0
  if (columns === 0) return undefined
  const natural: number[] = Array.from({ length: columns }, () => 1)
  for (const row of rows) for (const [column, cell] of row.entries()) {
    for (const line of cell) natural[column] = Math.max(natural[column]!, stringWidth(line.text))
  }
  const widths = natural.map(size => Math.min(size, 12))
  let remaining = Math.floor(width) - 3 * (columns - 1) - widths.reduce((sum, size) => sum + size, 0)
  if (remaining < 0) return undefined
  // Distribute spare cells without stretching a column beyond its content.
  while (remaining > 0) {
    const growing = widths.flatMap((size, index) => size < natural[index]! ? [index] : [])
    if (growing.length === 0) break
    const share = Math.max(1, Math.floor(remaining / growing.length))
    for (const index of growing) {
      const added = Math.min(share, remaining, natural[index]! - widths[index]!)
      widths[index]! += added; remaining -= added
    }
  }
  const quiet: Emphasis = { tone: tone === 'thought' ? tone : 'quiet' }
  const wrapped = (cell: readonly MarkdownLine[], cells: number): MarkdownLine[] => cell.flatMap(line => {
    let offset = 0
    return wrapAnsi(line.text, cells, { hard: true, trim: false }).split('\n').map(text => {
      const spans = line.spans === undefined ? [{ tone, length: text.length }] : sliceSpans(line.spans, offset, offset + text.length)
      offset += text.length
      return { text, spans }
    })
  })
  return rows.map((row, index) => {
    const cells = row.map((cell, column) => wrapped(cell, widths[column]!))
    const result: MarkdownLine[] = []
    const height = Math.max(...cells.map(cell => cell.length))
    for (let at = 0; at < height; at++) {
      let text = ''
      const spans: Span[] = []
      for (const [column, cell] of cells.entries()) {
        if (column > 0) { text += ' | '; spans.push({ ...quiet, length: 3 }) }
        const line = cell[at] ?? { text: '' }
        const padding = Math.max(0, widths[column]! - stringWidth(line.text))
        const left = align[column] === 'right' ? padding : align[column] === 'center' ? Math.floor(padding / 2) : 0
        text += ' '.repeat(left) + line.text + ' '.repeat(padding - left)
        if (left > 0) spans.push({ ...quiet, length: left })
        spans.push(...line.spans ?? [{ tone, length: line.text.length }])
        if (padding > left) spans.push({ ...quiet, length: padding - left })
      }
      result.push({ text, spans, literal: true })
    }
    if (index === 0) {
      const rule = widths.map(size => '-'.repeat(size)).join('-+-')
      result.push({ text: rule, spans: [{ ...quiet, length: rule.length }], literal: true })
    }
    return result
  })
}
