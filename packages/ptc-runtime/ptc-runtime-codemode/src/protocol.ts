/**
 * Private agreements between the host provider and its worker entry: the
 * header that carries per-run limits into the worker, the crash signals the
 * worker reports, and the program-relative rendering of QuickJS stacks. Pure
 * and erasable, so Node's type stripping can load it in the worker during
 * source runs.
 * @module bake-ptc-runtime-codemode/protocol
 */

/**
 * Source prefix pi-codemode 1.0.0 places before the script it evaluates as
 * `codemode.js`. Program columns on line 1 are offset by it; the exact pin in
 * this package's manifest keeps the value current.
 */
export const CODEMODE_SCRIPT_PREFIX = '(async (tools, console) => {'

/** Location shown for program frames in rendered diagnostics. */
const PROGRAM_LOCATION = '<program>'

/**
 * `crash` text the worker reports when the program's console output, or its
 * completion or failure diagnostic, would cross the run's output budget.
 */
export const OUTPUT_LIMIT_SIGNAL = 'dsh-ptc-runtime-codemode: output budget exceeded'

/** `crash` text the worker reports when one binding call's arguments cannot fit the message budget. */
export const MESSAGE_LIMIT_SIGNAL = 'dsh-ptc-runtime-codemode: binding message budget exceeded'

/** Where the program body sits in the source pi-codemode evaluates. */
export interface ProgramLayout {
  /** Line count of the stripped program; later lines belong to the runtime's own code. */
  readonly lines: number
  /** One-based column, in the evaluated script, of the program's first character on line 1. */
  readonly column: number
}

/** Per-run values the host passes to its worker entry ahead of the program source. */
export interface WorkerHeader extends ProgramLayout {
  /** Combined serialized logs and completion or diagnostic byte budget. */
  readonly maxOutputBytes: number
  /** Serialized byte budget for one binding call's arguments. */
  readonly maxMessageBytes: number
}

const HEADER_START = '/*dsh-ptc-codemode:'
const HEADER_END = '*/'
const HEADER_FIELDS = ['maxOutputBytes', 'maxMessageBytes', 'lines', 'column'] as const

/**
 * Prefix the source with the worker header. The worker removes it before
 * pi-codemode evaluates the source, so program positions are unaffected.
 * @param header - per-run values for the worker.
 * @param code - source for pi-codemode.
 * @returns the source the host passes to pi-codemode.
 */
export function withWorkerHeader(header: WorkerHeader, code: string): string {
  return `${HEADER_START}${HEADER_FIELDS.map(field => header[field]).join(':')}${HEADER_END}${code}`
}

/**
 * Split the worker header from the source the worker received.
 * @param source - the source pi-codemode passed to the worker.
 * @returns the header and the source without it, or undefined when it is missing or malformed.
 */
export function readWorkerHeader(source: string): { header: WorkerHeader; code: string } | undefined {
  if (!source.startsWith(HEADER_START)) return undefined
  const end = source.indexOf(HEADER_END, HEADER_START.length)
  if (end === -1) return undefined
  const parts = source.slice(HEADER_START.length, end).split(':')
  if (parts.length !== HEADER_FIELDS.length || !parts.every(part => /^\d+$/.test(part))) return undefined
  const [maxOutputBytes, maxMessageBytes, lines, column] = parts.map(Number) as [number, number, number, number]
  if (![maxOutputBytes, maxMessageBytes, lines, column].every(Number.isSafeInteger)) return undefined
  return { header: { maxOutputBytes, maxMessageBytes, lines, column }, code: source.slice(end + HEADER_END.length) }
}

/**
 * A script failure as pi-codemode reports it: an `Error`'s fields, or only
 * `message` for other thrown values. The program controls these fields, so
 * they are untyped JSON.
 */
export interface ScriptFailure {
  readonly name?: unknown
  readonly message?: unknown
  readonly stack?: unknown
}

/** A QuickJS frame position in the script pi-codemode evaluates as `codemode.js`. */
const FRAME_LOCATION = /codemode\.js:(\d+):(\d+)/

/**
 * Render one script failure for the model: the error's `Name: message` head
 * and its program frames, with positions relative to the submitted program.
 * Frames in the runtime's own wrapper and binding code are dropped.
 * @param failure - the failure pi-codemode reported.
 * @param layout - where the program sits in the evaluated script.
 * @returns the stack-shaped diagnostic, or the message for a thrown non-error.
 */
export function programFailureText(failure: ScriptFailure, layout: ProgramLayout): string {
  const message = typeof failure.message === 'string' ? failure.message : String(failure.message)
  const stack = failure.stack
  if (typeof stack !== 'string') return message
  const head = message ? `${String(failure.name)}: ${message}` : String(failure.name)
  if (stack === head) return head
  if (!stack.startsWith(`${head}\n`)) return stack
  const frames: string[] = []
  for (const frame of stack.slice(head.length + 1).split('\n')) {
    const match = FRAME_LOCATION.exec(frame)
    if (match === null) {
      frames.push(frame)
      continue
    }
    const line = Number(match[1])
    let column = Number(match[2])
    if (line > layout.lines || (line === 1 && column < layout.column)) continue
    if (line === 1) column -= layout.column - 1
    frames.push(`${frame.slice(0, match.index)}${PROGRAM_LOCATION}:${line}:${column}${frame.slice(match.index + match[0].length)}`)
  }
  return [head, ...frames].join('\n')
}
