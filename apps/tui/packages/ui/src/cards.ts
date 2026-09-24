/**
 * Tool render intents, correlated to their calls and mapped to card lines.
 *
 * `dsh-tools` lets a tool say how one call presents without knowing which
 * surface draws it: `presentCall`/`presentResult` return a `card`-tagged
 * intent, and each consumer maps the cards it understands. This module is the
 * terminal's half of that seam — the one place a `card` becomes display lines.
 *
 * The presenters are pure over `args` and the durable result, so a replayed
 * session log reproduces the identical card. A tool the registry no longer
 * knows, one that declares no presenter, and a card this build does not
 * recognize all fall back to the raw arguments and result text, which is the
 * presentation every tool had before this seam existed.
 *
 * @module @dsh-tui/ui/cards
 */

import type { FileDiff, ToolCallView, ToolResult, ToolResultView } from '@deepseek-ai/dsh-tools'
import { diffArrays, diffWordsWithSpace } from 'diff'
import type { TuiCopy } from './copy.ts'
import type { CardLine } from './rows.ts'
import { outputLines, toolText } from './tool-output.ts'

/**
 * The presentation half of a registered tool.
 *
 * Structurally a `ToolDefinition`, narrowed to the two optional methods this
 * surface calls, so the projection needs no tool registry to be tested.
 */
export interface ToolPresenters {
  presentCall?(args: unknown): ToolCallView | undefined
  presentResult?(args: unknown, result: ToolResult): ToolResultView | undefined
}

/** Resolve a tool's presenters by the name the session log recorded. */
export type ToolLookup = (name: string) => ToolPresenters | undefined

/** What one card contributes to a row: a headline, and lines under it. */
export interface Card {
  /** Replaces the row's headline — the tool's own words for what this call is. */
  readonly title: string
  /** Lines shown under the headline, in display order. */
  readonly detail: readonly CardLine[]
}

/**
 * Calls retained while awaiting their result.
 *
 * A result names only its `callId`, so its arguments have to be held from the
 * call that produced them, and a tool's arguments can be as large as a file it
 * writes. An entry is released the moment its result arrives; this bound only
 * catches calls that never got one, which is what an interrupted turn leaves
 * behind.
 */
const RETAINED_CALLS = 64

/** One terminal's view of the tool calls in one session. */
export class ToolCards {
  private readonly pending = new Map<string, { readonly tool: string, readonly args: unknown, readonly title?: string }>()

  /**
   * @param lookup - resolves a recorded tool name to its presenters.
   * @param copy - localized labels for the counts and caps a card reports.
   */
  constructor(private readonly lookup: ToolLookup, private readonly copy: TuiCopy) {}

  /**
   * Present a pending call, retaining its arguments for the result.
   *
   * @param callId - the call's identifier, as the session log recorded it.
   * @param tool - the called tool's registered name.
   * @param args - the raw arguments JSON exactly as the model produced it.
   * @returns the card, or undefined to keep the raw arguments.
   */
  call(callId: string, tool: string, args: string): Card | undefined {
    const parsed = parseArguments(args)
    if (this.pending.size >= RETAINED_CALLS) this.pending.delete(this.pending.keys().next().value!)
    if (parsed === undefined) return undefined
    const view = attempt(() => this.lookup(tool)?.presentCall?.(parsed.value))
    const card = view === undefined ? undefined : callCard(view)
    this.pending.set(callId, { tool, args: parsed.value, ...card === undefined ? {} : { title: card.title } })
    return card
  }

  /**
   * Present a completed call, releasing the arguments it was holding.
   *
   * @param callId - the identifier the result carries back from its call.
   * @param result - the durable result projection, including any tool `meta`.
   * @returns the card, or undefined to keep the raw result text.
   */
  result(callId: string, result: ToolResult): Card | undefined {
    const call = this.pending.get(callId)
    this.pending.delete(callId)
    if (call === undefined) return undefined
    const view = attempt(() => this.lookup(call.tool)?.presentResult?.(call.args, result))
    return view === undefined ? undefined : resultCard(view, this.copy, call.title)
  }
}

