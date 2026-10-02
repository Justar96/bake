/**
 * Harness request-history conversion into pi-ai's Context vocabulary.
 *
 * @module dsh-llm-pi-ai/context
 */

import { brandString } from '@deepseek-ai/dsh-brand'
import { contentHasImage, IMAGE_OFFLOAD_REQUIRED_CODE, LlmError, offloadedImageText, projectOffloadedImages, requestImageHandleText, requiredImageOffload } from '@deepseek-ai/dsh-llm'
import type { ContentBlock, GenerateOptions, ImageAttachmentAccessResolver, Message, ToolCallId } from '@deepseek-ai/dsh-llm'
import type {
  AttachmentId,
  AttachmentStore,
  ImageAttachmentRef,
  ImageRequestTarget,
  RequestImageAttachment,
} from '@deepseek-ai/dsh-attachment'
import type {
  Context as PiContext,
  ImageContent,
  Message as PiMessage,
  SystemMessage as PiSystemMessage,
  TextContent,
  Tool as PiTool,
} from '@earendil-works/pi-ai'
import { toPiAssistant } from './replay.ts'
import { longEdgeDimensions, requestImageDimensions } from '@deepseek-ai/dsh-attachment'
import { DEFAULT_REQUEST_IMAGE_MAX_BYTES, DEFAULT_REQUEST_IMAGE_PIXEL_BUDGET } from './config.ts'

/** Join the text blocks of a harness message. */
function flattenText(message: Message): string {
  return message.content
    .filter(block => block.type === 'text')
    .map(block => block.text)
    .join('')
}


/** Flatten text recursively inside one tool result. */
function toolResultText(blocks: readonly ContentBlock[]): string {
  return blocks.map(block => block.type === 'text'
    ? block.text
    : block.type === 'tool-result' ? toolResultText(block.content) : '').join('')
}

/** Reject image roles that pi-ai cannot replay before request-size offloading can replace them. */
function assertSupportedImageRoles(messages: readonly Message[]): void {
  for (const message of messages) {
    if (message.role !== 'user' && contentHasImage(message.content)) {
      throw new LlmError(
        `pi-ai cannot represent an image in an in-history ${message.role} message`,
        'UNSUPPORTED_CONTENT',
      )
    }
  }
}

async function userContent(
  blocks: readonly ContentBlock[],
  requestImages: ReadonlyMap<AttachmentId, RequestImageAttachment>,
  resolveImageAccess: ImageAttachmentAccessResolver,
): Promise<string | (TextContent | ImageContent)[]> {
  const content: (TextContent | ImageContent)[] = []
  for (const block of blocks) {
    switch (block.type) {
      case 'text':
        if (block.text.length > 0) content.push({ type: 'text', text: block.text })
        break
      case 'image': {
        const version = requestImages.get(block.attachment.attachmentId) as RequestImageAttachment
        content.push({
          type: 'text',
          text: requestImageHandleText(block.attachment, version, resolveImageAccess(block.attachment)),
        })
        content.push({
          type: 'image',
          data: Buffer.from(version.data).toString('base64'),
          mimeType: version.mediaType,
        })
        break
      }
      case 'tool-result':
        {
          const nested = await userContent(block.content, requestImages, resolveImageAccess)
          if (typeof nested === 'string') {
            if (nested.length > 0) content.push({ type: 'text', text: nested })
          } else {
            content.push(...nested)
          }
        }
        break
      default:
        // Other merge-extensible blocks are not user-input vocabulary for pi-ai.
        break
    }
  }
  if (content.every(block => block.type === 'text')) return content.map(block => block.text).join('')
  return content
}

function collectImageRefs(
  blocks: readonly ContentBlock[],
  refs: Map<AttachmentId, ImageAttachmentRef>,
): void {
  for (const block of blocks) {
    if (block.type === 'image') {
      if (block.offloaded !== true) refs.set(block.attachment.attachmentId, block.attachment)
    } else if (block.type === 'tool-result') {
      collectImageRefs(block.content, refs)
    }
  }
}

