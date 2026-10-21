/**
 * A workload as a Pi session file (format version 3), which the native
 * `bake` opens with `--session <path>`.
 *
 * Entries follow Pi v1.3.0's `SessionManager` layout
 * (`packages/coding-agent/src/core/session-manager.ts`): a `session` header,
 * then one `message` entry per user, assistant, and tool-result message, each
 * the child of the one before. Pi stores finished messages only, so the
 * 0.3 log's stream records have no counterpart here. The logical content is
 * the same as {@link ./seed-session03.ts}: the same text, tool calls, and
 * markers, from one generator.
 */
import { history, TIME, TOOL_NAME, type HistoryDimensions, type WorkloadSpec } from './workloads.ts'

/** The id of every seeded Pi session. */
export const PI_SESSION_ID = '01890a5d-ac96-774b-bcce-b302099a8057'
/** The provider and model of every seeded answer; the faux route has the same names. */
export const PERF_PROVIDER = 'perf'
export const PERF_MODEL = 'synthetic'

/** Pi-specific sizes beside the logical dimensions. */
export interface PiDimensions extends HistoryDimensions {
  entries: number
  fileBytes: number
}

/** A seeded Pi session file: its bytes and sizes. */
export interface PiSeed {
  readonly bytes: Uint8Array
  readonly dimensions: PiDimensions
}

const COST = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 }
/** Small usage, so resume estimates no context pressure from history. */
const USAGE = { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: COST }

/**
 * Encode a workload as a Pi session file.
 * @param spec - the workload.
 * @param cwd - the header's working directory, where the measured process runs.
 * @returns the file's bytes; empty for a fresh workload, which starts a new session instead.
 */
export function seedPi(spec: WorkloadSpec, cwd: string): PiSeed {
  const { turns, dimensions } = history(spec)
  const lines: string[] = [JSON.stringify({ type: 'session', version: 3, id: PI_SESSION_ID, timestamp: new Date(TIME).toISOString(), cwd })]
  let entries = 0
  let parentId: string | null = null
  const append = (message: Record<string, unknown>): void => {
    const id = (++entries).toString(16).padStart(8, '0')
    const time = TIME + entries
    lines.push(JSON.stringify({ type: 'message', id, parentId, timestamp: new Date(time).toISOString(), message: { ...message, timestamp: time } }))
    parentId = id
  }
  for (const { userText, reasoning, answer, calls } of turns) {
    append({ role: 'user', content: [{ type: 'text', text: userText }] })
    append({
      role: 'assistant',
      content: [
        { type: 'thinking', thinking: reasoning },
        { type: 'text', text: answer },
        ...calls.map(call => ({ type: 'toolCall', id: call.id, name: TOOL_NAME, arguments: { path: 'src/example.ts' } })),
      ],
      api: 'openai-responses', provider: PERF_PROVIDER, model: PERF_MODEL, usage: USAGE,
      stopReason: calls.length > 0 ? 'toolUse' : 'stop',
    })
    for (const call of calls) {
      append({ role: 'toolResult', toolCallId: call.id, toolName: TOOL_NAME, content: [{ type: 'text', text: call.output }], isError: false })
    }
  }
  const bytes = turns.length === 0 ? new Uint8Array() : new TextEncoder().encode(lines.join('\n') + '\n')
  return { bytes, dimensions: { ...dimensions, entries, fileBytes: bytes.length } }
}
