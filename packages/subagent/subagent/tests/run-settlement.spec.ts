import { describe, expect, it } from 'vitest'
import type { ContentBlock } from 'bake-llm'
import { SessionId } from 'bake-session'
import { settleRun } from '../src/index.ts'
import type { SubagentResult } from '../src/index.ts'

const MAX_SUBAGENT_DIAGNOSTIC_BYTES = 4_096
const DIAGNOSTIC_TRUNCATION_SUFFIX = '\n[diagnostic truncated]'

/**
 * Spec-local fixture settlement, inlined from the removed out-of-process
 * provider vocabulary: shape an attempt into the never-rejecting result the
 * background-Task path consumes. Bounds provider diagnostics to
 * {@link MAX_SUBAGENT_DIAGNOSTIC_BYTES} without splitting a UTF-8 sequence and
 * maps a cancellation-adjacent outcome to `aborted`; the abort listener is
 * removed on every path.
 */
const utf8Encoder = new TextEncoder()
const utf8Decoder = new TextDecoder()

function limitDiagnostic(diagnostic: string): string {
  const bytes = utf8Encoder.encode(diagnostic)
  if (bytes.byteLength <= MAX_SUBAGENT_DIAGNOSTIC_BYTES) return diagnostic
  const suffixBytes = utf8Encoder.encode(DIAGNOSTIC_TRUNCATION_SUFFIX).byteLength
  let prefixBytes = MAX_SUBAGENT_DIAGNOSTIC_BYTES - suffixBytes
  while (((bytes[prefixBytes] as number) & 0b1100_0000) === 0b1000_0000) {
    prefixBytes -= 1
  }
  return utf8Decoder.decode(bytes.subarray(0, prefixBytes)) + DIAGNOSTIC_TRUNCATION_SUFFIX
}

async function settleRunResult(parts: {
  attempt: () => Promise<SubagentResult>
  collectOutput: () => ContentBlock[]
  collectDiagnostic?: (() => string | undefined) | undefined
  cancelled: () => boolean
  signal: AbortSignal
  onAbort: () => void
}): Promise<SubagentResult> {
  try {
    const result = await parts.attempt()
    if (parts.cancelled()) return { output: parts.collectOutput(), stopReason: 'aborted' }
    return result.diagnostic === undefined
      ? result
      : { ...result, diagnostic: limitDiagnostic(result.diagnostic) }
  } catch {
    // A rejection already queued when cancellation arrives still aborts.
    if (parts.cancelled()) return { output: parts.collectOutput(), stopReason: 'aborted' }
    const collected = parts.collectDiagnostic?.()
    const diagnostic = collected === undefined ? undefined : limitDiagnostic(collected)
    return {
      output: parts.collectOutput(),
      ...(diagnostic === undefined ? {} : { diagnostic }),
      stopReason: 'error',
    }
  } finally {
    parts.signal.removeEventListener('abort', parts.onAbort)
  }
}

