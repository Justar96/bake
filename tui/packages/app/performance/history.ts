/** Fixed synthetic histories for measuring built terminal resume without user data. */
import { Session, SessionId, SESSION_FORMAT_VERSION, type SessionHeader } from '@deepseek-ai/dsh-session'
import { createAssistantMessage, createUserMessage, createSystemMessage, createToolResultMessage, ToolCallId, MessageId, type StreamChunk, type ContentBlock } from '@deepseek-ai/dsh-llm'
import { AssistantStreamAccumulator } from '@deepseek-ai/dsh-llm/assistant-stream'

/** Fixed event time for synthetic fixtures. */
const TIME = 1_700_000_000_000

/**
 * Build complete historical turns with reasoning, prose, and periodic tool output.
 * @param turns - count of completed turns.
 * @param cwd - private workspace used by the measured process.
 * @returns the validated Session and dimensions of the input.
 */
export function history(turns: number, cwd: string) {
  const id = SessionId('tui-perf-session')
  const header: SessionHeader = { id, version: SESSION_FORMAT_VERSION, createdAt: TIME, cwd, agentPreset: 'standard', isSeeded: false, delegationDepth: 0 }
  const session = Session.create(id, undefined, header)
  let deltaCount = 0
  let toolCount = 0
  for (let turn = 1; turn <= turns; turn++) {
    session.append('turn/start', { turn })
    session.append('step/start', { turn, step: 1 })
    if (turn === 1) session.append('system/message', { turn, step: 1, message: { ...createSystemMessage('', '@deepseek-ai/dsh-system-prompt'), id: MessageId('perf-system') } }, { surfaceOp: 'append' })
    session.append('user/message', { ...createUserMessage({ content: [{ type: 'text', text: `Synthetic review ${turn}. ` + '检查边界和顺序。 '.repeat(8) }], source: { kind: 'user' } }), id: MessageId(`perf-user-${turn}`) }, { surfaceOp: 'append' })
    const tool = turn % 4 === 0
    const callId = ToolCallId(`perf-call-${turn}`)
    const content: ContentBlock[] = [
      { type: 'reasoning', text: 'Compare the synthetic implementation and its tests. '.repeat(6) },
      { type: 'text', text: 'Validate ordering, cancellation, and resource cleanup. '.repeat(12) + `\nH${String(turn).padStart(5, '0')}_END` },
      ...tool ? [{ type: 'tool-call' as const, id: callId, name: 'synthetic_tool', arguments: '{"path":"src/example.ts"}' }] : [],
    ]
    const stream = new AssistantStreamAccumulator()
    let tick = TIME + turn * 10000
    for (const [index, block] of content.entries()) {
      stream.push({ time: tick++, chunk: { type: 'block-start', index, blockType: block.type } })
      if (block.type === 'text' || block.type === 'reasoning') {
        for (let offset = 0; offset < block.text.length; offset += 24) {
          stream.push({ time: tick++, chunk: { type: block.type === 'text' ? 'text-delta' : 'reasoning-delta', index, text: block.text.slice(offset, offset + 24) } })
          deltaCount++
        }
      }
      stream.push({ time: tick++, chunk: { type: 'block-end', index, block } })
    }
    stream.push({ time: tick, chunk: { type: 'finish', reason: { kind: tool ? 'tool-calls' : 'stop' } } })
    session.append('assistant/message', { turn, step: 1, stream: [...stream.snapshot()], message: { ...createAssistantMessage({ source: { provider: 'perf', model: 'synthetic' }, content }), id: MessageId(`perf-assistant-${turn}`) } }, { surfaceOp: 'append' })
    if (tool) {
      toolCount++
      const call = session.append('tool/call', { turn, step: 1, callId, name: 'synthetic_tool', arguments: '{"path":"src/example.ts"}' })
      session.append('tool/result', { turn, step: 1, message: { ...createToolResultMessage({ callId, isError: false, content: [{ type: 'text', text: 'Synthetic tool output: observed value matches expected value.\n'.repeat(128) }] }), id: MessageId(`perf-tool-${turn}`) } }, { surfaceOp: 'append', sourceEventSeqs: [call.seq] })
    }
    session.append('step/end', { turn, step: 1 })
    session.append('turn/end', { turn, reason: { kind: 'completed' } })
  }
  const events = session.snapshotEvents().map(event => ({ ...event, time: TIME + Number(event.seq) }))
  return { header: session.header, events, dimensions: { turns, events: events.length, deltaCount, toolCount } }
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
