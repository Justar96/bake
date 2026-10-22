/** Running calls' live output and finish reach the live rows, and the logged result replaces them. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ToolCallId, type StreamChunk } from 'bake-llm'
import { defineContentToolFixture } from 'bake-tools'
import { transcriptRows } from 'bake-tui-ui'
import { dictionaries } from 'bake-tui-ui/copy.ts'
import type { Row, ToolCallRow } from 'bake-tui-ui/rows.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import { harness, textResponse } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

/** The calls the live region would draw, flattened out of a group. */
const liveCalls = (rows: readonly Row[]): readonly ToolCallRow[] =>
  rows.flatMap(row => row.kind === 'tool-call' ? [row] : row.kind === 'tool-group' ? row.calls : [])

function twoCalls(): StreamChunk[] {
  return [
    ...(['slow', 'fast'] as const).flatMap((id, index): StreamChunk[] => [
      { type: 'block-start', index, blockType: 'tool-call' },
      { type: 'block-end', index, block: { type: 'tool-call', id: ToolCallId(id), name: 'probe', arguments: JSON.stringify({ id }) } },
    ]),
    { type: 'finish', reason: { kind: 'tool-calls' } },
  ]
}

describe('live tool progress', () => {
  it('shows a running call\'s output, marks a fast parallel call finished, and commits only logged rows', async () => {
    const fixture = await harness()
    cleanup.push(fixture.dispose)
    const { ctx, model } = fixture
    const gates = new Map<string, () => void>()
    cleanup.push(async () => { for (const release of gates.values()) release() })
    ctx.tools.register(defineContentToolFixture({
      name: 'probe', description: 'gated probe', parameters: { id: { type: 'string', required: true } },
      isConcurrencySafe: () => true,
      async execute(args, exec) {
        exec.reportProgress({ output: `${args.id} LIVEMARK\n` })
        await new Promise<void>((resolve) => { gates.set(args.id, resolve) })
        return [{ type: 'text', text: `${args.id} finished` }]
      },
    }))
    let replies = 0
    model.response = async function* () {
      replies++
      yield* replies === 1 ? twoCalls() : textResponse('All done')
    }
    let controller!: SessionController
    const handle = await openSession(ctx, {}, new AbortController().signal, agent => {
      controller = new SessionController(ctx, agent, dictionaries.en, { refs: [] }, () => {}, { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { controller.close(); await handle.dispose(); await controller.drain() })
    await controller.replay(new AbortController().signal)

    controller.submit('Probe twice')
    await vi.waitFor(() => expect(gates.size).toBe(2))
    await vi.waitFor(() => expect(liveCalls(controller.view.live).map(call => call.live?.tail)).toEqual([['slow LIVEMARK'], ['fast LIVEMARK']]))
    gates.get('fast')!()
    await vi.waitFor(() => expect(liveCalls(controller.view.live).find(call => call.callId === 'fast')?.live?.finished).toEqual({ ok: true }))
    // Its result waits for the slow call's, which is still running.
    expect(liveCalls(controller.view.live).every(call => call.result === undefined)).toBe(true)
    expect(liveCalls(controller.view.live).find(call => call.callId === 'slow')?.live?.finished).toBeUndefined()

    gates.get('slow')!()
    await handle.agent.whenIdle()
    expect(controller.view.live).toEqual([])
    const committed = transcriptRows(controller.view.committed)
    const calls = liveCalls(committed)
    expect(calls.map(call => [call.callId, call.result?.ok])).toEqual([['slow', true], ['fast', true]])
    expect(calls.some(call => call.live !== undefined)).toBe(false)
    expect(JSON.stringify(handle.agent.session.snapshotEvents())).not.toContain('LIVEMARK')
  })
})