async function prepareRequestImages(
  messages: readonly Message[],
  attachments: AttachmentStore,
  budget: PiImageRequestBudget,
  signal?: AbortSignal,
): Promise<Map<AttachmentId, RequestImageAttachment>> {
  const refs = new Map<AttachmentId, ImageAttachmentRef>()
  for (const message of messages) collectImageRefs(message.content, refs)
  const orderedRefs = [...refs.values()]
  const prepared = await Promise.all(orderedRefs.map(
    ref => attachments.readImageRequest(ref, requestImageTarget(ref, budget), signal),
  ))
  const versions = new Map<AttachmentId, RequestImageAttachment>()
  for (const [index, ref] of orderedRefs.entries()) {
    versions.set(ref.attachmentId, prepared[index] as RequestImageAttachment)
  }
  return versions
}

/** Route-declared transcript handling the conversion honours; see `LlmResolvedModelInfo`. */
export interface PiTranscriptUpdates {
  /** The route reads a later `system` message as the complete effective system prompt. */
  systemPromptUpdate?: 'in-history'
}

function piTool(tool: NonNullable<GenerateOptions['tools']>[number]): PiTool {
  return {
    name: tool.name,
    description: tool.description,
    // ToolSchema.parameters is a JSON Schema object; pi-ai's TSchema
    // (TypeBox) is structurally JSON Schema, so it assigns directly.
    parameters: tool.parameters,
  }
}

/**
 * The tools active from the first request: every declaration the seam did not
 * defer. A deferred declaration becomes available only through the tool change
 * that adds it, so the leading tool set must not include it.
 */
function initialToolsOf(options: GenerateOptions): PiTool[] | undefined {
  return options.tools?.filter(tool => tool.deferLoading !== true).map(piTool)
}

/**
 * Index the seam's tool changes as pi-ai system messages by the message they
 * follow. Each addition carries its complete definition from `options.tools`,
 * because pi-ai declares a later tool from the system message that adds it.
 * pi-ai then decides the wire form: native `tool_addition` and `tool_removal`
 * blocks where the model's compat accepts them, otherwise the folded current
 * tool set, so a route that cannot carry the change still sends the right tools.
 */
function toolChangesOf(options: GenerateOptions): Map<string, PiSystemMessage[]> {
  const definitions = new Map((options.tools ?? []).map(tool => [tool.name, tool]))
  const changes = new Map<string, PiSystemMessage[]>()
  for (const update of options.toolUpdates ?? []) {
    const toolsAdded = update.additions.map((name) => {
      const tool = definitions.get(name)
      if (tool === undefined) {
        throw new LlmError(`pi-ai tool update adds "${name}", which the request does not declare`, 'INVALID_REQUEST')
      }
      return piTool(tool)
    })
    const message: PiSystemMessage = {
      role: 'system',
      content: '',
      ...toolsAdded.length === 0 ? {} : { toolsAdded },
      ...update.removals.length === 0 ? {} : { toolsRemoved: update.removals.map(name => ({ name })) },
      timestamp: 0,
    }
    changes.set(update.afterMessageId, [...changes.get(update.afterMessageId) ?? [], message])
  }
  return changes
}

/**
 * Place the tool changes anchored on one converted harness message. pi-ai holds
 * a later system message until the next assistant message, so a change after a
 * user turn's tool results still lands before the reply it governs.
 */
function appendToolChanges(
  message: Message,
  changes: Map<string, PiSystemMessage[]>,
  messages: PiMessage[],
): void {
  const anchored = changes.get(message.id)
  if (anchored === undefined) return
  if (message.role !== 'user') {
    throw new LlmError(`pi-ai tool update follows a ${message.role} message; only a user turn can anchor one`, 'INVALID_REQUEST')
  }
  messages.push(...anchored)
  changes.delete(message.id)
}

/** Refuse tool changes whose anchoring message is not in the request. */
function assertToolChangesPlaced(changes: ReadonlyMap<string, PiSystemMessage[]>): void {
  if (changes.size > 0) {
    throw new LlmError('pi-ai tool update follows a message absent from the request', 'INVALID_REQUEST')
  }
}

