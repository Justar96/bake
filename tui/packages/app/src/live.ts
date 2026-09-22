/**
 * Stream chunks as the rows the live region draws while a turn is still running.
 *
 * Separate from `BlockAssembler.interruptedBlocks()`, which answers a different
 * question: what an interrupted attempt may safely finalize into a durable
 * message. That answer drops every tool call, because interruption precedes
 * dispatch and keeping one would need a fabricated result. Display has no such
 * obligation — nothing here is written anywhere — and dropping the calls is
 * what makes a turn look idle for as long as the model spends calling tools.
 *
 * @module @dsh-tui/app/live
 */

import type { StreamChunk } from '@deepseek-ai/dsh-llm'
import type { Row } from '@dsh-tui/ui'
import { PENDING_ARGUMENTS } from '@dsh-tui/ui/present.ts'

/** One block being assembled, before it is known whether the stream completes. */
interface OpenBlock {
  /** Block kind from `block-start`, or inferred from the first delta. */
  type: string
  /** Text or reasoning content accumulated from deltas. */
  text: string
  /** Tool-call identity, absent until the first tool-call delta carries it. */
  id?: string
  /** Tool name, absent until a tool-call delta carries it. */
  name?: string
}

/** A live row and the stream index of the block it shows. */
export interface KeyedRow {
  readonly key: number
  readonly row: Row
}

/**
 * Live rows for one assistant attempt, rebuilt from the chunks it has streamed.
 *
 * One instance per attempt: it accumulates, and a second attempt's chunks would
 * append to the first attempt's blocks.
 */
export class LiveBlocks {
  private readonly order: number[] = []
  private readonly blocks = new Map<number, OpenBlock>()

  /**
   * Feed one chunk into the display state.
   *
   * Tolerant of delta-only protocols the same way the assembler is: a delta for
   * an index no `block-start` announced opens that block itself, so an adapter
   * that sends no block framing still shows its output.
   *
   * @param chunk - the next raw chunk, in stream order.
   */
  push(chunk: StreamChunk): void {
    switch (chunk.type) {
      case 'block-start':
        this.open(chunk.index, chunk.blockType)
        break
      case 'text-delta':
        this.open(chunk.index, 'text').text += chunk.text
        break
      case 'reasoning-delta':
        this.open(chunk.index, 'reasoning').text += chunk.text
        break
      case 'tool-call-delta': {
        const block = this.open(chunk.index, 'tool-call')
        block.id ??= chunk.id
        if (chunk.name !== undefined) block.name = chunk.name
        break
      }
      case 'block-end': {
        // `block-end` is authoritative: an adapter may correct what its deltas
        // implied, and only here is a tool call's name guaranteed to have
        // arrived. Arguments are still not shown — the committed row carries
        // them, presented, a moment later.
        const block = this.open(chunk.index, chunk.block.type)
        block.type = chunk.block.type
        if (chunk.block.type === 'text' || chunk.block.type === 'reasoning') block.text = chunk.block.text
        if (chunk.block.type === 'tool-call') { block.id = chunk.block.id; block.name = chunk.block.name }
        break
      }
      default:
        // `usage` and `finish` carry no displayable content, and a merge-extended
        // chunk type this build does not know is not guessed at.
        break
    }
  }

  /**
   * The rows to draw for what has streamed so far.
   *
   * Stream order, because that is the order the events happened in: reasoning
   * before the call it led to, and the answer after the call it waited for. A
   * block with nothing to show yet produces no row rather than an empty one,
   * so a block that has only been announced costs no height.
   *
   * @returns live rows in stream order, empty before anything displayable arrives.
   */
  rows(): readonly Row[] {
    return this.keyed().map(entry => entry.row)
  }

  /**
   * The same rows, each with its block's stream index, which stays fixed while
   * a block that had nothing to show yet gains a row ahead of it.
   * @returns keyed live rows in stream order.
   */
  keyed(): readonly KeyedRow[] {
    return this.order.flatMap((index): KeyedRow[] => {
      const block = this.blocks.get(index)!
      if (block.type === 'text') return block.text.trim() === '' ? [] : [{ key: index, row: { kind: 'assistant', text: block.text } }]
      if (block.type === 'reasoning') return block.text.trim() === '' ? [] : [{ key: index, row: { kind: 'reasoning', text: block.text } }]
      // A call is shown from the moment its name is known and not before: the
      // verb column is derived from the name, so a nameless call could only be
      // drawn by guessing at what the agent is doing.
      if (block.type === 'tool-call' && block.name !== undefined) {
        return [{ key: index, row: { kind: 'tool-call', callId: block.id ?? `live-${index}`, tool: block.name, input: PENDING_ARGUMENTS } }]
      }
      return []
    })
  }

  private open(index: number, type: string): OpenBlock {
    const existing = this.blocks.get(index)
    if (existing !== undefined) return existing
    const block: OpenBlock = { type, text: '' }
    this.order.push(index)
    this.blocks.set(index, block)
    return block
  }
}