/**
 * Parse recorded tool arguments.
 *
 * The model produces this JSON, so the log can hold text that does not parse
 * and a presenter narrows its own input from the result. A wrapper
 * distinguishes a failed parse from a parsed `undefined`.
 *
 * @param args - the raw arguments string from `tool/call`.
 * @returns the parsed value, or undefined when the text is not JSON.
 */
function parseArguments(args: string): { readonly value: unknown } | undefined {
  try {
    return { value: JSON.parse(args) }
  } catch {
    // Malformed model output. Nothing is lost by not reporting it: the raw
    // string renders, so the reader sees exactly what the model sent.
    return undefined
  }
}

/**
 * Call a presenter without letting it take the transcript down.
 *
 * Presenters are declared pure, but they narrow arguments the model wrote and
 * are registered by plugins — including tools defined at runtime. A thrown
 * presenter costs its card, not the session.
 *
 * @param present - the presenter invocation.
 * @returns the view, or undefined when none was produced or it threw.
 */
function attempt<T>(present: () => T | undefined): T | undefined {
  try {
    return present()
  } catch {
    // A tool's own presentation failing is not the reader's problem, and this
    // layer has nowhere to report it: the raw arguments and result text remain.
    return undefined
  }
}

/**
 * Map a pending-call intent to its card.
 *
 * @param view - the render intent the tool declared for this call.
 * @returns the headline and lines under it.
 */
function callCard(view: ToolCallView): Card {
  switch (view.card) {
    case 'terminal':
      return { title: view.title, detail: view.description === undefined ? [] : [{ text: view.description }] }

    case 'diff':
      // The applied hunks arrive with the result. Showing the proposed change
      // here too would print every hunk twice in a transcript that scrolls.
      return { title: view.title, detail: [] }

    case 'generic':
      return { title: view.title, detail: [...blockLines(view.content), ...rawInputLines(view.rawInput, view.title)] }

    default:
      // A card this build does not know: tools are registered by plugins, and
      // one may declare an intent newer than this surface. The title is the
      // part every card has, so it is the part that still renders.
      return { title: titleOf(view), detail: [] }
  }
}

/**
 * Map a completed-call intent to its card.
 *
 * @param view - the render intent the tool declared for this result.
 * @param copy - localized labels for counts and caps.
 * @param called - the call's own title, which a result need not repeat.
 * @returns the headline and lines under it; an empty title keeps the call's.
 */
function resultCard(view: ToolResultView, copy: TuiCopy, called?: string): Card {
  const title = view.title ?? ''
  switch (view.card) {
    case 'terminal': {
      const status = view.signal !== undefined ? `signal ${view.signal}`
        : view.exitCode !== undefined && view.exitCode !== 0 ? `exit ${view.exitCode}`
          : undefined
      return { title, detail: [
        ...outputLines(view.output ?? ''),
        // A zero exit says only what the absence of a failure already says.
        ...status === undefined ? [] : [{ text: status, summary: 'failure' as const }],
      ] }
    }

    case 'diff':
      return { title, detail: diffLines(view.diffs) }

    case 'read': {
      const last = view.lines.at(-1)?.number
      const width = String(last ?? view.offset).length
      const partial = view.lines.length < view.totalLines
      return { title, detail: [
        ...view.lines.map((line, index) => ({
          text: `${String(line.number).padStart(width)}  ${toolText(line.text)}`,
          source: view.lang ?? view.path, codeOffset: width + 2, number: line.number,
          ...index === 0 ? { codeStart: true } : {},
        })),
        // Only a window says which window it is; a whole file needs no caption.
        ...partial ? [{ text: `${view.offset}–${last ?? view.offset}/${view.totalLines} ${copy.cardLines}`, summary: 'count' as const }] : [],
      ] }
    }

    case 'search':
      return { title, detail: view.shape === 'matches' ? matchLines(view.files, view.total, view.truncated, copy)
        : [...view.paths.map(path => ({ text: path })), countLine(view.total, [copy.cardPath, copy.cardPaths], view.truncated, copy)] }

    case 'web':
      return { title, detail: view.kind === 'fetch'
        ? [
            // The status is the outcome; the address only when a redirect
            // made it other than the one the call's title already names.
            { text: `${view.statusCode}${view.truncated ? ` (${copy.cardTruncated})` : ''}`, summary: view.statusCode >= 400 ? 'failure' : 'count' },
            ...view.url === called ? [] : [{ text: view.url }],
          ]
        : [
            ...view.answer === undefined ? [] : textLines(view.answer),
            ...view.sources.map(source => ({ text: [source.url, source.title].filter(part => part !== undefined && part !== '').join('  ') })),
            countLine(view.sources.length, [copy.cardSource, copy.cardSources], view.truncated, copy),
          ] }

    case 'generic':
      return { title, detail: blockLines(view.content) }

    default:
      // As for a call: an unknown card keeps whatever title it carries, and
      // the raw result text below it is unaffected.
      return { title: titleOf(view), detail: [] }
  }
}

