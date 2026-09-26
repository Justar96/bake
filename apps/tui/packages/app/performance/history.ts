/** Fixed synthetic histories for measuring built terminal resume without user data. */
import { Session, SessionId, SESSION_FORMAT_VERSION, type SessionHeader } from '@deepseek-ai/dsh-session'
import { createAssistantMessage, createUserMessage, createSystemMessage, createToolResultMessage, ToolCallId, MessageId, type StreamChunk, type ContentBlock } from '@deepseek-ai/dsh-llm'
import { AssistantStreamAccumulator } from '@deepseek-ai/dsh-llm/assistant-stream'

/** Fixed event time for synthetic fixtures. */
const TIME = 1_700_000_000_000
const ANSWER_SECTION_BYTES = 4096
const ANSWER_TEXT = 'Validate ordering, cancellation, and resource cleanup. '
const TOOL_TEXT = 'Synthetic tool output: observed value matches expected value.\n'.repeat(128)

/** Independent historical tool density and assistant message size. */
export interface HistoryOptions {
  /** One tool-bearing turn every this many turns; defaults to four. */
  toolEvery?: number
  /** Calls and results in each tool-bearing turn; defaults to one. */
  toolsPerTurn?: number
  /** ASCII bytes per assistant answer, in 4096-byte sections; omitted preserves ordinary prose. */
  assistantTextBytes?: number
}

/** Exact dimensions of the generated logical history, before persistence encoding. */
export interface HistoryDimensions {
  turns: number
  events: number
  deltaCount: number
  compactRecordCount: number
  toolCount: number
  assistantMarkerCount: number
  historyMarkerCount: number
  userTextBytes: number
  assistantTextBytes: number
  reasoningBytes: number
  toolOutputBytes: number
  maxAssistantTextBytes: number
}

/**
 * Build complete historical turns with reasoning, prose, and periodic tool output.
 * @param turns - count of completed turns.
 * @param cwd - private workspace used by the measured process.
 * @param options - independent tool density or large-answer dimensions.
 * @returns the validated Session and dimensions of the input.
 */
