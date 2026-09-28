/**
 * The globally named `send_message` and `interrupt_agent` tools: thin
 * model-facing adapters over `ctx.subagents.sendMessage()` and
 * `ctx.subagents.interrupt()`. They perform no lifecycle routing of their own —
 * residency, cold resume, and interrupt authorization belong to the subagent
 * service — and they live apart from the provider-bound
 * `@deepseek-ai/dsh-tool-subagent` instances so multiple delegation tools share
 * one control API.
 * @module @deepseek-ai/dsh-tool-subagent-control
 */

import type { Context } from '@deepseek-ai/cordis'
import { brandString } from '@deepseek-ai/dsh-brand'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ContentBlock } from '@deepseek-ai/dsh-llm'
import type { SessionId } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-subagent'
import { markAdjacentAgentSendMessageTool } from '@deepseek-ai/dsh-subagent/internal'
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
      'Send a message to one of your direct continuable subagents by its agent id. If you are a continuable '
      + 'subagent, you can also message your parent. A working target reads the message at its next step; '
      + 'otherwise the message starts a new turn. You get delivery confirmation, not a reply, and an error '
      + 'means the message was not delivered.',
    parameters: {
      agent_id: {
        type: 'string',
        required: true,
        description: 'The agent id of a direct continuable subagent, or of your parent if you are a continuable subagent.',
      },
      message: {
        type: 'string',
        required: true,
        description: 'The message to send.',
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
      'Stop the current turn of a continuable subagent below you, whether a direct child or deeper, by its '
      + 'agent id. Messages already queued for it wait for a later send_message, subagents it started keep '
      + 'running, and it stays available for follow-ups. The call returns once the stop is requested, so the '
      + 'subagent may keep running briefly; interrupting one that has already finished does nothing.',
    parameters: {
      agent_id: {
        type: 'string',
        required: true,
        description: 'The agent id of the subagent to interrupt.',
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
