/**
 * What the launcher does with an unhandled rejection once startup has
 * committed: record it, log it, and show it once per distinct error, while the
 * session continues.
 *
 * Cordis `emit` drops the promises its listeners return, and any plugin can
 * forget one, so a single stray rejection would otherwise end a long session.
 * The process guard keeps such rejections fatal until readiness, where a
 * failed plugin is a failed launch; afterwards it hands them here.
 * @module bake-cli/late-rejections
 */

import { appendFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import type { AppRejection } from 'bake-cmdline'

/** Distinct rejections recorded and shown per {@link LATE_REJECTION_WINDOW_MS}; later ones in the window are counted. */
export const LATE_REJECTION_BURST = 5

/** The sliding window, in milliseconds, that {@link LATE_REJECTION_BURST} applies to. */
export const LATE_REJECTION_WINDOW_MS = 60_000

/** Distinct errors remembered as already shown; past this, the oldest is forgotten and could be shown again. */
const REMEMBERED_ERRORS = 256
const MAX_NAME = 100
const MAX_SUMMARY = 200
const MAX_MESSAGE = 2_000
const MAX_STACK = 16_000
/** C0 and C1 control characters, which could move the cursor or restyle a terminal that prints them. */
const CONTROL = /[\u0000-\u001f\u007f-\u009f]+/gu

/** One line of a rejection record file. Times are milliseconds; nothing beyond the error's own text is kept. */
export interface LateRejectionRecord {
  readonly time: string
  readonly kind: 'unhandled-rejection'
  readonly pid: number
  readonly uptimeMs: number
  /** The reason's name, message, and stack, each bounded. A non-Error reason keeps only its primitive value or type tag. */
  readonly error: { readonly name: string; readonly message: string; readonly stack?: string }
  /** Rejections left unrecorded since the previous record: repeats of an error already recorded, and distinct ones beyond the burst. */
  readonly suppressed: number
}

/** Where a late rejection goes; the launcher wires each to the running app. */
export interface LateRejectionOptions {
  /** Absolute directory for this process's record file, created owner-only on the first record. */
  readonly directory: string
  /**
   * Offer the rejection to a surface that can show it without disturbing its own output.
   * @returns true when a surface showed it.
   */
  readonly present: (rejection: AppRejection) => boolean
  /** Warning sink for the normal logger; receives finished text, not a format string. */
  readonly warn: (message: string) => void
  /** Where the fallback warning line goes when no surface showed the rejection. */
  readonly stderr: { write(chunk: string): unknown }
  /** Monotonic milliseconds since process start; the rate limit and `uptimeMs` read it. */
  readonly now?: () => number
  readonly pid?: number
}

/**
 * Create the handler the launcher passes to the fail-loud guard at readiness.
 *
 * The first occurrence of each distinct error, judged by its name, message, and
 * top stack frame, is appended to `rejections.<YYYYMMDD>.<HHMMSS>.<pid>.jsonl` in
 * `directory`, logged, and offered to `present`; when no surface shows it, one
 * `dsh: warning:` line goes to `stderr`. At most {@link LATE_REJECTION_BURST}
 * distinct errors are handled per {@link LATE_REJECTION_WINDOW_MS}. Repeats and
 * rejections beyond the burst are counted into the next record's `suppressed`.
 * The record is written synchronously, so one that arrives during disposal is
 * on disk before the process exits. Every step contains its own failure: the
 * handler never throws and never changes the exit status.
 * @param options - record directory, presentation, logging, and fallback output.
 * @returns the rejection handler.
 */
export function createLateRejectionReporter(options: LateRejectionOptions): (reason: unknown) => void {
  const now = options.now ?? (() => performance.now())
  const pid = options.pid ?? process.pid
  const shown = new Set<string>()
  const recent: number[] = []
  let suppressed = 0
  let file: string | undefined
  return (reason) => {
    const error = describe(reason)
    const key = `${error.name}\0${error.message}\0${topFrame(error.stack)}`
    const at = now()
    for (let oldest = recent[0]; oldest !== undefined && at - oldest >= LATE_REJECTION_WINDOW_MS; oldest = recent[0]) {
      recent.shift()
    }
    if (shown.has(key) || recent.length >= LATE_REJECTION_BURST) {
      suppressed++
      return
    }
    recent.push(at)
    shown.add(key)
    if (shown.size > REMEMBERED_ERRORS) {
      const forgotten = shown.values().next().value
      if (forgotten !== undefined) shown.delete(forgotten)
    }
    const record: LateRejectionRecord = {
      time: new Date().toISOString(), kind: 'unhandled-rejection', pid, uptimeMs: Math.round(at), error, suppressed,
    }
    suppressed = 0
    const summary = oneLine(`${error.name}: ${error.message}`, MAX_SUMMARY)
    const path = file ??= join(options.directory, `rejections.${fileStamp(new Date())}.${pid}.jsonl`)
    let written = false
    try {
      mkdirSync(options.directory, { recursive: true, mode: 0o700 })
      appendFileSync(path, JSON.stringify(record) + '\n', { mode: 0o600 })
      written = true
    } catch (writeError) {
      contain(() => { options.warn(`could not write ${path}: ${oneLine(stringOf(writeError), MAX_SUMMARY)}`) })
    }
    contain(() => {
      options.warn(`unhandled rejection after startup: ${summary}; the session continues${written ? `; recorded in ${path}` : ''}`)
    })
    const rejection: AppRejection = written ? { summary, record: path } : { summary }
    let presented = false
    try {
      presented = options.present(rejection)
    } catch {
      // A surface that failed to show it leaves the fallback line to do so.
    }
    if (!presented) contain(() => { options.stderr.write(warningLine(rejection)) })
  }
}

/**
 * The fallback line for a rejection no surface showed.
 * @param rejection - the recorded rejection.
 * @returns one `dsh: warning:` line, newline included.
 */
export function warningLine(rejection: AppRejection): string {
  const where = rejection.record === undefined ? '' : `; details in ${rejection.record}`
  return `dsh: warning: unhandled rejection after startup: ${rejection.summary}; the session continues${where}\n`
}

/** Keep a reporting step's failure from reaching the process handler that called it. */
function contain(step: () => void): void {
  try {
    step()
  } catch {
    // The record or the other channels still carry the rejection.
  }
}

/**
 * The parts of a rejection reason a record may keep. An error-like reason keeps
 * its name, message, and stack; any other enumerable property may hold request
 * headers or file contents, so none is read. A non-error object keeps only its
 * type tag.
 */
function describe(reason: unknown): LateRejectionRecord['error'] {
  if (typeof reason === 'object' && reason !== null) {
    const { name, message, stack } = reason as { name?: unknown; message?: unknown; stack?: unknown }
    if (typeof message === 'string') {
      return {
        name: bounded(typeof name === 'string' && name !== '' ? name : 'Error', MAX_NAME),
        message: bounded(message, MAX_MESSAGE),
        ...typeof stack === 'string' ? { stack: bounded(stack, MAX_STACK) } : {},
      }
    }
    return { name: 'Non-error rejection', message: Object.prototype.toString.call(reason) }
  }
  return { name: 'Non-error rejection', message: bounded(stringOf(reason), MAX_MESSAGE) }
}

/** The first stack line below the message, which tells two throw sites with the same message apart. */
function topFrame(stack: string | undefined): string {
  return stack?.split('\n').find(line => line.trimStart().startsWith('at ')) ?? ''
}

function bounded(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`
}

/** The first line, with control characters folded to spaces, for a terminal or a log line. */
function oneLine(text: string, max: number): string {
  return bounded((text.split('\n', 1)[0] ?? '').replace(CONTROL, ' ').trim(), max)
}

function stringOf(value: unknown): string {
  try {
    return value instanceof Error ? value.message : String(value)
  } catch {
    // A null-prototype object or a throwing toString has no string form.
    return typeof value
  }
}

/** `YYYYMMDD.HHMMSS` in local time, the stamp the runtime watchdog and Node's reports use in this directory. */
function fileStamp(date: Date): string {
  const pad = (value: number): string => String(value).padStart(2, '0')
  return `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}`
    + `.${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
}
