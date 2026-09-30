/**
 * The registry's reserved on-demand details reader. A tool's native schema is
 * resent with every request, so a tool whose usage reference the model needs
 * only when it actually uses the tool declares that reference as
 * {@link ToolDefinition.details} and keeps its description short; this tool
 * returns the reference once, into the conversation, where the prompt cache
 * keeps it after the first read.
 *
 * @module @deepseek-ai/dsh-tools/tool-help
 */
import { defineTool } from './schema.ts'
import type { ToolDefinition } from './index.ts'
import type { ScopeKey } from '@deepseek-ai/dsh-scope'

/** Reserved name of the on-demand details reader. */
export const TOOL_HELP_NAME = 'tool_help'

/**
 * Build the details reader over one registry's scoped view.
 * @param lookup - resolves a name the way the calling scope sees it.
 * @returns the reserved definition the registry adds to a scope's view.
 */
export function createToolHelpTool(
  lookup: (name: string, scope: ScopeKey | undefined) => ToolDefinition | undefined,
): ToolDefinition {
  return defineTool({
    name: TOOL_HELP_NAME,
    description: 'Return the full usage reference of a tool whose description says to read it with tool_help first.',
    parameters: {
      name: { type: 'string', required: true, description: 'Name of the tool.' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          name: { type: 'string', required: true },
          details: { type: 'string', required: true },
        },
      },
      render: (_args, value) => [{ type: 'text', text: value.details }],
    },
    isConcurrencySafe: () => true,
    execute(args, exec) {
      const definition = lookup(args.name, exec.agent)
      if (definition?.details === undefined) {
        throw new Error(`tool "${args.name}" has no usage reference; its description is complete`)
      }
      return Promise.resolve({ name: definition.name, details: definition.details })
    },
  })
}