/**
 * The sign and marker text of one change line, and the gap between two changes.
 *
 * `⋯` stands for the unchanged lines between two hunks, or between two changes
 * inside one: they are what a reader skips to find the edit, and the line
 * numbers already say how far apart the changes are.
 */
const SIGN = { added: '+', removed: '-' } as const
const GAP: CardLine = { text: '⋯', emphasis: 'gap' }

/**
 * Longest pair of lines compared word by word. Past it, a line is marked
 * changed whole: a minified bundle rewritten in one edit costs a word diff
 * quadratic in its length, and nobody reads its words.
 */
const WORD_DIFF_LIMIT = 2000

/**
 * Lines for an edit: what changed, numbered, and nothing it only surrounds.
 *
 * The contract carries each hunk as a whole before and after text with its
 * context, which is what a patch needs and not what a reader of the
 * transcript does: the three lines above a change push the change itself
 * below the preview bound. Only changed lines are drawn, each numbered on its
 * own side, and `⋯` separates changes the context lines stood between. A path
 * heads each file's lines only when the edit touched more than one, since
 * the card's title names the file otherwise.
 *
 * @param diffs - the applied hunks, in order, or a whole-file create.
 * @returns the change lines.
 */
function diffLines(diffs: readonly FileDiff[]): readonly CardLine[] {
  const several = new Set(diffs.map(diff => diff.path)).size > 1
  const lines: CardLine[] = []
  let path: string | undefined
  for (const diff of diffs) {
    const changes = hunkLines(diff)
    if (changes.length === 0) continue
    if (several && diff.path !== path) lines.push({ text: diff.path })
    else if (lines.length > 0) lines.push(GAP)
    path = diff.path
    lines.push(...changes)
  }
  return lines
}

/**
 * One hunk's changed lines.
 *
 * A hunk can hold several changes a few context lines apart, which a patch
 * joins into one hunk when their context overlaps, so its sides are diffed
 * line by line rather than trimmed at each end: trimming marked every line
 * between the first change and the last as rewritten.
 *
 * @param diff - one applied hunk.
 * @returns its removed and added lines, `⋯` between separate changes.
 */
function hunkLines(diff: FileDiff): readonly CardLine[] {
  const after = textLines(diff.newText).map(line => line.text)
  if (diff.oldText === null) return after.map((text, index) => changeLine('added', text, diff.path, diff.newStart ?? 1, index))
  const before = textLines(diff.oldText).map(line => line.text)
  const lines: CardLine[] = []
  let removed: string[] = []
  let added: string[] = []
  // Offsets into each side, which number its lines from the hunk's start.
  let old = 0
  let now = 0
  let apart = false
  const flush = (): void => {
    if (removed.length === 0 && added.length === 0) return
    if (apart && lines.length > 0) lines.push(GAP)
    const pairs = removed.map((text, index) => index < added.length ? wordRanges(text, added[index]!) : undefined)
    lines.push(
      ...removed.map((text, index) => changeLine('removed', text, diff.path, diff.oldStart, old - removed.length + index, pairs[index]?.removed)),
      ...added.map((text, index) => changeLine('added', text, diff.path, diff.newStart, now - added.length + index, pairs[index]?.added)))
    removed = []
    added = []
    apart = false
  }
  for (const part of diffArrays(before, after)) {
    if (part.removed) {
      removed.push(...part.value)
      old += part.count
    } else if (part.added) {
      added.push(...part.value)
      now += part.count
    } else {
      flush()
      apart = true
      old += part.count
      now += part.count
    }
  }
  flush()
  return lines
}

