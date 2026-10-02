/**
 * Deterministic working-state sections appended to a compaction checkpoint:
 * the files the compacted span read and modified, and its latest open todo
 * list. Both derive only from the span's own messages, including a prior
 * checkpoint's sections inside it, so the checkpoint text in the durable
 * `user/message` stays the single record the model sees.
 *
 * @module @deepseek-ai/dsh-compaction-basic/checkpoint-context
 */

import { isCompactCheckpointSource } from '@deepseek-ai/dsh-compaction'
import type { ContentBlock, Message, TextBlock } from '@deepseek-ai/dsh-llm'

/** Most paths listed per file section; older paths beyond it are counted, not listed. */
export const MAX_LISTED_FILES = 50
/** Longest path listed; a longer argument is not a path worth carrying. */
const MAX_PATH_CHARS = 500
/** Most todo items listed. */
export const MAX_TODO_ITEMS = 50
/** Longest todo line kept, in characters. */
const MAX_TODO_CHARS = 300

const READ_TOOLS: ReadonlySet<string> = new Set(['read', 'read_image'])
const MODIFY_TOOLS: ReadonlySet<string> = new Set(['write', 'edit'])
const TODO_TOOL = 'todo_write'
const TODO_STATUSES: ReadonlySet<string> = new Set(['pending', 'in_progress', 'completed'])

const READ_OPEN = '<read-files>'
const READ_CLOSE = '</read-files>'
const MODIFIED_OPEN = '<modified-files>'
const MODIFIED_CLOSE = '</modified-files>'
const TODO_OPEN = '<todo-list>'
const TODO_CLOSE = '</todo-list>'
/** Closing tag of the summary block; sections are parsed only after it. */
const SUMMARY_CLOSE = '</compacted-summary>'

/** One todo entry as the checkpoint carries it. */
export interface CheckpointTodo {
  readonly content: string
  readonly status: 'pending' | 'in_progress' | 'completed'
}

/** Working state gathered from one compacted span. */
export interface CheckpointContext {
  /** Paths only read, oldest touch first. */
  readonly readFiles: readonly string[]
  /** Paths written or edited, oldest touch first. */
  readonly modifiedFiles: readonly string[]
  /** The span's latest todo list; `undefined` when none or none left open. */
  readonly todos: readonly CheckpointTodo[] | undefined
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
 * Collect the compacted span's file and todo state. A prior checkpoint inside
 * the span contributes its own sections first, so the lists carry forward
 * across successive compactions; the span's later successful `read`,
 * `read_image`, `write`, `edit`, and `todo_write` calls then update them.
 * Calls whose results are errors, and unparseable arguments, are ignored.
 * @param messages - the span's derived messages in surface order, without the system head.
 * @returns the read-only paths, modified paths, and latest open todo list.
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
  let todos: CheckpointTodo[] | undefined
  for (const message of messages) {
    if (message.role === 'user' && isCompactCheckpointSource(message.source)) {
      const prior = parseCheckpointSections(message.content)
      for (const path of prior.readFiles) read.touch(path)
      for (const path of prior.modifiedFiles) modified.touch(path)
      todos = prior.todos
      continue
    }
    if (message.role !== 'assistant') continue
    for (const block of message.content) {
      if (block.type !== 'tool-call' || failed.has(block.id)) continue
      const args = parseArguments(block.arguments)
      if (args === undefined) continue
      if (block.name === TODO_TOOL) {
        const list = todoList(args['todos'])
        if (list !== undefined) todos = list
        continue
      }
      const path = filePath(args['file_path'])
      if (path === undefined) continue
      if (READ_TOOLS.has(block.name)) read.touch(path)
      else if (MODIFY_TOOLS.has(block.name)) modified.touch(path)
    }
  }

  return {
    readFiles: read.values().filter(path => !modified.has(path)),
    modifiedFiles: modified.values(),
    todos: todos !== undefined && todos.some(todo => todo.status !== 'completed') ? todos : undefined,
  }
}

/**
 * Render the checkpoint sections that follow the summary block, or nothing
 * when the span touched no file and left no open todo.
 * @param context - state collected from the compacted span.
 * @returns zero or one text block holding every non-empty section.
 */
export function formatCheckpointContext(context: CheckpointContext): TextBlock[] {
  const sections: string[] = []
  if (context.readFiles.length > 0) sections.push(fileSection(READ_OPEN, READ_CLOSE, context.readFiles))
  if (context.modifiedFiles.length > 0) {
    sections.push(fileSection(MODIFIED_OPEN, MODIFIED_CLOSE, context.modifiedFiles))
  }
  if (context.todos !== undefined) {
    const items = context.todos.slice(0, MAX_TODO_ITEMS).map(todo => `- [${todo.status}] ${todo.content}`)
    const omitted = context.todos.length - items.length
    if (omitted > 0) items.push(`... ${omitted} more not shown`)
    sections.push(`${TODO_OPEN}\n${items.join('\n')}\n${TODO_CLOSE}`)
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
 * Read the sections a prior checkpoint appended after its summary block.
 * Omission lines and malformed entries are skipped.
 * @param content - the prior checkpoint message's content.
 * @returns the carried read paths, modified paths, and todo list.
 */
function parseCheckpointSections(content: readonly ContentBlock[]): {
  readFiles: string[]
  modifiedFiles: string[]
  todos: CheckpointTodo[] | undefined
} {
  const text = content.map(block => block.type === 'text' ? block.text : '').join('\n')
  const closeAt = text.lastIndexOf(SUMMARY_CLOSE)
  const tail = closeAt === -1 ? '' : text.slice(closeAt + SUMMARY_CLOSE.length)
  const todoLines = sectionLines(tail, TODO_OPEN, TODO_CLOSE)
  let todos: CheckpointTodo[] | undefined
  if (todoLines !== undefined) {
    todos = []
    for (const line of todoLines) {
      const match = /^- \[(pending|in_progress|completed)\] (.+)$/.exec(line)
      if (match !== null) todos.push({ status: match[1] as CheckpointTodo['status'], content: match[2] as string })
    }
  }
  return {
    readFiles: pathLines(sectionLines(tail, READ_OPEN, READ_CLOSE)),
    modifiedFiles: pathLines(sectionLines(tail, MODIFIED_OPEN, MODIFIED_CLOSE)),
    todos,
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

/**
 * Normalize a `todo_write` argument list the way the tool does: trimmed
 * non-empty single-line content and a known status. A malformed list is
 * ignored so it never replaces a valid earlier one.
 */
function todoList(value: unknown): CheckpointTodo[] | undefined {
  if (!Array.isArray(value)) return undefined
  const todos: CheckpointTodo[] = []
  for (const item of value) {
    if (typeof item !== 'object' || item === null) return undefined
    const { content, status } = item as { content?: unknown; status?: unknown }
    if (typeof content !== 'string' || typeof status !== 'string' || !TODO_STATUSES.has(status)) return undefined
    const line = content.replace(/\s+/g, ' ').trim()
    if (line.length === 0) return undefined
    todos.push({
      content: line.length > MAX_TODO_CHARS ? `${line.slice(0, MAX_TODO_CHARS - 1)}…` : line,
      status: status as CheckpointTodo['status'],
    })
  }
  return todos
}
