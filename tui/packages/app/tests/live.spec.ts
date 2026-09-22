/** What the live region shows while an attempt is still streaming. */
import { describe, expect, it } from 'vitest'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import type { StreamChunk } from '@deepseek-ai/dsh-llm'
import { present, type ResultBound } from '@dsh-tui/ui/present.ts'
import { VERB } from '@dsh-tui/ui/layout.ts'
import { LiveBlocks } from '../src/live.ts'

/** Feed chunks in stream order and read back what the region would draw. */
function streamed(...chunks: readonly StreamChunk[]) {
  const blocks = new LiveBlocks()
  for (const chunk of chunks) blocks.push(chunk)
  return blocks.rows()
}

/** The live region's own bound; these rows carry no result, so it only has to exist. */
const live: ResultBound = { lines: 8, unit: 'lines', more: 'more lines' }

/** The verbs of every line the rows present, so a section is named by what it says. */
const verbs = (rows: readonly Parameters<typeof present>[0][]): string[] =>
  rows.flatMap(row => present(row, live)).map(line => line.verb).filter(verb => verb !== '')

describe('LiveBlocks', () => {
  it('shows the call the agent is making, not only what it said', () => {
    // The regression this guards: a turn that reasons, then calls a tool, used
    // to go blank for the whole call — the surface looked idle while the agent
    // was at its busiest.
    const rows = streamed(
      { type: 'block-start', index: 0, blockType: 'reasoning' },
      { type: 'reasoning-delta', index: 0, text: 'need the file' },
      { type: 'block-start', index: 1, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 1, id: ToolCallId('c1'), name: 'read_file', argumentsDelta: '{"pa' },
    )
    expect(rows.map(row => row.kind)).toEqual(['reasoning', 'tool-call'])
    expect(verbs(rows)).toEqual([VERB.think, VERB.read])
  })

  it('keeps blocks in stream order, so reasoning stays above the call it led to', () => {
    const rows = streamed(
      { type: 'block-start', index: 0, blockType: 'reasoning' },
      { type: 'reasoning-delta', index: 0, text: 'first' },
      { type: 'block-start', index: 1, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 1, id: ToolCallId('c1'), name: 'bash', argumentsDelta: '{' },
      { type: 'block-start', index: 2, blockType: 'text' },
      { type: 'text-delta', index: 2, text: 'here it is' },
    )
    expect(rows.map(row => row.kind)).toEqual(['reasoning', 'tool-call', 'assistant'])
  })

  it('never shows the arguments, whether they are half sent or complete', () => {
    // Every prefix of a streamed JSON argument string is invalid JSON, and most
    // end mid-token. The complete arguments are no better here: the committed
    // row presents them through the tool's own presenter a moment later.
    const half = streamed(
      { type: 'block-start', index: 0, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 0, id: ToolCallId('c1'), name: 'bash', argumentsDelta: '{"command": "rm -' },
    )
    const whole = streamed(
      { type: 'block-start', index: 0, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 0, id: ToolCallId('c1'), name: 'bash', argumentsDelta: '{"command": "ls"}' },
      { type: 'block-end', index: 0, block: { type: 'tool-call', id: ToolCallId('c1'), name: 'bash', arguments: '{"command": "ls"}' } },
    )
    for (const rows of [half, whole]) {
      expect(rows).toHaveLength(1)
      const text = present(rows[0]!, live).map(line => line.text).join('\n')
      expect(text).not.toContain('command')
      expect(text).toContain('bash')
    }
  })

  it('waits for the name before drawing a call, rather than guessing the verb', () => {
    // The verb column is derived from the tool's name; announced but unnamed,
    // the only row that could be drawn is a guess at what the agent is doing.
    expect(streamed({ type: 'block-start', index: 0, blockType: 'tool-call' })).toEqual([])
  })

  it('takes block-end as authoritative over what the deltas implied', () => {
    const rows = streamed(
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: 'draf' },
      { type: 'block-end', index: 0, block: { type: 'text', text: 'drafted' } },
    )
    expect(rows).toEqual([{ kind: 'assistant', text: 'drafted' }])
  })

  it('assembles a delta-only stream that sends no block framing', () => {
    const rows = streamed(
      { type: 'reasoning-delta', index: 0, text: 'a' },
      { type: 'reasoning-delta', index: 0, text: 'b' },
      { type: 'text-delta', index: 1, text: 'answer' },
    )
    expect(rows).toEqual([{ kind: 'reasoning', text: 'ab' }, { kind: 'assistant', text: 'answer' }])
  })

  it('draws no row for a block with nothing in it yet', () => {
    // An announced block, and whitespace before the first real token, would
    // each otherwise take a row of a region whose whole point is a fixed height.
    expect(streamed(
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: '  \n' },
      { type: 'block-start', index: 1, blockType: 'reasoning' },
      { type: 'usage', usage: { inputTokens: 1, outputTokens: 1 } },
    )).toEqual([])
  })

  it('ignores a block kind this build does not know', () => {
    expect(streamed(
      { type: 'block-start', index: 0, blockType: 'mystery' } as unknown as StreamChunk,
      { type: 'text-delta', index: 1, text: 'known' },
    )).toEqual([{ kind: 'assistant', text: 'known' }])
  })
})
