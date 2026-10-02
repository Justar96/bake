/**
 * Outer-output accounting shared by the host and the worker thread: exact JSON
 * byte costs and the combined result ledger. Pure and erasable, so Node's type
 * stripping can load it in the worker during source runs.
 * @module @deepseek-ai/dsh-ptc-runtime-codemode/output
 */

import type { PtcJsonValue, PtcRunFailure, PtcRunResult } from '@deepseek-ai/dsh-ptc-runtime'

/** JSON's two-character escapes for control characters; every other control character takes six. */
const SHORT_ESCAPES = new Set([0x08, 0x09, 0x0a, 0x0c, 0x0d])

/** Serialized UTF-8 bytes one code point contributes inside a JSON string literal. */
function codePointBytes(codePoint: number): number {
  if (codePoint === 0x22 || codePoint === 0x5c) return 2
  if (codePoint < 0x20) return SHORT_ESCAPES.has(codePoint) ? 2 : 6
  if (codePoint < 0x80) return 1
  if (codePoint < 0x800) return 2
  // A lone surrogate serializes as a `\uXXXX` escape.
  if (codePoint >= 0xd800 && codePoint <= 0xdfff) return 6
  return codePoint < 0x10000 ? 3 : 4
}

/**
 * Measure one JSON string, quotes included, without building its escaped form.
 * @param text - the candidate string.
 * @param maxBytes - largest size the caller can admit.
 * @returns exact serialized bytes, or undefined as soon as the cap is crossed.
 */
export function jsonStringBytesUpTo(text: string, maxBytes: number): number | undefined {
  let bytes = 2
  if (bytes > maxBytes) return undefined
  for (let index = 0; index < text.length;) {
    const codePoint = text.codePointAt(index) as number
    bytes += codePointBytes(codePoint)
    if (bytes > maxBytes) return undefined
    index += codePoint > 0xffff ? 2 : 1
  }
  return bytes
}

/**
 * Longest code-point-aligned prefix whose JSON string encoding, quotes
 * included, fits the budget.
 * @param text - the candidate string.
 * @param maxBytes - serialized bytes available.
 * @returns the fitting prefix; empty when not even one character fits.
 */
export function truncateJsonStringBytes(text: string, maxBytes: number): string {
  let bytes = 2
  let end = 0
  while (end < text.length) {
    const codePoint = text.codePointAt(end) as number
    const cost = codePointBytes(codePoint)
    if (bytes + cost > maxBytes) break
    bytes += cost
    end += codePoint > 0xffff ? 2 : 1
  }
  return text.slice(0, end)
}

/**
 * Measure one lossless JSON value without serializing it. The traversal is
 * iterative, so nesting depth costs no call stack.
 * @param value - an already validated lossless JSON value.
 * @param maxBytes - largest size the caller can admit.
 * @returns exact serialized bytes, or undefined as soon as the cap is crossed.
 */
export function jsonValueBytesUpTo(value: PtcJsonValue, maxBytes: number): number | undefined {
  let bytes = 0
  const pending: PtcJsonValue[] = [value]
  for (let current = pending.pop(); current !== undefined; current = pending.pop()) {
    if (current === null) bytes += 4
    else if (typeof current === 'boolean') bytes += current ? 4 : 5
    else if (typeof current === 'number') bytes += String(current).length
    else if (typeof current === 'string') {
      const stringBytes = jsonStringBytesUpTo(current, maxBytes - bytes)
      if (stringBytes === undefined) return undefined
      bytes += stringBytes
    } else if (Array.isArray(current)) {
      // Brackets plus one comma between items.
      bytes += 2 + Math.max(0, current.length - 1)
      pending.push(...current)
    } else {
      const keys = Object.keys(current)
      bytes += 2 + Math.max(0, keys.length - 1)
      for (const key of keys) {
        const keyBytes = jsonStringBytesUpTo(key, maxBytes - bytes)
        if (keyBytes === undefined) return undefined
        // The key, its colon, then its value.
        bytes += keyBytes + 1
        pending.push(current[key] as PtcJsonValue)
      }
    }
    if (bytes > maxBytes) return undefined
  }
  return bytes
}

/**
 * One run's combined outer-output budget: the serialized `logs` array plus the
 * completion value or failure diagnostic. Binding traffic never enters it.
 */
export class OutputLedger {
  /** JSON serialization of the empty logs array: `[]`. */
  private bytes = 2
  private entries = 0
  private readonly maxBytes: number

  constructor(maxBytes: number) {
    this.maxBytes = maxBytes
  }

  /**
   * Charge one log entry against the budget.
   * @param text - the captured text.
   * @returns false when the entry would cross the cap; nothing is charged then.
   */
  admit(text: string): boolean {
    const separatorBytes = this.entries > 0 ? 1 : 0
    const stringBytes = jsonStringBytesUpTo(text, this.maxBytes - this.bytes - separatorBytes)
    if (stringBytes === undefined) return false
    this.bytes += stringBytes + separatorBytes
    this.entries += 1
    return true
  }

  /**
   * Longest prefix of an entry that {@link admit} would accept next.
   * @param text - an entry that did not fit.
   * @returns the fitting prefix; empty when not even one character fits.
   */
  fittingPrefix(text: string): string {
    return truncateJsonStringBytes(text, this.maxBytes - this.bytes - (this.entries > 0 ? 1 : 0))
  }

  /** Bytes left for the completion value or failure diagnostic after the admitted entries. */
  get remaining(): number {
    return this.maxBytes - this.bytes
  }

  /**
   * Finish a run that completed, with or without a value.
   * @param logs - the entries this ledger admitted, in order.
   * @param value - the lossless completion value, if any.
   */
  success(logs: string[], value?: PtcJsonValue): PtcRunResult {
    if (value !== undefined && jsonValueBytesUpTo(value, this.maxBytes - this.bytes) === undefined) return this.limit(logs)
    return { logs, ...value === undefined ? {} : { value } }
  }

  /**
   * Finish a failed run; a diagnostic that crosses the cap becomes the output-limit failure.
   * @param logs - the entries this ledger admitted, in order.
   * @param error - the selected failure.
   */
  failure(logs: string[], error: PtcRunFailure): PtcRunResult {
    if (jsonStringBytesUpTo(error.message, this.maxBytes - this.bytes) === undefined) return this.limit(logs)
    return { logs, error }
  }

  /**
   * Build the output-limit failure, keeping the longest log prefix that fits
   * beside its fixed diagnostic.
   * @param logs - every captured entry, including the one that overflowed.
   */
  limit(logs: string[]): PtcRunResult {
    const fullMessage = `outer output exceeded ${this.maxBytes} bytes`
    // The diagnostic is ASCII: one byte per character plus its quotes.
    const logBudget = this.maxBytes - (fullMessage.length + 2)
    const retained: string[] = []
    let retainedBytes = 2
    for (const text of logs) {
      const separatorBytes = retained.length > 0 ? 1 : 0
      const available = logBudget - retainedBytes - separatorBytes
      const stringBytes = jsonStringBytesUpTo(text, available)
      if (stringBytes !== undefined) {
        retained.push(text)
        retainedBytes += stringBytes + separatorBytes
        continue
      }
      const prefix = truncateJsonStringBytes(text, available)
      if (prefix.length > 0) {
        retained.push(prefix)
        retainedBytes += (jsonStringBytesUpTo(prefix, available) as number) + separatorBytes
      }
      break
    }
    const message = truncateJsonStringBytes(fullMessage, this.maxBytes - retainedBytes)
    return { logs: retained, error: { kind: 'output-limit', message } }
  }
}
