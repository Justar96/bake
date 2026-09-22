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
import type { TuiCopy } from './copy.ts'
import type { CardLine } from './rows.ts'

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
  private readonly pending = new Map<string, { readonly tool: string, readonly args: unknown }>()

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
    if (parsed !== undefined) this.pending.set(callId, { tool, args: parsed.value })
    if (parsed === undefined) return undefined
    const view = attempt(() => this.lookup(tool)?.presentCall?.(parsed.value))
    return view === undefined ? undefined : callCard(view)
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
    return view === undefined ? undefined : resultCard(view, this.copy)
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
      return { title: view.title, detail: [...blockLines(view.content), ...rawInputLines(view.rawInput)] }

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
 * @returns the headline and lines under it; an empty title keeps the call's.
 */
function resultCard(view: ToolResultView, copy: TuiCopy): Card {
  const title = view.title ?? ''
  switch (view.card) {
    case 'terminal': {
      const status = view.signal !== undefined ? `signal ${view.signal}`
        : view.exitCode !== undefined && view.exitCode !== 0 ? `exit ${view.exitCode}`
          : undefined
      return { title, detail: [
        ...textLines(view.output ?? ''),
        // A zero exit says only what the absence of a failure already says.
        ...status === undefined ? [] : [{ text: status }],
      ] }
    }

    case 'diff':
      return { title, detail: view.diffs.flatMap(diffDetail) }

    case 'read': {
      const last = view.lines.at(-1)?.number
      const width = String(last ?? view.offset).length
      const partial = view.lines.length < view.totalLines
      return { title, detail: [
        ...view.lines.map(line => ({ text: `${String(line.number).padStart(width)}  ${line.text}` })),
        // Only a window says which window it is; a whole file needs no caption.
        ...partial ? [{ text: `${view.offset}–${last ?? view.offset}/${view.totalLines} ${copy.cardLines}` }] : [],
      ] }
    }

    case 'search':
      return { title, detail: view.shape === 'matches' ? matchLines(view.files, view.total, view.truncated, copy)
        : [...view.paths.map(path => ({ text: path })), countLine(view.total, copy.cardPaths, view.truncated, copy)] }

    case 'web':
      return { title, detail: view.kind === 'fetch'
        ? [{ text: `${view.statusCode} ${view.url}${view.truncated ? ` (${copy.cardTruncated})` : ''}` }]
        : [
            ...view.answer === undefined ? [] : textLines(view.answer),
            ...view.sources.map(source => ({ text: [source.url, source.title].filter(part => part !== undefined && part !== '').join('  ') })),
            countLine(view.sources.length, copy.cardSources, view.truncated, copy),
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
 * Lines for one file's change, headed by its path.
 *
 * The contract carries a whole before and after text per hunk, context lines
 * included, so the two have to be told apart here: marking every line changed
 * would report a three-line edit as a rewrite of the surrounding function.
 *
 * The lines the two texts share at each end are the context the hunk was cut
 * with, and what lies between them is the change. That is exact for a hunk,
 * which is what a mutation tool sends: one contiguous edit with its context.
 * A pair that also matches inside the change widens the marked span rather
 * than splitting it, which overstates the edit but never misplaces it.
 *
 * @param diff - one applied hunk, or a whole-file create.
 * @returns the path line followed by the hunk's lines.
 */
function diffDetail(diff: FileDiff): readonly CardLine[] {
  const lines: CardLine[] = [{ text: diff.path }]
  const after = textLines(diff.newText).map(line => line.text)
  if (diff.oldText === null) {
    for (const text of after) lines.push({ text: `+ ${text}`, emphasis: 'added' })
    return lines
  }
  const before = textLines(diff.oldText).map(line => line.text)
  let head = 0
  while (head < before.length && head < after.length && before[head] === after[head]) head++
  let tail = 0
  while (tail < before.length - head && tail < after.length - head
    && before[before.length - 1 - tail] === after[after.length - 1 - tail]) tail++
  for (const text of before.slice(0, head)) lines.push({ text: `  ${text}` })
  for (const text of before.slice(head, before.length - tail)) lines.push({ text: `- ${text}`, emphasis: 'removed' })
  for (const text of after.slice(head, after.length - tail)) lines.push({ text: `+ ${text}`, emphasis: 'added' })
  for (const text of before.slice(before.length - tail)) lines.push({ text: `  ${text}` })
  return lines
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
      ...file.matches.map(match => ({ text: `  ${String(match.lineNumber).padStart(width)}  ${match.line}` })),
    ]),
    countLine(total, copy.cardMatches, truncated, copy),
  ]
}

/**
 * The line reporting how much a search found, and whether it was capped.
 *
 * A capped result must never read as a complete one, which is the whole reason
 * the presentation contract carries `truncated` beside the total.
 *
 * @param total - items found before any cap.
 * @param noun - localized name for what was counted.
 * @param truncated - whether the listed items are only the retained ones.
 * @param copy - localized labels.
 * @returns the count line.
 */
function countLine(total: number, noun: string, truncated: boolean, copy: TuiCopy): CardLine {
  return { text: `${total} ${noun}${truncated ? ` (${copy.cardTruncated})` : ''}` }
}

/**
 * Text of a view's UI-facing content blocks.
 *
 * @param content - the blocks a presenter supplied, if any.
 * @returns one line per line of text across the blocks.
 */
function blockLines(content: readonly { type: string, text?: string }[] | undefined): readonly CardLine[] {
  if (content === undefined) return []
  return content.filter(block => block.type === 'text').flatMap(block => textLines(block.text ?? ''))
}

/**
 * A card's salient raw input, as lines.
 *
 * @param rawInput - the value a generic view chose to show, if any.
 * @returns the lines to show, empty when the view showed none.
 */
function rawInputLines(rawInput: unknown): readonly CardLine[] {
  if (rawInput === undefined) return []
  return textLines(typeof rawInput === 'string' ? rawInput : JSON.stringify(rawInput, undefined, 2))
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