describe('outcome mapping helpers', () => {
  it.each([
    ['completed', { status: 'completed', output: 'partial' }],
    ['aborted', { status: 'killed' }],
    ['error', { status: 'failed', detail: 'error' }],
    ['max-tokens', { status: 'failed', detail: 'max-tokens' }],
    ['refusal', { status: 'failed', detail: 'refusal' }],
    ['paused', { status: 'failed', detail: 'paused' }],
  ] as const)('settleRun maps the %s stop reason onto its Task outcome', async (stopReason, expected) => {
    const output = [{ type: 'text' as const, text: 'partial' }]
    await expect(settleRun({
      id: SessionId('child'),
      localAgent: undefined,
      result: Promise.resolve({ output, stopReason: stopReason as never }),
      dispose: () => Promise.resolve(),
    })).resolves.toEqual(expected)
  })

  it('settleRun disposes the run before reporting, on both result paths', async () => {
    const order: string[] = []
    const completed = await settleRun({
      id: SessionId('child-1'),
      localAgent: undefined,
      result: Promise.resolve({ output: [{ type: 'text' as const, text: 'ok' }], stopReason: 'completed' as const }),
      dispose() { order.push('dispose'); return Promise.resolve() },
    })
    order.push('reported')
    expect(completed).toEqual({ status: 'completed', output: 'ok' })
    expect(order).toEqual(['dispose', 'reported'])

    // An infrastructure rejection still disposes and reports failed.
    let disposed = false
    const failed = await settleRun({
      id: SessionId('child-2'),
      localAgent: undefined,
      result: Promise.reject(new Error('transport gone')),
      dispose() { disposed = true; return Promise.resolve() },
    })
    expect(failed).toEqual({ status: 'failed', detail: 'Error: transport gone' })
    expect(disposed).toBe(true)

    const disposeFailed = await settleRun({
      id: SessionId('child-4'),
      localAgent: undefined,
      result: Promise.resolve({ output: [], stopReason: 'completed' }),
      dispose: () => Promise.reject(new Error('reap failed')),
    })
    expect(disposeFailed).toEqual({ status: 'failed', detail: 'dispose failed: Error: reap failed' })

    const bothFailed = await settleRun({
      id: SessionId('child-5'),
      localAgent: undefined,
      result: Promise.reject(new Error('result failed')),
      dispose: () => Promise.reject(new Error('reap failed')),
    })
    expect(bothFailed).toEqual({
      status: 'failed',
      detail: 'Error: result failed; dispose failed: Error: reap failed',
    })
  })

  it('keeps provider diagnostics separate in failed background outcomes', async () => {
    await expect(settleRun({
      id: SessionId('child-diagnostic'),
      localAgent: undefined,
      result: Promise.resolve({
        output: [{ type: 'text', text: 'partial assistant text' }],
        diagnostic: 'Claude Code denied a tool request',
        stopReason: 'error',
      }),
      dispose: () => Promise.resolve(),
    })).resolves.toEqual({
      status: 'failed',
      detail: 'error; diagnostic: Claude Code denied a tool request',
    })
  })

  it('treats a diagnostic-bearing remote abort as failed without changing local cancellation', async () => {
    await expect(settleRun({
      id: SessionId('child-remote-abort'),
      localAgent: undefined,
      result: Promise.resolve({
        output: [],
        diagnostic: 'ACP permission was denied',
        stopReason: 'aborted',
      }),
      dispose: () => Promise.resolve(),
    })).resolves.toEqual({
      status: 'failed',
      detail: 'aborted; diagnostic: ACP permission was denied',
    })
  })

  it('bounds multibyte diagnostics and marks truncation', async () => {
    const exact = 'x'.repeat(MAX_SUBAGENT_DIAGNOSTIC_BYTES)
    const oversized = '权限'.repeat(MAX_SUBAGENT_DIAGNOSTIC_BYTES)
    const controller = new AbortController()
    const exactResult = await settleRunResult({
      attempt: async () => { throw new Error('provider failed') },
      collectOutput: () => [],
      collectDiagnostic: () => exact,
      cancelled: () => false,
      signal: controller.signal,
      onAbort: () => {},
    })
    expect(exactResult.diagnostic).toBe(exact)

    const result = await settleRunResult({
      attempt: async () => { throw new Error('provider failed') },
      collectOutput: () => [],
      collectDiagnostic: () => oversized,
      cancelled: () => false,
      signal: controller.signal,
      onAbort: () => {},
    })
    const limited = result.diagnostic ?? ''
    expect(Buffer.byteLength(limited, 'utf8'))
      .toBeLessThanOrEqual(MAX_SUBAGENT_DIAGNOSTIC_BYTES)
    expect(limited.endsWith('[diagnostic truncated]')).toBe(true)
    expect(limited).not.toContain('\uFFFD')
    expect(result.stopReason).toBe('error')
    expect(result.diagnostic).toBe(limited)
  })

  it('applies the same diagnostic rules to provider-returned results', async () => {
    const controller = new AbortController()
    const oversized = '权限'.repeat(MAX_SUBAGENT_DIAGNOSTIC_BYTES)
    const failed = await settleRunResult({
      attempt: () => Promise.resolve({
        output: [],
        diagnostic: oversized,
        stopReason: 'error',
      }),
      collectOutput: () => [],
      cancelled: () => false,
      signal: controller.signal,
      onAbort: () => {},
    })
    expect(Buffer.byteLength(failed.diagnostic ?? '', 'utf8'))
      .toBeLessThanOrEqual(MAX_SUBAGENT_DIAGNOSTIC_BYTES)
    expect(failed.diagnostic).toMatch(/\[diagnostic truncated\]$/)

    const plainFailure = await settleRunResult({
      attempt: () => Promise.resolve({ output: [], stopReason: 'error' }),
      collectOutput: () => [],
      cancelled: () => false,
      signal: controller.signal,
      onAbort: () => {},
    })
    expect(plainFailure).toEqual({ output: [], stopReason: 'error' })

    const cancelledAfterAttempt = await settleRunResult({
      attempt: () => Promise.resolve({ output: [], stopReason: 'completed' }),
      collectOutput: () => [{ type: 'text', text: 'partial' }],
      cancelled: () => true,
      signal: controller.signal,
      onAbort: () => {},
    })
    expect(cancelledAfterAttempt).toEqual({
      output: [{ type: 'text', text: 'partial' }],
      stopReason: 'aborted',
    })
  })
})
