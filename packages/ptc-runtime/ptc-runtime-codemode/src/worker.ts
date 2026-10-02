/**
 * Worker thread entry. It reads the per-run header, meters every message the
 * pi-codemode worker posts to the host, then starts that worker. pi-codemode
 * keeps output in host memory without a limit, so the budget is enforced here,
 * before data crosses the thread boundary: text output is charged against the
 * combined output budget, image output is dropped, oversized binding arguments
 * and completions stop the run, and nothing passes once the run is stopped.
 *
 * Erasable TypeScript with relative `.ts` imports only, so source runs load it
 * through Node's type stripping and builds bundle it as `lib/worker.js`.
 * @module @deepseek-ai/dsh-ptc-runtime-codemode/worker
 */

import { parentPort, workerData } from 'node:worker_threads'
import { OutputLedger, jsonStringBytesUpTo } from './output.ts'
import { MESSAGE_LIMIT_SIGNAL, OUTPUT_LIMIT_SIGNAL, programFailureText, readWorkerHeader } from './protocol.ts'
import type { ScriptFailure, WorkerHeader } from './protocol.ts'

/** The pi-codemode 1.0.0 worker-to-host messages this entry inspects. */
type WorkerMessage =
  | { type: 'output'; item: { type: string; text?: string } }
  | { type: 'call'; args?: string }
  | { type: 'done'; ok: boolean; value?: string; error?: string }
  | { type: 'crash'; message: string }

/** Fields of pi-codemode's worker data this entry reads or rewrites. */
interface CodemodeWorkerData {
  code: string
  interrupt: SharedArrayBuffer
}

/** Bytes `[0,` and `]` add around a completion value. */
const COMPLETION_ENVELOPE_BYTES = 4

/**
 * Wrap the port so only metered messages reach the host.
 * @param port - this worker's parent port.
 * @param header - the run's limits and program layout.
 * @param interrupt - pi-codemode's VM interrupt flag.
 */
function meter(port: NonNullable<typeof parentPort>, header: WorkerHeader, interrupt: Int32Array): void {
  const post = port.postMessage.bind(port)
  const ledger = new OutputLedger(header.maxOutputBytes)
  let stopped = false
  const stop = (signal: string): void => {
    stopped = true
    post({ type: 'crash', message: signal })
    // Stops a script still running in the VM; the host then terminates this thread.
    Atomics.store(interrupt, 0, 1)
  }
  const fitsCompletion = (message: Extract<WorkerMessage, { type: 'done' }>): boolean => {
    if (message.ok) {
      return message.value === undefined || Buffer.byteLength(message.value) - COMPLETION_ENVELOPE_BYTES <= ledger.remaining
    }
    if (message.error === undefined) return true
    const failure = JSON.parse(message.error) as ScriptFailure
    return jsonStringBytesUpTo(programFailureText(failure, header), ledger.remaining) !== undefined
  }
  const completion = (message: Extract<WorkerMessage, { type: 'done' }>): string | undefined => {
    try {
      return fitsCompletion(message) ? undefined : OUTPUT_LIMIT_SIGNAL
    } catch {
      // The host parses this failure without a guard; it must never receive one it cannot parse.
      return 'worker reported a malformed failure'
    }
  }
  port.postMessage = (value: unknown): void => {
    if (stopped) return
    const message = value as WorkerMessage
    switch (message.type) {
      case 'output': {
        if (message.item.type !== 'text' || message.item.text === undefined) return
        const text = message.item.text
        if (ledger.admit(text)) {
          post(message)
          return
        }
        const prefix = ledger.fittingPrefix(text)
        if (prefix.length > 0) post({ type: 'output', item: { type: 'text', text: prefix } })
        stop(OUTPUT_LIMIT_SIGNAL)
        return
      }
      case 'call':
        // Each argument is at most the budget and each declared member name is
        // host-chosen, so twice the budget is beyond any call the host accepts.
        if (message.args !== undefined && Buffer.byteLength(message.args) > 2 * header.maxMessageBytes) {
          stop(MESSAGE_LIMIT_SIGNAL)
          return
        }
        post(message)
        return
      case 'done': {
        const signal = completion(message)
        if (signal !== undefined) {
          stop(signal)
          return
        }
        post(message)
        return
      }
      case 'crash':
        // Only stop() may report the limits; a VM fault that happens to carry the same text does not.
        post(message.message === OUTPUT_LIMIT_SIGNAL || message.message === MESSAGE_LIMIT_SIGNAL
          ? { type: 'crash', message: `worker fault: ${message.message}` }
          : message)
        return
      default:
        post(message)
    }
  }
}

const port = parentPort
const data = workerData as CodemodeWorkerData
const parsed = readWorkerHeader(data.code)
if (port !== null) {
  if (parsed === undefined) {
    port.postMessage({ type: 'crash', message: 'worker started without a run header' })
  } else {
    data.code = parsed.code
    meter(port, parsed.header, new Int32Array(data.interrupt))
    await import('@earendil-works/pi-codemode/worker')
  }
}
