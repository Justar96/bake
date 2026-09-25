/** Markdown to terminal text and styled runs. No ANSI, I/O, or renderer state. */
import { fromMarkdown } from 'mdast-util-from-markdown'
import { gfmFromMarkdown } from 'mdast-util-gfm'
import { gfm } from 'micromark-extension-gfm'
import type { Nodes, Root } from 'mdast'
import type { Highlight, Span, Tone } from './present.ts'
import { PALETTE } from './palette.ts'

export interface MarkdownLine {
  readonly text: string
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
 * blank line. Lists, tables, and fences stay together, including their blank
 * lines. Reference syntax stays literal, so a later definition cannot change
 * text already printed. Offsets refer to the original, unsanitized source.
 */
export function finishedMarkdown(source: string): number {
  const nodes = parse(source).children
  const last = nodes.at(-1)
  if (last === undefined) return 0
  const end = last.position!.end.offset!
  const tail = source.slice(end)
  const settled = (last.type === 'paragraph' && /^(?:\r?\n)[ \t]*(?:\r?\n)/.test(tail))
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
 * stacked labeled cells. A wide value stays readable in a narrow terminal.
 * Unfinished syntax stays readable while the live region waits for the rest
 * of its block.
 */
export function markdownLines(source: string, tone: Tone = 'plain', code?: Highlight): readonly MarkdownLine[] {
  if (source.trim() === '') return []
  const tree = parse(source)
  const lines: MarkdownLine[] = []
  let text = ''
  let spans: Span[] = []
  const base: Emphasis = { tone }
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
  const inline = (node: Nodes, style: Emphasis = base, depth = 0): void => {
    if (depth > 64) { write(raw(node), style); return }
    switch (node.type) {
      case 'strong': style = { ...style, bold: true }; break
      case 'emphasis': style = { ...style, italic: true }; break
      case 'delete': style = { ...style, strikethrough: true }; break
      case 'inlineCode': write(node.value, { ...style, bold: true }); return
      case 'break': push(); return
      case 'link': {
        const reference = tone === 'thought' ? style : { ...style, color: PALETTE.reference }
        for (const child of node.children) inline(child, { ...reference, underline: true }, depth + 1)
        const label = node.children.map(child => child.type === 'text' ? child.value : '').join('')
        if (label !== node.url) write(` (${node.url})`, reference)
        return
      }
      case 'image': {
        const reference = tone === 'thought' ? style : { ...style, color: PALETTE.reference }
        write(`${node.alt || '[]'} (${node.url})`, reference); return
      }
      case 'linkReference': case 'imageReference': write(raw(node), style); return
    }
    if ('children' in node) for (const child of node.children) inline(child, style, depth + 1)
    else if ('value' in node) write(node.value, style)
    else write(raw(node), style)
  }
  const prefix = (from: number, first: string, rest = first): void => {
    for (let at = from; at < lines.length; at++) {
      const line = lines[at]!
      const lead = at === from ? first : rest
      lines[at] = { ...line, text: lead + line.text,
        ...line.spans === undefined ? {} : { spans: [{ length: lead.length, tone }, ...line.spans] } }
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
          ? { ...base, bold: true, ...tone === 'thought' ? {} : { color: PALETTE.reference } } : base)
        push(); return
      case 'code': {
        const sourceLines = safe(node.value).split('\n')
        const language = node.lang?.split(/\s/)[0]
        if (language) { write(language, { tone: tone === 'thought' ? tone : 'quiet' }); push() }
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
          prefix(from, marker + check, ' '.repeat(marker.length + check.length))
        }
        return
      case 'table': {
        const [header, ...rows] = node.children
        if (header === undefined) return
        for (const [index, row] of rows.entries()) {
          if (index > 0) lines.push({ text: '' })
          for (const [column, cell] of row.children.entries()) {
            const label = header.children[column]
            if (label) inline(label, { ...base, bold: true })
            write(': '); inline(cell); push()
          }
        }
        if (rows.length === 0) {
          for (const [index, cell] of header.children.entries()) {
            if (index > 0) write(' | ')
            inline(cell, { ...base, bold: true })
          }
          push()
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