/**
 * Convert one non-leading harness `system` message. A route reading the
 * latest system message as the whole prompt receives it as a pi-ai system
 * message in place; every other route folds it into a `user` message to
 * preserve order, since its single system slot already holds the prompt.
 */
function laterSystemMessage(message: Message, transcript: PiTranscriptUpdates | undefined): PiMessage {
  const text = flattenText(message)
  if (transcript?.systemPromptUpdate !== 'in-history') return { role: 'user', content: text, timestamp: 0 }
  // An empty snapshot would leave the previous prompt in force on the wire
  // while the Session recorded an empty one.
  if (text.length === 0) throw new LlmError('pi-ai cannot send an empty in-history system prompt', 'INVALID_REQUEST')
  return { role: 'system', content: text, timestamp: 0 }
}

/** The request split into pi-ai's single `systemPrompt` slot and the history that converts to `messages`. */
interface SystemPromptSplit {
  /** Text for pi-ai's `systemPrompt`; `undefined` sends no system prompt. */
  systemPrompt: string | undefined
  /** History messages that convert to pi-ai `messages`. */
  messages: readonly Message[]
}

/**
 * Select the pi-ai `systemPrompt` source shared by both conversion paths.
 * `options.system` wins when defined and every history message converts,
 * including a leading `system` message. Otherwise a leading `system` history
 * message supplies the prompt and leaves the converted history; empty leading
 * text sends no prompt. On an in-history route the whole leading run of system
 * messages supplies it, the last one winning, because each is a complete
 * prompt and no conversation precedes them.
 */
function splitSystemPrompt(options: GenerateOptions, transcript: PiTranscriptUpdates | undefined): SystemPromptSplit {
  if (options.system !== undefined) return { systemPrompt: options.system, messages: options.messages }
  const leading = transcript?.systemPromptUpdate === 'in-history'
    ? options.messages.findIndex(message => message.role !== 'system')
    : options.messages[0]?.role === 'system' ? 1 : 0
  const count = leading === -1 ? options.messages.length : leading
  if (count === 0) return { systemPrompt: undefined, messages: options.messages }
  const text = flattenText(options.messages[count - 1] as Message)
  return { systemPrompt: text.length > 0 ? text : undefined, messages: options.messages.slice(count) }
}

/** Assemble the request-level pi-ai context envelope shared by both conversion paths. */
function piContext(systemPrompt: string | undefined, options: GenerateOptions, messages: PiMessage[]): PiContext {
  const tools = initialToolsOf(options)
  return {
    ...systemPrompt !== undefined ? { systemPrompt } : {},
    messages,
    ...tools !== undefined && tools.length > 0 ? { tools } : {},
  }
}

function appendAssistant(
  message: Message,
  messages: PiMessage[],
  toolNames: Map<ToolCallId, string>,
  onReplayDegrade?: (reason: string) => void,
): void {
  const assistant = toPiAssistant(message, onReplayDegrade)
  for (const block of assistant.content) {
    if (block.type === 'toolCall') toolNames.set(brandString<ToolCallId>(block.id), block.name)
  }
  messages.push(assistant)
}

function textOnlyContext(
  options: GenerateOptions,
  onReplayDegrade?: (reason: string) => void,
  transcript?: PiTranscriptUpdates,
): PiContext {
  assertSupportedImageRoles(options.messages)
  const split = splitSystemPrompt(options, transcript)
  const changes = toolChangesOf(options)
  const toolNames = new Map<ToolCallId, string>()
  const messages: PiMessage[] = []
  for (const message of split.messages) {
    if (contentHasImage(message.content)) {
      throw new LlmError('pi-ai image conversion requires the durable attachment service', 'UNSUPPORTED_CONTENT')
    }
    if (message.role === 'system') {
      messages.push(laterSystemMessage(message, transcript))
      appendToolChanges(message, changes, messages)
      continue
    }
    if (message.role === 'assistant') {
      appendAssistant(message, messages, toolNames, onReplayDegrade)
      appendToolChanges(message, changes, messages)
      continue
    }
    const text = flattenText(message)
    const results = message.content.filter(block => block.type === 'tool-result')
    if (text.length > 0 || results.length === 0) messages.push({ role: 'user', content: text, timestamp: 0 })
    for (const result of results) {
      messages.push({
        role: 'toolResult',
        toolCallId: result.toolCallId,
        toolName: toolNames.get(result.toolCallId) ?? 'unknown',
        content: [{
          type: 'text',
          text: toolResultText(result.content) || '(no output)',
        }],
        isError: result.isError ?? false,
        timestamp: 0,
      })
    }
    appendToolChanges(message, changes, messages)
  }
  assertToolChangesPlaced(changes)
  return piContext(split.systemPrompt, options, messages)
}