/**
 * One changed line, with its sign, number, and source.
 * @param side - which side of the change it is on.
 * @param text - the source line.
 * @param path - the file it is from.
 * @param start - the side's first line number, absent when unknown.
 * @param offset - the line's offset from `start`.
 * @param changed - the runs of the source line the change touched.
 * @returns the card line; offsets shifted past the sign.
 */
function changeLine(side: 'added' | 'removed', text: string, path: string, start: number | undefined, offset: number,
  changed?: readonly (readonly [number, number])[]): CardLine {
  return {
    text: `${SIGN[side]} ${text}`, emphasis: side, source: path,
    ...start === undefined ? {} : { number: start + offset },
    ...changed === undefined || changed.length === 0 ? {} : { changed: changed.map(([from, to]) => [from + 2, to + 2] as const) },
  }
}

/**
 * Which words of an edited line changed, on each side.
 *
 * A removed line paired with the added line in its place is usually the same
 * line with a few words changed, and those words are the edit; marking them
 * lets the eye skip the rest. A pair that shares less than half of the
 * shorter line was replaced rather than edited, and marking nearly every word
 * says nothing the sign does not.
 *
 * @param before - the removed line.
 * @param after - the added line in its place.
 * @returns the changed runs of each, or undefined when the pair is a replacement.
 */
function wordRanges(before: string, after: string): { readonly removed: readonly (readonly [number, number])[], readonly added: readonly (readonly [number, number])[] } | undefined {
  if (before.length + after.length > WORD_DIFF_LIMIT) return undefined
  const removed: [number, number][] = []
  const added: [number, number][] = []
  const extend = (ranges: [number, number][], from: number, to: number): void => {
    const last = ranges.at(-1)
    if (last !== undefined && last[1] === from) last[1] = to
    else ranges.push([from, to])
  }
  let old = 0
  let now = 0
  let kept = 0
  for (const part of diffWordsWithSpace(before, after)) {
    const length = part.value.length
    if (part.removed) {
      extend(removed, old, old + length)
      old += length
    } else if (part.added) {
      extend(added, now, now + length)
      now += length
    } else {
      kept += part.value.trim().length
      old += length
      now += length
    }
  }
  return kept * 2 < Math.min(before.trim().length, after.trim().length) ? undefined : { removed, added }
}

/**
 * Lines for content matches grouped by file, indented under each path.
 *
 * @param files - matched lines grouped by file, in first-seen order.
 * @param total - matches found before any cap.
 * @param truncated - whether the groups carry only the retained matches.
 * @param copy - localized labels.
 * @returns the grouped lines followed by the count.
 */
function matchLines(files: readonly { path: string, matches: readonly { lineNumber: number, line: string }[] }[],
  total: number, truncated: boolean, copy: TuiCopy): readonly CardLine[] {
  const width = String(Math.max(0, ...files.flatMap(file => file.matches.map(match => match.lineNumber)))).length
  return [
    ...files.flatMap(file => [
      { text: file.path },
      ...file.matches.map((match, index) => ({
        text: `  ${String(match.lineNumber).padStart(width)}  ${toolText(match.line)}`,
        source: file.path, codeOffset: width + 4, number: match.lineNumber,
        ...index === 0 ? { codeStart: true } : {},
      })),
    ]),
    countLine(total, [copy.cardMatch, copy.cardMatches], truncated, copy),
  ]
}

/**
 * The line reporting how much a search found, and whether it was capped.
 *
 * A capped result must never read as a complete one, which is the whole reason
 * the presentation contract carries `truncated` beside the total.
 *
 * @param total - items found before any cap.
 * @param noun - localized names for one of what was counted, and for several.
 * @param truncated - whether the listed items are only the retained ones.
 * @param copy - localized labels.
 * @returns the count line, the card's summary.
 */
