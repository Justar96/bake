/**
 * Bounded summarizer input for a span whose replayed form overflowed the
 * summarizing model's context window: the span serialized as one plain-text
 * transcript with long tool output cut, sent without the conversation's
 * system prompt or tool schemas.
 *
 * @module bake-compaction-basic/bounded-input
 */

import { createUserMessage } from 'bake-llm'
import type { ContentBlock, Message } from 'bake-llm'
import type { SummarizationInput } from './summarizer.ts'

/** Most characters kept from one tool result, tool-call argument list, or reasoning block. */
export const TRANSCRIPT_TOOL_MAX_CHARS = 2_000

/** Lead-in of the transcript message; the compaction instruction follows it. */
const TRANSCRIPT_PREAMBLE =
  'The conversation to condense is serialized below as a transcript. Tool results, tool-call arguments, and reasoning longer than 2,000 characters are cut, with a marker giving the number of characters removed.'

/**
 * Cut text to a character budget, keeping its beginning and stating what was cut.
 * @param text - complete text.
 * @param maxChars - characters kept.
 * @returns the text, or its head followed by a truncation marker.
 */
export function truncateForTranscript(text: string, maxChars: number = TRANSCRIPT_TOOL_MAX_CHARS): string {
  if (text.length <= maxChars) return text
  return `${text.slice(0, maxChars)}\n[... ${text.length - maxChars} more characters truncated]`
}

/**
 * Serialize a span's messages as a role-labelled transcript. User and
 * assistant text stay whole; tool results, tool-call arguments, and reasoning
 * are cut to {@link TRANSCRIPT_TOOL_MAX_CHARS}; images and files become placeholders.
 * @param messages - the span's derived messages in surface order, without the system head.
 * @returns the transcript text.
 */
export function serializeTranscript(messages: readonly Message[]): string {
  const parts: string[] = []
  for (const message of messages) {
    if (message.role === 'system') continue
    const text: string[] = []
    const flush = (): void => {
      if (text.length === 0) return
      parts.push(`[${message.role === 'assistant' ? 'Assistant' : 'User'}]: ${text.join('\n')}`)
      text.length = 0
    }
    for (const block of message.content) {
      switch (block.type) {
        case 'text':
          text.push(block.text)
          break
        case 'image':
          text.push('[image]')
          break
        case 'file':
          text.push('[file]')
          break
        case 'reasoning':
          flush()
          parts.push(`[Assistant reasoning]: ${truncateForTranscript(block.text)}`)
          break
        case 'tool-call':
          flush()
          parts.push(`[Assistant tool call]: ${block.name}(${truncateForTranscript(block.arguments)})`)
          break
        case 'tool-result':
          flush()
          parts.push(`[${block.isError === true ? 'Tool error' : 'Tool result'}]: ${truncateForTranscript(blockText(block.content))}`)
          break
        default:
          break
      }
    }
    flush()
  }
  return parts.join('\n\n')
}

/**
 * Build the bounded summarizer input for one span.
 * @param messages - the span's derived messages in surface order, without the system head.
 * @returns one transcript user message, with no system prompt and no tools.
 */
export function boundedSummarizationInput(messages: readonly Message[]): SummarizationInput {
  return {
    messages: [createUserMessage({
      content: [{
        type: 'text',
        text: `${TRANSCRIPT_PREAMBLE}\n\n<conversation>\n${serializeTranscript(messages)}\n</conversation>`,
      }],
      source: { kind: 'plugin', plugin: 'dsh-compaction-basic' },
    })],
  }
}

/** Flatten nested tool-result content to text with media placeholders. */
function blockText(blocks: readonly ContentBlock[]): string {
  return blocks.map((block) => {
    switch (block.type) {
      case 'text':
      case 'reasoning':
        return block.text
      case 'image':
        return '[image]'
      case 'file':
        return '[file]'
      case 'tool-result':
        return blockText(block.content)
      default:
        return ''
    }
  }).filter(text => text.length > 0).join('\n')
}
