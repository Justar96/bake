/**
 * The globally named `send_message` and `interrupt_agent` tools: thin
 * model-facing adapters over `ctx.subagents.sendMessage()` and
 * `ctx.subagents.interrupt()`. They perform no lifecycle routing of their own —
 * residency, cold resume, and interrupt authorization belong to the subagent
 * service — and they live apart from the provider-bound
 * `bake-tool-subagent` instances so multiple delegation tools share
 * one control API.
 * @module bake-tool-subagent-control
 */

import type { Context } from '@deepseek-ai/cordis'
import { brandString } from 'bake-brand'
import { defineTool } from 'bake-tools'
import type { ContentBlock } from 'bake-llm'
import type { SessionId } from 'bake-session'
import type {} from 'bake-subagent'
import { markAdjacentAgentSendMessageTool } from 'bake-subagent/internal'
import { presentInterruptCall, presentSendMessageCall } from './presentation.ts'

export const name = 'tool-subagent-control'
export const inject = ['tools', 'subagents']

/**
 * Register the `send_message` and `interrupt_agent` tools.
 * @param ctx - context carrying the tool registry and subagent service.
 */
export function apply(ctx: Context): void {
  ctx.tools.register(markAdjacentAgentSendMessageTool(defineTool({
    name: 'send_message',
    description:
      'Message a direct continuable subagent, or your parent if you are one. A busy target reads it next '
      + 'step; an idle one starts a new turn. Confirms delivery; no reply.',
    parameters: {
      agent_id: {
        type: 'string',
        required: true,
      },
      message: {
        type: 'string',
        required: true,
      },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          messageId: { type: 'string', required: true },
        },
      },
      render: (args, _value) => [{
        type: 'text',
        text: `message delivered to agent ${args.agent_id}`,
      }],
    },
    async execute(args, exec) {
      const sender = exec.agent
      if (!sender) {
        throw new Error('send_message requires a calling agent (exec.agent was undefined)')
      }
      const message: ContentBlock[] = [{ type: 'text', text: args.message }]
      const messageId = await ctx.subagents.sendMessage(
        sender,
        brandString<SessionId>(args.agent_id),
        message,
        { signal: exec.signal },
      )
      return { messageId }
    },
    presentCall: args => presentSendMessageCall(args),
  })))

  ctx.tools.register(defineTool({
    name: 'interrupt_agent',
    description:
      'Request a stop of a continuable subagent\'s current turn; it stays available, and its own '
      + 'subagents keep running.',
    parameters: {
      agent_id: {
        type: 'string',
        required: true,
      },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          accepted: { type: 'boolean', required: true },
        },
      },
      render: (args, _value) => [{
        type: 'text',
        text: `interrupt requested for agent ${args.agent_id}`,
      }],
    },
    execute(args, exec) {
      const caller = exec.agent
      if (!caller) {
        // Ancestor authority requires an exact live calling agent.
        throw new Error('interrupt_agent requires a calling agent (exec.agent was undefined)')
      }
      // The service authorizes the exact live caller against the target's
      // recorded lineage; the tool adds no authority of its own.
      ctx.subagents.interrupt(brandString<SessionId>(args.agent_id), { kind: 'ancestor', agent: caller })
      return Promise.resolve({ accepted: true })
    },
    presentCall: args => presentInterruptCall(args),
  }))
}
