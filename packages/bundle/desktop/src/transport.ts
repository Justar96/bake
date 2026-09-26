/**
 * The bridge's two carriers for the same protocol messages: the Electron
 * utility process's message port when the desktop launches Bake, or
 * newline-delimited stdin/stdout under plain Node (tests and benchmarks).
 * @module @deepseek-ai/dsh-desktop/transport
 */

import { createInterface } from 'node:readline'
import {
  CORE_MESSAGE_TYPES,
  type CoreMessage,
  type HarnessMessage,
  MAX_LINE_BYTES,
  PROTOCOL_VERSION,
} from './protocol.ts'

/** The Electron `process.parentPort` surface the bridge uses. */
export interface ParentPortLike {
  on(event: 'message', listener: (event: { data: unknown }) => void): unknown
  postMessage(message: unknown): void
}

/** Receives decoded inbound messages and refused lines. */
export interface TransportHandlers {
  onMessage(message: CoreMessage): void
  onInvalid(reason: string): void
  /** The host closed its side (stdin ended); the bridge should exit. */
  onClose(): void
}

/** An open carrier; `send` never throws. */
export interface Transport {
  readonly kind: 'port' | 'stdio'
  send(message: HarnessMessage): void
  close(): void
}

const CORE_TYPES = new Set<string>(CORE_MESSAGE_TYPES)

/**
 * Decodes one inbound line. The envelope is checked here; handlers check the
 * fields they read.
 * @returns the message, or a reason string when the line is refused.
 */
export function decodeCoreLine(line: string): CoreMessage | string {
  if (Buffer.byteLength(line) > MAX_LINE_BYTES) return 'line exceeds the protocol size limit'
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch (error: unknown) {
    return `invalid JSON: ${error instanceof Error ? error.message : String(error)}`
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return 'message is not an object'
  const { v, type } = value as { v?: unknown; type?: unknown }
  if (v !== PROTOCOL_VERSION) return `unsupported protocol version ${String(v)}`
  if (typeof type !== 'string' || !CORE_TYPES.has(type)) return `unknown message type ${String(type)}`
  return value as CoreMessage
}

function dispatch(line: string, handlers: TransportHandlers): void {
  if (line.trim() === '') return
  const decoded = decodeCoreLine(line)
  if (typeof decoded === 'string') handlers.onInvalid(decoded)
  else handlers.onMessage(decoded)
}

/**
 * Opens the message-port carrier when Electron provides one, else stdio.
 * @param handlers - inbound callbacks.
 * @param io - process streams and port; injectable for tests.
 */
export function openTransport(
  handlers: TransportHandlers,
  io: {
    parentPort?: ParentPortLike | undefined
    stdin?: NodeJS.ReadableStream
    stdout?: { write(chunk: string): unknown }
  } = {},
): Transport {
  const port = 'parentPort' in io
    ? io.parentPort
    : (process as unknown as { parentPort?: ParentPortLike }).parentPort
  if (port !== undefined) {
    let open = true
    port.on('message', ({ data }) => {
      if (!open) return
      if (typeof data === 'string') dispatch(data, handlers)
      else handlers.onInvalid('port message is not a string')
    })
    return {
      kind: 'port',
      send: (message) => {
        if (open) port.postMessage(JSON.stringify(message))
      },
      close: () => {
        open = false
      },
    }
  }

  const stdin = io.stdin ?? process.stdin
  const stdout = io.stdout ?? process.stdout
  const lines = createInterface({ input: stdin, crlfDelay: Infinity })
  let open = true
  lines.on('line', (line) => {
    if (open) dispatch(line, handlers)
  })
  lines.on('close', () => {
    if (open) handlers.onClose()
  })
  return {
    kind: 'stdio',
    send: (message) => {
      if (open) stdout.write(`${JSON.stringify(message)}\n`)
    },
    close: () => {
      open = false
      lines.close()
    },
  }
}