/** Inputs that bind deterministic request images to one current tool execution world. */
export interface PiImageRequestContext {
  /** Durable provider that resolves request-image bytes and provider-owned host objects. */
  attachments: AttachmentStore
  /** Resolve current tool access separately from deterministic request-image versions. */
  resolveImageAccess: ImageAttachmentAccessResolver
  /** Request-level bound on the base64-encoded payload of retained images; omission leaves the bound unchecked. */
  maxRequestImageBytes?: number
  /** Route pixel, long-edge, and raw encoded-byte budgets; omission applies the default budgets without a long-edge cap. */
  requestImagePolicy?: PiImageRequestBudget
}

/** Per-route budgets from which each request image's target is derived. */
export interface PiImageRequestBudget {
  /** Total-pixel budget; larger sources are downscaled proportionally. */
  maxPixels: number
  /** Long-edge cap in pixels applied after the pixel budget; omission applies the pixel budget alone. */
  maxDimension?: number
  /** Encoded-byte target for one request image. */
  maxBytes: number
}

/**
 * Deterministic request target for one source under the route budgets. The
 * pixel budget applies first; a long edge still above the cap is then
 * recomputed from the source dimensions, so the short edge rounds once, as the
 * attachment provider's long-edge-only resize derives it, and the result stays
 * inside both bounds. The target depends only on the source dimensions and the
 * budgets, never on the other images in the request, so an image's request
 * bytes do not change as history grows.
 * @param source - intrinsic dimensions of the normalized attachment.
 * @param budget - the dispatching model's pixel budget, long-edge cap, and byte target.
 * @returns aspect-preserving dimensions of at least 1 px per side, never enlarged, beside the byte target.
 */
export function requestImageTarget(
  source: Pick<ImageAttachmentRef, 'width' | 'height'>,
  budget: PiImageRequestBudget,
): ImageRequestTarget {
  const budgeted = requestImageDimensions(source.width, source.height, budget.maxPixels)
  const dimensions = budget.maxDimension !== undefined && Math.max(budgeted.width, budgeted.height) > budget.maxDimension
    ? longEdgeDimensions(source.width, source.height, budget.maxDimension)
    : budgeted
  return { ...dimensions, maxBytes: budget.maxBytes }
}

/**
 * Convert text-only harness history to a synchronous pi-ai Context. Tool
 * result names are recovered from preceding assistant tool calls.
 * @param options - the harness request; `options.system`, else a leading `system` message, maps to pi-ai's single `systemPrompt` slot.
 * @param images - absent; selects the synchronous conversion.
 * @param onReplayDegrade - forwarded to {@link toPiAssistant} for each assistant message.
 * @param transcript - the dispatching route's declared transcript handling.
 * @returns the pi-ai context; `tools` is omitted when the request declares none.
 * @throws {LlmError} `UNSUPPORTED_CONTENT` for images in any history role, including a leading system message.
 * @throws {LlmError} `INVALID_REQUEST` for a tool update without its anchoring user message or added definition.
 */
export function toPiContext(
  options: GenerateOptions,
  images?: undefined,
  onReplayDegrade?: (reason: string) => void,
  transcript?: PiTranscriptUpdates,
): PiContext
/**
 * Convert harness history to a pi-ai Context while resolving durable images.
 * Tool result names are recovered from preceding assistant tool calls. Image
 * occurrences the surface marks offloaded become text placeholders; when the
 * retained occurrences' exact base64 payload still exceeds
 * `maxRequestImageBytes`, the call fails with `IMAGE_OFFLOAD_REQUIRED` naming
 * how many more oldest occurrences must be offloaded.
 * @param options - the harness request; `options.system`, else a leading `system` message, maps to pi-ai's single `systemPrompt` slot.
 * @param images - attachment provider, current path resolver, and request limits.
 * @param onReplayDegrade - forwarded to {@link toPiAssistant} for each assistant message.
 * @param transcript - the dispatching route's declared transcript handling.
 * @returns the asynchronously resolved pi-ai context.
 */
