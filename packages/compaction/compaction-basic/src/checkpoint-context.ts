/**
 * Deterministic working-state sections appended to a compaction checkpoint:
 * the files the compacted span read and modified. Both derive only from the
 * span's own messages, including a prior checkpoint's sections inside it, so
 * the checkpoint text in the durable `user/message` stays the single record
 * the model sees. A section a prior checkpoint carries that this module does
 * not read, such as an older checkpoint's `<todo-list>`, is not carried forward.
 *
 * @module @deepseek-ai/dsh-compaction-basic/checkpoint-context
 */

import { isCompactCheckpointSource } from 'bake-compaction'
import type { ContentBlock, Message, TextBlock } from '@deepseek-ai/dsh-llm'

/** Most paths listed per file section; older paths beyond it are counted, not listed. */
export const MAX_LISTED_FILES = 50
/** Longest path listed; a longer argument is not a path worth carrying. */
const MAX_PATH_CHARS = 500

const READ_TOOLS: ReadonlySet<string> = new Set(['read', 'read_image'])
const MODIFY_TOOLS: ReadonlySet<string> = new Set(['write', 'edit'])

const READ_OPEN = '<read-files>'
const READ_CLOSE = '</read-files>'
const MODIFIED_OPEN = '<modified-files>'
const MODIFIED_CLOSE = '</modified-files>'
/** Closing tag of the summary block; sections are parsed only after it. */
const SUMMARY_CLOSE = '</compacted-summary>'

/** Working state gathered from one compacted span. */
export interface CheckpointContext {
  /** Paths only read, oldest touch first. */
  readonly readFiles: readonly string[]
  /** Paths written or edited, oldest touch first. */
  readonly modifiedFiles: readonly string[]
}

/**
 * Ordered path set whose order is the most recent touch: touching a path
 * again moves it to the end, so a capped listing keeps the newest paths.
 */
class RecencySet {
  private readonly order = new Map<string, true>()

  touch(path: string): void {
    this.order.delete(path)
    this.order.set(path, true)
  }

  has(path: string): boolean {
    return this.order.has(path)
  }

  values(): string[] {
    return [...this.order.keys()]
  }
}

/**
 * Collect the compacted span's file state. A prior checkpoint inside the span
 * contributes its own sections first, so the lists carry forward across
 * successive compactions; the span's later successful `read`, `read_image`,
 * `write`, and `edit` calls then update them. Calls whose results are errors,
 * and unparseable arguments, are ignored.
 * @param messages - the span's derived messages in surface order, without the system head.
 * @returns the read-only and modified paths.
 */
export function collectCheckpointContext(messages: readonly Message[]): CheckpointContext {
  const failed = new Set<string>()
  for (const message of messages) {
    for (const block of message.content) {
      if (block.type === 'tool-result' && block.isError === true) failed.add(block.toolCallId)
    }
  }

  const read = new RecencySet()
  const modified = new RecencySet()
  for (const message of messages) {
    if (message.role === 'user' && isCompactCheckpointSource(message.source)) {
      const prior = parseCheckpointSections(message.content)
      for (const path of prior.readFiles) read.touch(path)
      for (const path of prior.modifiedFiles) modified.touch(path)
      continue
    }
    if (message.role !== 'assistant') continue
    for (const block of message.content) {
      if (block.type !== 'tool-call' || failed.has(block.id)) continue
      const args = parseArguments(block.arguments)
      if (args === undefined) continue
      const path = filePath(args['file_path'])
      if (path === undefined) continue
      if (READ_TOOLS.has(block.name)) read.touch(path)
      else if (MODIFY_TOOLS.has(block.name)) modified.touch(path)
    }
  }

  return {
    readFiles: read.values().filter(path => !modified.has(path)),
    modifiedFiles: modified.values(),
  }
}

/**
 * Render the checkpoint sections that follow the summary block, or nothing
 * when the span touched no file.
 * @param context - state collected from the compacted span.
 * @returns zero or one text block holding every non-empty section.
 */
export function formatCheckpointContext(context: CheckpointContext): TextBlock[] {
  const sections: string[] = []
  if (context.readFiles.length > 0) sections.push(fileSection(READ_OPEN, READ_CLOSE, context.readFiles))
  if (context.modifiedFiles.length > 0) {
    sections.push(fileSection(MODIFIED_OPEN, MODIFIED_CLOSE, context.modifiedFiles))
  }
  return sections.length === 0 ? [] : [{ type: 'text', text: sections.join('\n\n') }]
}

/** One file section listing the newest paths, prefixed by a count of older omitted ones. */
function fileSection(open: string, close: string, paths: readonly string[]): string {
  const kept = paths.slice(-MAX_LISTED_FILES)
  const omitted = paths.length - kept.length
  const lines = omitted > 0 ? [`... ${omitted} earlier paths not shown`, ...kept] : kept
  return `${open}\n${lines.join('\n')}\n${close}`
}

/**
 * Read the file sections a prior checkpoint appended after its summary block.
 * Omission lines, malformed entries, and other sections are skipped.
 * @param content - the prior checkpoint message's content.
 * @returns the carried read and modified paths.
 */
function parseCheckpointSections(content: readonly ContentBlock[]): {
  readFiles: string[]
  modifiedFiles: string[]
} {
  const text = content.map(block => block.type === 'text' ? block.text : '').join('\n')
  const closeAt = text.lastIndexOf(SUMMARY_CLOSE)
  const tail = closeAt === -1 ? '' : text.slice(closeAt + SUMMARY_CLOSE.length)
  return {
    readFiles: pathLines(sectionLines(tail, READ_OPEN, READ_CLOSE)),
    modifiedFiles: pathLines(sectionLines(tail, MODIFIED_OPEN, MODIFIED_CLOSE)),
  }
}

/** Lines between one section's tags, or `undefined` when the section is absent. */
function sectionLines(text: string, open: string, close: string): string[] | undefined {
  const start = text.indexOf(`${open}\n`)
  if (start === -1) return undefined
  const end = text.indexOf(`\n${close}`, start)
  if (end === -1) return undefined
  return text.slice(start + open.length + 1, end).split('\n')
}

/** Keep listed paths, dropping omission lines and blanks. */
function pathLines(lines: readonly string[] | undefined): string[] {
  return (lines ?? []).filter(line => line.length > 0 && !line.startsWith('... '))
    .map(line => filePath(line))
    .filter((path): path is string => path !== undefined)
}

/** Parse logged tool-call arguments, which are raw model JSON and never schema-checked on replay. */
function parseArguments(raw: string): Record<string, unknown> | undefined {
  try {
    const value: unknown = JSON.parse(raw)
    return typeof value === 'object' && value !== null && !Array.isArray(value)
      ? value as Record<string, unknown>
      : undefined
  } catch {
    return undefined
  }
}

/** A listable path: a trimmed single-line string of bounded length. */
function filePath(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const path = value.trim()
  if (path.length === 0 || path.length > MAX_PATH_CHARS || /[\r\n]/.test(path) || path.startsWith('... ')) {
    return undefined
  }
  return path
}