export function history(turns: number, cwd: string, options: HistoryOptions = {}) {
  const sections = options.assistantTextBytes === undefined ? 1 : options.assistantTextBytes / ANSWER_SECTION_BYTES
  if (!Number.isSafeInteger(sections) || sections < 1) throw new Error('Assistant text size must be a positive multiple of 4096 bytes')
  const id = SessionId('tui-perf-session')
  const header: SessionHeader = { id, version: SESSION_FORMAT_VERSION, createdAt: TIME, cwd, agentPreset: 'standard', isSeeded: false, delegationDepth: 0 }
  const session = Session.create(id, undefined, header)
  const dimensions: HistoryDimensions = { turns, events: 0, deltaCount: 0, compactRecordCount: 0, toolCount: 0,
    assistantMarkerCount: turns * sections, historyMarkerCount: turns * sections,
    userTextBytes: 0, assistantTextBytes: 0, reasoningBytes: 0, toolOutputBytes: 0, maxAssistantTextBytes: 0 }
  for (let turn = 1; turn <= turns; turn++) {
    session.append('turn/start', { turn })
    session.append('step/start', { turn, step: 1 })
    if (turn === 1) session.append('system/message', { turn, step: 1, message: { ...createSystemMessage('', '@deepseek-ai/dsh-system-prompt'), id: MessageId('perf-system') } }, { surfaceOp: 'append' })
    const userText = `Synthetic review ${turn}. ` + '检查边界和顺序。 '.repeat(8)
    dimensions.userTextBytes += Buffer.byteLength(userText)
    session.append('user/message', { ...createUserMessage({ content: [{ type: 'text', text: userText }], source: { kind: 'user' } }), id: MessageId(`perf-user-${turn}`) }, { surfaceOp: 'append' })
    const calls = turn % (options.toolEvery ?? 4) === 0 ? Array.from({ length: options.toolsPerTurn ?? 1 }, (_, index) => ({
      id: ToolCallId(`perf-call-${turn}${(options.toolsPerTurn ?? 1) === 1 ? '' : `-${index + 1}`}`),
      messageId: MessageId(`perf-tool-${turn}${(options.toolsPerTurn ?? 1) === 1 ? '' : `-${index + 1}`}`),
    })) : []
    const answer = Array.from({ length: sections }, (_, index) => {
      const marker = `\nH${String((turn - 1) * sections + index + 1).padStart(5, '0')}_END`
      return options.assistantTextBytes === undefined ? ANSWER_TEXT.repeat(12) + marker
        : ANSWER_TEXT.repeat(Math.ceil(ANSWER_SECTION_BYTES / ANSWER_TEXT.length)).slice(0, ANSWER_SECTION_BYTES - marker.length - 1) + marker + '\n'
    }).join('')
    const reasoning = 'Compare the synthetic implementation and its tests. '.repeat(6)
    const answerBytes = Buffer.byteLength(answer)
    dimensions.assistantTextBytes += answerBytes
    dimensions.maxAssistantTextBytes = Math.max(dimensions.maxAssistantTextBytes, answerBytes)
    dimensions.reasoningBytes += Buffer.byteLength(reasoning)
    const content: ContentBlock[] = [
      { type: 'reasoning', text: reasoning },
      { type: 'text', text: answer },
      ...calls.map(call => ({ type: 'tool-call' as const, id: call.id, name: 'synthetic_tool', arguments: '{"path":"src/example.ts"}' })),
    ]
    const stream = new AssistantStreamAccumulator()
    let tick = TIME + turn * 10000
    for (const [index, block] of content.entries()) {
      stream.push({ time: tick++, chunk: { type: 'block-start', index, blockType: block.type } })
      if (block.type === 'text' || block.type === 'reasoning') {
        for (let offset = 0; offset < block.text.length; offset += 24) {
          stream.push({ time: tick++, chunk: { type: block.type === 'text' ? 'text-delta' : 'reasoning-delta', index, text: block.text.slice(offset, offset + 24) } })
          dimensions.deltaCount++
        }
      }
      stream.push({ time: tick++, chunk: { type: 'block-end', index, block } })
    }
    stream.push({ time: tick, chunk: { type: 'finish', reason: { kind: calls.length > 0 ? 'tool-calls' : 'stop' } } })
    const records = [...stream.snapshot()]
    dimensions.compactRecordCount += records.length
    session.append('assistant/message', { turn, step: 1, stream: records, message: { ...createAssistantMessage({ source: { provider: 'perf', model: 'synthetic' }, content }), id: MessageId(`perf-assistant-${turn}`) } }, { surfaceOp: 'append' })
    for (const { id: callId, messageId } of calls) {
      // The final tool body follows the last answer and survives the result preview's tail.
      const last = turn === turns && callId === calls.at(-1)?.id
      const toolText = last ? TOOL_TEXT + `H${String(++dimensions.historyMarkerCount).padStart(5, '0')}_END\n` : TOOL_TEXT
      dimensions.toolCount++
      dimensions.toolOutputBytes += Buffer.byteLength(toolText)
      const call = session.append('tool/call', { turn, step: 1, callId, name: 'synthetic_tool', arguments: '{"path":"src/example.ts"}' })
      session.append('tool/result', { turn, step: 1, message: { ...createToolResultMessage({ callId, isError: false, content: [{ type: 'text', text: toolText }] }), id: messageId } }, { surfaceOp: 'append', sourceEventSeqs: [call.seq] })
    }
    session.append('step/end', { turn, step: 1 })
    session.append('turn/end', { turn, reason: { kind: 'completed' } })
  }
  const events = session.snapshotEvents().map(event => ({ ...event, time: TIME + Number(event.seq) }))
  dimensions.events = events.length
  return { header: session.header, events, dimensions }
}

/**
 * Fixed paced response for input measurements while the model is streaming.
 * @returns chunks consumed by the Harness replay adapter; no network is contacted.
 */
export function reply(): StreamChunk[] {
  const parts = Array.from({ length: 100 }, (_, index) => index === 0 ? 'PERF_STREAM_START\n' : index === 99 ? '\nPERF_STREAM_DONE' : `Synthetic delta ${index}. `)
  return [{ type: 'block-start', index: 0, blockType: 'text' },
    ...parts.map(text => ({ type: 'text-delta' as const, index: 0, text })),
    { type: 'block-end', index: 0, block: { type: 'text', text: parts.join('') } },
    { type: 'finish', reason: { kind: 'stop' } }]
}