function countLine(total: number, noun: readonly [one: string, many: string], truncated: boolean, copy: TuiCopy): CardLine {
  return { text: `${total} ${noun[total === 1 ? 0 : 1]}${truncated ? ` (${copy.cardTruncated})` : ''}`, summary: 'count' }
}

/**
 * Text of a view's UI-facing content blocks.
 *
 * A block is markdown, and a presenter that shows output verbatim wraps it in
 * a fence, as a failed command's is. This surface draws output as it is
 * already, so the fence is the one piece of markdown it removes: drawn, it is
 * two lines of backticks around the error the reader came for.
 *
 * @param content - the blocks a presenter supplied, if any.
 * @returns one line per line of text across the blocks.
 */
function blockLines(content: readonly { type: string, text?: string }[] | undefined): readonly CardLine[] {
  if (content === undefined) return []
  return content.filter(block => block.type === 'text').flatMap(block => {
    const { text, language } = unfenced(block.text ?? '')
    return outputLines(text, language)
  })
}

/**
 * A block that is one fenced code block, as its code.
 * @param text - a markdown block.
 * @returns the contents and language hint, or unchanged text when it is not one fence.
 */
function unfenced(text: string): { readonly text: string, readonly language?: string } {
  const fence = /^(`{3,}|~{3,})([^\n`]*)\n([\s\S]*?)\n?\1[ \t]*\n?$/.exec(text)
  if (fence === null || fence[3]!.includes(fence[1]!)) return { text }
  const language = fence[2]!.trim().split(/\s/)[0]
  return { text: fence[3]!, ...language === undefined || language === '' || /^(?:text|plaintext|console|output)$/.test(language) ? {} : { language } }
}

/**
 * A card's salient raw input, as lines.
 *
 * A value the title already names, as a search's pattern is, is left out:
 * under its own title it is the same words again. A structured value is drawn
 * a field or an item to a line, compactly, rather than as indented JSON that
 * spends a row on every brace.
 *
 * @param rawInput - the value a generic view chose to show, if any.
 * @param title - the card's title.
 * @returns the lines to show, empty when the view showed none.
 */
function rawInputLines(rawInput: unknown, title: string): readonly CardLine[] {
  if (rawInput === undefined) return []
  if (typeof rawInput === 'string') return title.includes(rawInput.trim()) ? [] : textLines(rawInput)
  if (Array.isArray(rawInput)) return rawInput.map(item => ({ text: compact(item) }))
  if (typeof rawInput === 'object' && rawInput !== null) {
    return Object.entries(rawInput).map(([key, value]) => ({ text: `${key}: ${compact(value)}` }))
  }
  return [{ text: compact(rawInput) }]
}

/**
 * A value on one line: a string as itself, a record of plain values as its
 * values two spaces apart, as a task list's items read, and anything else as
 * compact JSON.
 */
function compact(value: unknown): string {
  const plain = (item: unknown): boolean => item === null || ['string', 'number', 'boolean'].includes(typeof item)
  const text = typeof value === 'string' ? value
    : typeof value === 'object' && value !== null && !Array.isArray(value) && Object.values(value).every(plain)
      ? Object.values(value).map(String).filter(item => item !== '').join('  ')
      : JSON.stringify(value) ?? String(value)
  return text.replace(/\s*\n\s*/g, ' ')
}

/**
 * Split text into card lines, dropping a trailing newline's empty line.
 *
 * @param text - the text to split.
 * @returns one line per line of text, empty for empty text.
 */
function textLines(text: string): readonly CardLine[] {
  if (text === '') return []
  const lines = text.split('\n')
  return (lines.at(-1) === '' ? lines.slice(0, -1) : lines).map(line => ({ text: line }))
}

/**
 * Title of a card whose kind this build does not recognize.
 *
 * @param view - the unrecognized view.
 * @returns its title, or the empty string when it carries none.
 */
function titleOf(view: object): string {
  return 'title' in view && typeof view.title === 'string' ? view.title : ''
}
