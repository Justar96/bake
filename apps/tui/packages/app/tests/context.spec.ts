/** Context occupancy follows the meter's public view through growth and compaction. */
import { expect, it } from 'vitest'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import { CompactionId } from '@deepseek-ai/dsh-compaction'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { AgentHandle } from '@deepseek-ai/dsh-agent'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { harness, textResponse } from './harness.ts'

it('reports projected context after output and reduces it immediately after compaction', async () => {
  const fixture = await harness()
  let controller: SessionController | undefined
  let handle: AgentHandle | undefined
  try {
    await fixture.ctx.plugin(TokenMeter)
    handle = await openSession(fixture.ctx, {}, new AbortController().signal, agent => {
      controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    const view = controller!
    await view.replay(new AbortController().signal)
    expect(view.view.context).toBeUndefined()
    fixture.model.response = async function* () {
      yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 100 } }
      yield* textResponse('An answer that grows the conversation. '.repeat(20))
    }
    view.submit('Explain the changes')
    await handle.agent.whenIdle()
    const session = handle.agent.session
    const pressure = () => fixture.ctx.sessionProjections.snapshot(session, ['contextPressure']).values.contextPressure!
    const before = pressure().projectedTokens!
    expect(before).toBeGreaterThan(900)
    expect(view.view.context).toEqual({ used: before, window: 8192 })

    // Record the meter-owned shadow price and replacement used by compaction-basic.
    // The first surface node is the system prompt; compaction preserves it.
    const shadowed = fixture.ctx.tokenMeter.measure(session).nodes.slice(1)
    const start = shadowed[0]!.seq
    const end = shadowed.at(-1)!.seq
    session.append('compaction/summary', {
      compactionId: CompactionId('tui-context-test'), summary: [{ type: 'text', text: 'summary' }],
      shadowedRange: { start, end }, shadowedSeqs: shadowed.map(node => node.seq),
      shadowedTokenCount: shadowed.reduce((total, node) => total + node.heuristicTokens, 0),
      provider: 'mock', model: 'model',
    })
    session.append('user/message', createUserMessage({
      content: [{ type: 'text', text: 'summary' }], source: { kind: 'plugin', plugin: 'test' },
    }), { surfaceOp: { op: 'replace', startSeq: start, endSeq: end }, sourceEventSeqs: shadowed.map(node => node.seq) })
    expect(pressure().pressureTokens).toBe(900)
    expect(pressure().projectedTokens).toBeLessThan(before)
    expect(view.view.context?.used).toBe(pressure().projectedTokens)
  } finally {
    controller?.close()
    await handle?.dispose()
    await fixture.dispose()
  }
})
