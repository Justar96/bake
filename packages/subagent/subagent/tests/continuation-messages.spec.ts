import { describe, expect, it } from 'vitest'
import { ToolCallId, type ContentBlock } from 'bake-llm'
import { SessionId } from 'bake-session'
import { createSettlementMessage, withContinuableReturnGuidance } from '../src/continuation-messages.ts'

const childId = SessionId('settled-child')
const summary = { type: 'text', text: `Background subagent ${childId} finished and will do no further work unless you send it more.` }
const reasoning: ContentBlock = { type: 'reasoning', text: 'private child reasoning' }
const toolCall: ContentBlock = { type: 'tool-call', id: ToolCallId('child-call'), name: 'read', arguments: '{}' }

it('appends automatic-return guidance after the unchanged task', () => {
  const prompt: ContentBlock[] = [{ type: 'text', text: 'Review the changes.' }]
  const original = structuredClone(prompt)
  const guided = withContinuableReturnGuidance(SessionId('parent'), prompt)
  expect(guided.slice(0, prompt.length)).toEqual(original)
  expect(prompt).toEqual(original)
  expect(guided.at(-1)).toEqual({ type: 'text', text:
    'Your parent agent id is "parent". Your final answer is delivered to the parent automatically; '
    + 'make it a self-contained result. The parent shares your workspace but does not receive your transcript, '
    + 'tool output, or reasoning. When an earlier finding changes what the parent should do next, use '
    + 'send_message({ agent_id: "parent", message: "<actionable finding>" }); sending a message does not end your turn.',
  })
})

describe('continuable settlement content', () => {
  it.each([
    ['reasoning before the answer', [reasoning, { type: 'text', text: 'answer' }]],
    ['a tool call after the answer', [{ type: 'text', text: 'answer' }, toolCall]],
  ] satisfies [string, ContentBlock[]][])('reports only the closing text with %s', (_label, output) => {
    const original = structuredClone(output)
    const message = createSettlementMessage(childId, { stopReason: 'completed', output })

    expect(message.role).toBe('user')
    expect(message.content).toEqual([
      summary,
      { type: 'text', text: 'Its closing message:' },
      { type: 'text', text: 'answer' },
    ])
    expect(output).toEqual(original)
  })

  it.each([
    ['absent output', undefined],
    ['empty output', []],
    ['reasoning-only output', [reasoning]],
    ['empty text', [{ type: 'text', text: '' }]],
  ] satisfies [string, ContentBlock[] | undefined][])('reports no closing message for %s', (_label, output) => {
    const message = createSettlementMessage(childId, { stopReason: 'completed', ...output === undefined ? {} : { output } })

    expect(message.content).toEqual([
      summary,
      { type: 'text', text: 'It left no closing message.' },
    ])
  })

  it('preserves text block order and bytes around omitted reasoning and tool calls', () => {
    const first: ContentBlock = { type: 'text', text: '  first\n' }
    const second: ContentBlock = { type: 'text', text: '\n第二段  ' }
    const message = createSettlementMessage(childId, {
      stopReason: 'completed',
      output: [reasoning, first, toolCall, second],
    })

    expect(message.content).toEqual([
      summary,
      { type: 'text', text: 'Its closing message:' },
      first,
      second,
    ])
  })
})
