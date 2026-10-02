/**
 * Route-configured rewrites of the Anthropic Messages request body pi-ai
 * builds, applied through its `onPayload` hook just before the request is sent.
 * Each rewrite reshapes the wire only; the logged history and pi-ai's context
 * are untouched, so the model-visible request stays reconstructable from the
 * session log and the same profile.
 *
 * @module dsh-llm-pi-ai/payload
 */

import type { PiAiMessagesWire, ResolvedPiAiProviderProfile } from './config.ts'

/**
 * Name of the never-callable deferred tool pi-ai declares beside native tool
 * changes. pi-ai does not export it; the drift test pins the spelling against
 * an actual request.
 */
export const DEFERRED_TOOL_PLACEHOLDER = '__pi_deferred_placeholder__'

/** The parts of a Messages body the rewrites read; everything else passes through. */
interface MessagesBody {
  thinking?: { type?: unknown }
  system?: unknown
  tools?: unknown
  messages?: unknown
  [key: string]: unknown
}

/** One rewrite: the changed body, or `undefined` when it has nothing to change. */
type PayloadStep = (body: MessagesBody) => MessagesBody | undefined

/** A block or tool as the rewrites see it. */
type WireRecord = Record<string, unknown>

function isRecord(value: unknown): value is WireRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * Respell pi-ai's adaptive `thinking` block as `enabled` for an endpoint that
 * reads `output_config.effort` but documents only `enabled` and `disabled`.
 * The effort itself is untouched; a request that is not adaptive passes through.
 */
function enabledThinking(body: MessagesBody): MessagesBody | undefined {
  if (body.thinking?.type !== 'adaptive') return undefined
  return { ...body, thinking: { type: 'enabled' } }
}

/**
 * Drop the deferred placeholder tool, unless no tool would then remain that
 * loads eagerly: a request whose tools are all deferred is one Anthropic
 * refuses, and pi-ai relies on the placeholder only beside eager tools.
 */
function withoutDeferredPlaceholder(body: MessagesBody): MessagesBody | undefined {
  if (!Array.isArray(body.tools)) return undefined
  const tools: unknown[] = body.tools
  const kept = tools.filter(tool => !(isRecord(tool) && tool['name'] === DEFERRED_TOOL_PLACEHOLDER))
  if (kept.length === tools.length) return undefined
  if (!kept.some(tool => isRecord(tool) && tool['defer_loading'] !== true)) return undefined
  return { ...body, tools: kept }
}

/** One record without its `cache_control` key, and whether it carried one. */
function uncached(value: unknown): { value: unknown; changed: boolean } {
  if (!isRecord(value)) return { value, changed: false }
  let changed = false
  let result: WireRecord = value
  if ('cache_control' in value) {
    const { cache_control: _cacheControl, ...rest } = value
    result = rest
    changed = true
  }
  // A tool result nests its own content blocks, which pi-ai may mark too.
  if (Array.isArray(result['content'])) {
    const nested = uncachedList(result['content'])
    if (nested !== undefined) {
      result = { ...result, content: nested }
      changed = true
    }
  }
  return { value: result, changed }
}

/** A list with every element's `cache_control` removed, or `undefined` when none had one. */
function uncachedList(list: readonly unknown[]): unknown[] | undefined {
  let changed = false
  const result = list.map((item) => {
    const next = uncached(item)
    changed ||= next.changed
    return next.value
  })
  return changed ? result : undefined
}

/**
 * Remove every `cache_control` breakpoint: on system blocks, tools, and each
 * message's content blocks. Only these structural positions are visited, so a
 * tool parameter or text that happens to be named `cache_control` survives.
 */
function withoutCacheControl(body: MessagesBody): MessagesBody | undefined {
  const system = Array.isArray(body.system) ? uncachedList(body.system) : undefined
  // A tool is stripped at its top level only, so its input schema is never visited.
  const tools = Array.isArray(body.tools) && body.tools.some(tool => isRecord(tool) && 'cache_control' in tool)
    ? body.tools.map(uncachedTool)
    : undefined
  let messagesChanged = false
  const messages = Array.isArray(body.messages)
    ? body.messages.map((message: unknown) => {
      if (!isRecord(message) || !Array.isArray(message['content'])) return message
      const content = uncachedList(message['content'])
      if (content === undefined) return message
      messagesChanged = true
      return { ...message, content }
    })
    : undefined
  if (system === undefined && tools === undefined && !messagesChanged) return undefined
  return {
    ...body,
    ...system === undefined ? {} : { system },
    ...tools === undefined ? {} : { tools },
    ...messagesChanged ? { messages } : {},
  }
}

/** One tool declaration without its top-level `cache_control`. */
function uncachedTool(tool: unknown): unknown {
  if (!isRecord(tool) || !('cache_control' in tool)) return tool
  const { cache_control: _cacheControl, ...rest } = tool
  return rest
}

/** A message's content as blocks, a plain string becoming one text block. */
function contentBlocks(content: unknown): unknown[] {
  if (Array.isArray(content)) return content
  if (typeof content === 'string') return [{ type: 'text', text: content }]
  return []
}

/**
 * Merge each run of adjacent same-role messages into one whose content is
 * their blocks in order. Order and every block survive, so the model reads the
 * same text; only the message boundaries between them go.
 */
function withMergedRoles(body: MessagesBody): MessagesBody | undefined {
  if (!Array.isArray(body.messages)) return undefined
  const merged: WireRecord[] = []
  let changed = false
  for (const message of body.messages as unknown[]) {
    const previous = merged.at(-1)
    if (isRecord(message) && previous !== undefined && previous['role'] === message['role']) {
      merged[merged.length - 1] = {
        ...previous,
        content: [...contentBlocks(previous['content']), ...contentBlocks(message['content'])],
      }
      changed = true
      continue
    }
    merged.push(message as WireRecord)
  }
  return changed ? { ...body, messages: merged } : undefined
}

/**
 * Build the `onPayload` hook one route's `anthropic-messages` requests use:
 * the adaptive-thinking respelling and each enabled {@link PiAiMessagesWire}
 * switch, composed in a fixed order so every one applies.
 * @param profile - the resolved route profile.
 * @returns the hook, or `undefined` when the route rewrites nothing.
 */
export function messagesPayloadHook(
  profile: Pick<ResolvedPiAiProviderProfile, 'adaptiveThinkingType' | 'messagesWire'>,
): ((payload: unknown) => unknown) | undefined {
  const steps = payloadSteps(profile.adaptiveThinkingType === 'enabled', profile.messagesWire)
  if (steps.length === 0) return undefined
  return (payload) => {
    if (!isRecord(payload)) return undefined
    let body: MessagesBody = payload
    let changed = false
    for (const step of steps) {
      const next = step(body)
      if (next === undefined) continue
      body = next
      changed = true
    }
    // `undefined` tells pi-ai to send its own body unchanged.
    return changed ? body : undefined
  }
}

function payloadSteps(enabled: boolean, wire: PiAiMessagesWire | undefined): PayloadStep[] {
  return [
    ...enabled ? [enabledThinking] : [],
    ...wire?.dropDeferredToolPlaceholder === true ? [withoutDeferredPlaceholder] : [],
    ...wire?.stripCacheControl === true ? [withoutCacheControl] : [],
    ...wire?.mergeAdjacentRoles === true ? [withMergedRoles] : [],
  ]
}
