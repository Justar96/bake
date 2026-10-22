/**
 * A workload as Bake 0.3's Session log: format v3, raw JSONL, one event per
 * row, in the layout `bake-session-persistence-jsonl` reads with
 * `compression: 'none'`.
 *
 * The bytes equal what the 0.3 fixture writer
 * (`apps/tui/packages/app/performance/seed.ts` at v0.3.8) persisted through
 * the Harness Session and JSONL backend, including each answer's compact
 * stream records (`AssistantStreamAccumulator`, `bake-llm/assistant-stream`):
 * a block's 24-character deltas, one millisecond apart, compact into one
 * `text-chunks` or `reasoning-chunks` record. No 0.3 package is loaded.
 */
import { join } from 'node:path'
import { history, TIME, TOOL_ARGUMENTS, TOOL_NAME, type HistoryDimensions, type WorkloadSpec } from './workloads.ts'

/** The id of every seeded 0.3 session, which the `tui-runner` resumes. */
export const SESSION03_ID = 'tui-perf-session'
const DELTA_CHARS = 24

/** 0.3-specific sizes beside the logical dimensions. */
export interface Session03Dimensions extends HistoryDimensions {
  events: number
  deltaCount: number
  compactRecordCount: number
  fileBytes: number
}

/** A seeded log: its bytes, where they belong under the sessions root, and their sizes. */
export interface Session03Seed {
  /** Path of the log relative to the persistence root. */
  readonly relativePath: string
  readonly bytes: Uint8Array
  readonly dimensions: Session03Dimensions
}

/**
 * The persistence backend's project directory name for `cwd`
 * (`projectKey` in `bake-session-persistence-jsonl/src/format.ts`).
 * @param cwd - the session's absolute working directory.
 * @returns `--<slug>--`.
 */
export function projectKey03(cwd: string): string {
  if (cwd.length === 0) throw new Error('cannot encode an empty project path')
  let readable = ''
  let separatorRun = false
  for (let i = 0; i < cwd.length; i++) {
    const code = cwd.charCodeAt(i)
    const ch = String.fromCharCode(code)
    if (ch === '/' || ch === '\\' || ch === ':') {
      if (!separatorRun) readable += '-'
      separatorRun = true
    } else if (ch !== '~' && /^[A-Za-z0-9._-]$/.test(ch)) {
      readable += ch
      separatorRun = false
    } else {
      readable += '~' + code.toString(16).toUpperCase().padStart(4, '0')
      separatorRun = false
    }
  }
  const slug = readable.replace(/^-+/, '') || 'root'
  return `--${slug.slice(0, 251)}--`
}

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

/** One delta block's compact records, as the accumulator snapshots them. */
function blockRecords(index: number, type: 'reasoning' | 'text', text: string, tick: number): { records: Json[]; tick: number; deltas: number } {
  const records: Json[] = [{ type: 'chunk', time: tick++, chunk: { type: 'block-start', index, blockType: type } }]
  const texts: string[] = []
  const time0 = tick
  for (let offset = 0; offset < text.length; offset += DELTA_CHARS) {
    texts.push(text.slice(offset, offset + DELTA_CHARS))
    tick++
  }
  if (texts.length > 0) records.push({ type: `${type}-chunks`, time0, index, dt: texts.slice(1).map(() => 1), texts })
  records.push({ type: 'chunk', time: tick++, chunk: { type: 'block-end', index, block: { type, text } } })
  return { records, tick, deltas: texts.length }
}

/**
 * Encode a workload as a 0.3 Session log.
 * @param spec - the workload.
 * @param cwd - the session header's working directory, where the measured process runs.
 * @returns the log's bytes and location under the sessions root; empty for a fresh workload.
 */
export function seedSession03(spec: WorkloadSpec, cwd: string): Session03Seed {
  const { turns, dimensions } = history(spec)
  const lines: string[] = [JSON.stringify({ type: 'session', version: 3, id: SESSION03_ID, createdAt: TIME, cwd, isSeeded: false, delegationDepth: 0, agentPreset: 'standard' })]
  let seq = 0
  let deltaCount = 0
  let compactRecordCount = 0
  const append = (type: string, data: Json, extra: { sourceEventSeqs?: number[]; surfaceOp?: 'append' } = {}): number => {
    const event: Record<string, Json> = { type, seq, time: TIME + seq, data }
    if (extra.sourceEventSeqs !== undefined) event.sourceEventSeqs = extra.sourceEventSeqs
    if (extra.surfaceOp !== undefined) event.surfaceOp = extra.surfaceOp
    lines.push(JSON.stringify(event))
    return seq++
  }
  for (const { turn, userText, reasoning, answer, calls } of turns) {
    append('turn/start', { turn })
    append('step/start', { turn, step: 1 })
    if (turn === 1) {
      append('system/message', { turn, step: 1, message: { role: 'system', content: [], source: { kind: 'plugin', plugin: '@deepseek-ai/dsh-system-prompt' }, id: 'perf-system' } }, { surfaceOp: 'append' })
    }
    append('user/message', { content: [{ type: 'text', text: userText }], source: { kind: 'user' }, role: 'user', id: `perf-user-${turn}` }, { surfaceOp: 'append' })
    const toolBlocks = calls.map(call => ({ type: 'tool-call', id: call.id, name: TOOL_NAME, arguments: TOOL_ARGUMENTS }))
    const stream: Json[] = []
    let tick = TIME + turn * 10000
    for (const [index, [type, text]] of ([['reasoning', reasoning], ['text', answer]] as const).entries()) {
      const block = blockRecords(index, type, text, tick)
      stream.push(...block.records)
      tick = block.tick
      deltaCount += block.deltas
    }
    for (const [offset, block] of toolBlocks.entries()) {
      const index = offset + 2
      stream.push({ type: 'chunk', time: tick++, chunk: { type: 'block-start', index, blockType: 'tool-call' } })
      stream.push({ type: 'chunk', time: tick++, chunk: { type: 'block-end', index, block } })
    }
    stream.push({ type: 'chunk', time: tick, chunk: { type: 'finish', reason: { kind: calls.length > 0 ? 'tool-calls' : 'stop' } } })
    compactRecordCount += stream.length
    const content: Json[] = [{ type: 'reasoning', text: reasoning }, { type: 'text', text: answer }, ...toolBlocks]
    append('assistant/message', { turn, step: 1, stream, message: { role: 'assistant', content, source: { kind: 'model', provider: 'perf', model: 'synthetic' }, id: `perf-assistant-${turn}` } }, { surfaceOp: 'append' })
    for (const call of calls) {
      const callSeq = append('tool/call', { turn, step: 1, callId: call.id, name: TOOL_NAME, arguments: TOOL_ARGUMENTS })
      append('tool/result', { turn, step: 1, message: { source: { kind: 'tool', callId: call.id }, content: [{ type: 'tool-result', toolCallId: call.id, content: [{ type: 'text', text: call.output }], isError: false }], role: 'user', id: call.messageId } },
        { sourceEventSeqs: [callSeq], surfaceOp: 'append' })
    }
    append('step/end', { turn, step: 1 })
    append('turn/end', { turn, reason: { kind: 'completed' } })
  }
  const bytes = turns.length === 0 ? new Uint8Array() : new TextEncoder().encode(lines.join('\n') + '\n')
  return {
    relativePath: join(projectKey03(cwd), SESSION03_ID, 'session.v3.jsonl'),
    bytes,
    dimensions: { ...dimensions, events: seq, deltaCount, compactRecordCount, fileBytes: bytes.length },
  }
}
