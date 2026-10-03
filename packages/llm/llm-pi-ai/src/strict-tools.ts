/**
 * Opt-in strict JSON-schema tool declarations for OpenAI-family wires.
 *
 * pi-ai sends a tool with `strict: true` and its schema rewritten to the
 * strict subset (every property required, optional ones made nullable,
 * `additionalProperties: false`) when the tool asks for JSON-schema
 * constrained sampling and the model's compat declares `supportsStrictMode`.
 * The request asks with `prefer`, so a tool whose schema has no strict form
 * keeps its ordinary declaration instead of failing the request.
 *
 * Strict sampling makes the model write every property, `null` standing for
 * one it would have omitted. Harness tool validation reads an absent optional
 * property, not a `null` one, so each completed call to a strict tool drops
 * the `null`s that conversion introduced before the call is recorded.
 *
 * @module dsh-llm-pi-ai/strict-tools
 */

import type { StreamChunk, ToolSchema } from '@deepseek-ai/dsh-llm'
import type { Api, Context as PiContext, Model, Tool as PiTool } from '@earendil-works/pi-ai'
import { resolveJsonSchemaStrictSampling } from '@earendil-works/pi-ai/api/constrained-sampling'

/**
 * Protocols whose pi-ai implementation reads a tool's constrained-sampling
 * request as `strict` on the function declaration: the two OpenAI wires a
 * route may name.
 */
export const STRICT_TOOL_APIS: ReadonlySet<string> = new Set(['openai-responses', 'openai-completions'])

/**
 * Whether a request to this model can carry strict tool declarations: it
 * speaks one of {@link STRICT_TOOL_APIS} and its compat says the endpoint
 * accepts `strict`. pi-ai defaults the switch off on both wires.
 * @param model - the dispatching model descriptor.
 * @returns whether marking the tools changes the request.
 */
export function offersStrictTools(model: Model<Api>): boolean {
  return STRICT_TOOL_APIS.has(model.api)
    && (model.compat as { supportsStrictMode?: unknown } | undefined)?.supportsStrictMode === true
}

/** One tool asking for strict JSON-schema sampling where its schema allows it. */
function strictTool<T extends PiTool>(tool: T): T {
  return { ...tool, constrainedSampling: { type: 'json_schema', strict: 'prefer' } }
}

/**
 * Mark every tool a request declares, in the leading tool set and in each
 * later tool addition, as preferring strict JSON-schema sampling.
 * @param context - the converted request; left unmodified.
 * @returns a copy whose tool declarations carry the strict request.
 */
export function withStrictTools(context: PiContext): PiContext {
  return {
    ...context,
    ...context.tools === undefined ? {} : { tools: context.tools.map(strictTool) },
    messages: context.messages.map(message => message.role === 'system' && message.toolsAdded !== undefined
      ? { ...message, toolsAdded: message.toolsAdded.map(strictTool) }
      : message),
  }
}

type JsonSchemaNode = Record<string, unknown>

function isNode(value: unknown): value is JsonSchemaNode {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** Whether a schema already admits `null`, so strict conversion left it as written. */
function allowsNull(schema: unknown): boolean {
  if (!isNode(schema)) return false
  if (schema.type === 'null' || (Array.isArray(schema.type) && schema.type.includes('null'))) return true
  if (schema.const === null || (Array.isArray(schema.enum) && schema.enum.includes(null))) return true
  return Array.isArray(schema.anyOf) && schema.anyOf.some(allowsNull)
}

/**
 * Drop the `null` values strict conversion made possible: a property the
 * original schema neither requires nor lets be `null`, at any object depth
 * the conversion reached through `properties` and `items`.
 * @param schema - the tool's original parameter schema at this depth.
 * @param value - the parsed argument value at the same depth.
 * @returns the value without those properties; the same value when none appear.
 */
function dropIntroducedNulls(schema: unknown, value: unknown): unknown {
  if (!isNode(schema)) return value
  if (Array.isArray(value)) {
    if (!isNode(schema.items)) return value
    const items = value.map(item => dropIntroducedNulls(schema.items, item))
    return items.every((item, index) => item === value[index]) ? value : items
  }
  if (!isNode(value) || !isNode(schema.properties)) return value
  const properties = schema.properties
  const required = new Set(Array.isArray(schema.required) ? schema.required : [])
  let changed = false
  const kept: JsonSchemaNode = {}
  for (const [key, entry] of Object.entries(value)) {
    const property = properties[key]
    if (entry === null && !required.has(key) && property !== undefined && !allowsNull(property)) {
      changed = true
      continue
    }
    const next = dropIntroducedNulls(property, entry)
    if (next !== entry) changed = true
    kept[key] = next
  }
  return changed ? kept : value
}

/**
 * Build the per-request step that restores omission in completed calls to
 * the tools pi-ai sent strict. The decision repeats pi-ai's own, so a tool
 * whose schema had no strict form keeps its arguments exactly as streamed.
 * @param tools - every tool the request declares, with its original schema.
 * @returns a chunk mapper that rewrites only a strict tool's `block-end`.
 */
export function strictArgumentRestorer(tools: readonly ToolSchema[]): (chunk: StreamChunk) => StreamChunk {
  const strict = new Map<string, JsonSchemaNode>()
  for (const tool of tools) {
    const declared = strictTool({ name: tool.name, description: tool.description, parameters: tool.parameters as PiTool['parameters'] })
    if (resolveJsonSchemaStrictSampling(declared, true) === true) strict.set(tool.name, tool.parameters)
  }
  return (chunk) => {
    if (chunk.type !== 'block-end' || chunk.block.type !== 'tool-call') return chunk
    const schema = strict.get(chunk.block.name)
    if (schema === undefined) return chunk
    let parsed: unknown
    try {
      parsed = JSON.parse(chunk.block.arguments)
    } catch (_unparsableArguments) {
      return chunk
    }
    const restored = dropIntroducedNulls(schema, parsed)
    if (restored === parsed) return chunk
    return { ...chunk, block: { ...chunk.block, arguments: JSON.stringify(restored) } }
  }
}
