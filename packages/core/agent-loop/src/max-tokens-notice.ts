/**
 * Model-facing notice for tool calls a reply lost at the output-token limit.
 * The assembler drops every tool call from a `max-tokens` reply because its
 * arguments may be incomplete, so the logged assistant message keeps no trace
 * of them; this notice tells the model they did not run.
 * @module dsh-agent-loop/max-tokens-notice
 */

import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { UserMessage } from 'bake-session'

/** Source stamped on the notice so derived history does not present it as a user prompt. */
const SOURCE = '@deepseek-ai/dsh-agent-loop'

/** Fixed guidance that follows the opening sentence. */
const GUIDANCE = 'Issue the calls you still need again, keeping each one small enough to finish '
  + 'within a single reply; for example, split large file content across several calls.'

/**
 * Build the next-step context for one reply whose tool calls were dropped.
 * @param names - dropped tool-call names in stream order; empty names (cut
 *   off before the name streamed) are omitted from the listing.
 * @returns a plugin-sourced user message naming the calls that did not run.
 */
export function droppedToolCallsNotice(names: readonly string[]): UserMessage {
  const listed = names.filter(name => name !== '')
  const opening = listed.length === 0
    ? 'Your previous reply was cut off at the output token limit, so its tool calls did not run.'
    : `Your previous reply was cut off at the output token limit, so its tool calls did not run: ${listed.join(', ')}.`
  return createUserMessage({
    content: [{ type: 'text', text: `${opening} ${GUIDANCE}` }],
    source: { kind: 'plugin', plugin: SOURCE },
  })
}