export function toPiContext(
  options: GenerateOptions,
  images: PiImageRequestContext,
  onReplayDegrade?: (reason: string) => void,
  transcript?: PiTranscriptUpdates,
): Promise<PiContext>
export function toPiContext(
  options: GenerateOptions,
  images?: PiImageRequestContext,
  onReplayDegrade?: (reason: string) => void,
  transcript?: PiTranscriptUpdates,
): PiContext | Promise<PiContext> {
  return images === undefined
    ? textOnlyContext(options, onReplayDegrade, transcript)
    : toPiContextWithImages(options, images, onReplayDegrade, transcript)
}

async function toPiContextWithImages(
  options: GenerateOptions,
  images: PiImageRequestContext,
  onReplayDegrade?: (reason: string) => void,
  transcript?: PiTranscriptUpdates,
): Promise<PiContext> {
  const { attachments, resolveImageAccess, maxRequestImageBytes } = images
  const requestImagePolicy = images.requestImagePolicy ?? {
    maxPixels: DEFAULT_REQUEST_IMAGE_PIXEL_BUDGET,
    maxBytes: DEFAULT_REQUEST_IMAGE_MAX_BYTES,
  }
  assertSupportedImageRoles(options.messages)
  const split = splitSystemPrompt(options, transcript)
  const changes = toolChangesOf(options)
  const requestImages = await prepareRequestImages(split.messages, attachments, requestImagePolicy, options.signal)
  if (maxRequestImageBytes !== undefined) {
    const offloadImages = requiredImageOffload(
      split.messages,
      { representation: 'base64', maxBytes: maxRequestImageBytes },
      block => (requestImages.get(block.attachment.attachmentId) as RequestImageAttachment).bytes,
    )
    if (offloadImages > 0) {
      throw new LlmError(
        `pi-ai request images exceed the ${maxRequestImageBytes}-byte base64 bound; ${offloadImages} more oldest occurrence(s) must be offloaded.`,
        IMAGE_OFFLOAD_REQUIRED_CODE,
        { offloadImages },
      )
    }
  }
  const exactMessages = projectOffloadedImages(
    split.messages,
    ref => offloadedImageText(ref, resolveImageAccess(ref)),
  )
  const toolNames = new Map<ToolCallId, string>()
  const messages: PiMessage[] = []

  for (const message of exactMessages) {
    if (message.role === 'system') {
      messages.push(laterSystemMessage(message, transcript))
      appendToolChanges(message, changes, messages)
      continue
    }
    if (message.role === 'assistant') {
      appendAssistant(message, messages, toolNames, onReplayDegrade)
      appendToolChanges(message, changes, messages)
      continue
    }
    // user role: text + tool results (each result becomes its own message).
    const regular = message.content.filter(block => block.type !== 'tool-result')
    const content = await userContent(regular, requestImages, resolveImageAccess)
    const results = message.content.filter((block): block is Extract<ContentBlock, { type: 'tool-result' }> => (
      block.type === 'tool-result'
    ))
    if (content.length > 0 || results.length === 0) {
      messages.push({ role: 'user', content, timestamp: 0 })
    }
    for (const result of results) {
      const resultContent = await userContent(result.content, requestImages, resolveImageAccess)
      messages.push({
        role: 'toolResult',
        toolCallId: result.toolCallId,
        toolName: toolNames.get(result.toolCallId) ?? 'unknown',
        content: typeof resultContent === 'string'
          ? [{ type: 'text', text: resultContent || '(no output)' }]
          : resultContent,
        isError: result.isError ?? false,
        timestamp: 0,
      })
    }
    appendToolChanges(message, changes, messages)
  }

  assertToolChangesPlaced(changes)
  return piContext(split.systemPrompt, options, messages)
}
