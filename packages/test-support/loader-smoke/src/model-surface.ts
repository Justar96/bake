/**
 * Model-surface snapshots: one shipped composition's first agent-loop request,
 * rendered as reviewable Markdown with its machine-specific values replaced by
 * placeholders.
 *
 * The request comes from the composition itself, driven to its first model
 * call through a keyless adapter, so nothing here assembles a prompt. This
 * module only plants a fixed workspace, normalizes the captured
 * provider-neutral request, and prints it.
 *
 * @module bake-loader-smoke/model-surface
 */

import { existsSync, realpathSync } from 'node:fs'
import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { GenerateOptions } from 'bake-llm'

/** The parts of one request a model sees; transport fields such as `signal` are left out. */
export type ModelSurfaceRequest = Pick<GenerateOptions,
  'system' | 'messages' | 'tools' | 'toolHistory' | 'toolUpdates' | 'reasoningEffort' | 'maxTokens' | 'temperature' | 'stop'>

/** One literal value and the placeholder that replaces it. */
export type ModelSurfacePlaceholder = readonly [value: string, placeholder: string]

/** Measured sizes of a normalized model surface, in UTF-16 code units (JavaScript string length). */
export interface ModelSurfaceSizes {
  /** The `system` field, then the text of every system-role message, joined by newlines. */
  readonly systemChars: number
  /** Compact `JSON.stringify` of the tool declarations, in request order. */
  readonly toolJsonChars: number
  /** Text of every non-system message before the first model reply. */
  readonly contextChars: number
  readonly toolCount: number
}

/** The task every model-surface scenario submits as the human's first message. */
export const MODEL_SURFACE_TASK = 'Summarize the workspace rules.'

/** Workspace instructions the scenarios plant, so the instructions message is pinned too. */
const SURFACE_AGENTS_MD = '# Rules\n\nKeep answers short and cite the files you read.\n'

/** The workspace skill the scenarios plant, so the skill catalog is pinned too. */
const SURFACE_SKILL_NAME = 'surface-skill'
const SURFACE_SKILL_MD = `---\nname: ${SURFACE_SKILL_NAME}\ndescription: Summarize a change for a reviewer.\n---\n\nList each changed file with one line on why it changed.\n`

/**
 * Plant the fixed model-surface workspace: an `AGENTS.md`, one workspace
 * skill, and a `.git` directory that stops instruction and skill discovery
 * from reaching any ancestor of the temporary directory.
 * @param workspace - an existing directory the caller owns.
 */
export async function plantModelSurfaceWorkspace(workspace: string): Promise<void> {
  await mkdir(join(workspace, '.git'), { recursive: true })
  await mkdir(join(workspace, '.agents', 'skills', SURFACE_SKILL_NAME), { recursive: true })
  await writeFile(join(workspace, 'AGENTS.md'), SURFACE_AGENTS_MD)
  await writeFile(join(workspace, '.agents', 'skills', SURFACE_SKILL_NAME, 'SKILL.md'), SURFACE_SKILL_MD)
}

/**
 * Every spelling a path can take in a request: as given, its real path (macOS
 * reports temporary directories under `/private`), and each with forward and
 * back slashes.
 * @param path - an absolute path.
 * @param placeholder - what replaces every spelling.
 * @returns the placeholder pairs for that path.
 */
export function pathPlaceholders(path: string, placeholder: string): ModelSurfacePlaceholder[] {
  const spellings = new Set([path])
  if (existsSync(path)) spellings.add(realpathSync(path))
  for (const spelling of [...spellings]) {
    spellings.add(spelling.replaceAll('\\', '/'))
    spellings.add(spelling.replaceAll('/', '\\'))
  }
  return [...spellings].filter(spelling => spelling.length > 1).map(spelling => [spelling, placeholder])
}

/** ISO dates and times, which a runtime snapshot may carry. */
const ISO_DATE = /\b\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?)?\b/gu

/**
 * Replace every placeholder value, longest first so a path never loses only
 * its prefix, then every ISO date.
 * @returns the normalized string.
 */
function normalizeString(text: string, placeholders: readonly ModelSurfacePlaceholder[]): string {
  let result = text.replaceAll('\r\n', '\n')
  for (const [value, placeholder] of placeholders) result = result.replaceAll(value, placeholder)
  return result.replace(ISO_DATE, '<date>')
}

/** @returns a deep copy of `value` with every string normalized; object key order is kept. */
function normalizeValue(value: unknown, placeholders: readonly ModelSurfacePlaceholder[]): unknown {
  if (typeof value === 'string') return normalizeString(value, placeholders)
  if (Array.isArray(value)) return value.map(item => normalizeValue(item, placeholders))
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, normalizeValue(item, placeholders)]))
  }
  return value
}

/**
 * Normalize one captured request.
 * @param request - the request as the adapter received it.
 * @param placeholders - machine-specific values to hide; the model id belongs here.
 * @returns the request with only its model-visible fields, every string normalized.
 */
export function normalizeModelSurface(
  request: ModelSurfaceRequest,
  placeholders: readonly ModelSurfacePlaceholder[],
): ModelSurfaceRequest {
  const ordered = [...placeholders].sort((left, right) => right[0].length - left[0].length)
  const visible: ModelSurfaceRequest = {
    ...request.system === undefined ? {} : { system: request.system },
    // Message ids and sources are log metadata no adapter sends as text.
    messages: request.messages.map(message => ({ role: message.role, content: message.content }) as typeof message),
    ...request.tools === undefined ? {} : { tools: request.tools },
    ...request.toolHistory === undefined ? {} : { toolHistory: request.toolHistory },
    ...request.toolUpdates === undefined ? {} : { toolUpdates: request.toolUpdates },
    ...request.reasoningEffort === undefined ? {} : { reasoningEffort: request.reasoningEffort },
    ...request.maxTokens === undefined ? {} : { maxTokens: request.maxTokens },
    ...request.temperature === undefined ? {} : { temperature: request.temperature },
    ...request.stop === undefined ? {} : { stop: request.stop },
  }
  return normalizeValue(visible, ordered) as ModelSurfaceRequest
}

type SurfaceMessage = ModelSurfaceRequest['messages'][number]

/** @returns the message's text blocks joined by newlines; other blocks as JSON lines. */
function messageText(message: SurfaceMessage): string {
  const content: unknown = message.content
  if (typeof content === 'string') return content
  return (content as readonly { type: string; text?: string }[])
    .map(block => block.type === 'text' && typeof block.text === 'string' ? block.text : JSON.stringify(block))
    .join('\n')
}

/**
 * Measure a normalized surface.
 * @param request - a request from {@link normalizeModelSurface}.
 * @returns its sizes.
 */
export function modelSurfaceSizes(request: ModelSurfaceRequest): ModelSurfaceSizes {
  const system = [
    ...request.system === undefined ? [] : [request.system],
    ...request.messages.filter(message => message.role === 'system').map(messageText),
  ].join('\n')
  const context = request.messages.filter(message => message.role !== 'system').map(messageText).join('\n')
  return {
    systemChars: system.length,
    toolJsonChars: JSON.stringify(request.tools ?? []).length,
    contextChars: context.length,
    toolCount: request.tools?.length ?? 0,
  }
}

/** @returns `text` in a fence longer than any backtick run inside it. */
function fenced(text: string, language = 'text'): string {
  const longest = Math.max(2, ...[...text.matchAll(/`+/gu)].map(match => match[0].length))
  const fence = '`'.repeat(longest + 1)
  return `${fence}${language}\n${text}${text.endsWith('\n') ? '' : '\n'}${fence}`
}

/** @returns whether two JSON values serialize identically. */
function sameJson(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right)
}

/**
 * Render a normalized surface as Markdown: sizes, request settings, the
 * `system` field when the request sets one, every message in order with its
 * exact text, and every tool in order with its exact description and
 * parameter schema.
 * @param title - the composition's name.
 * @param request - a request from {@link normalizeModelSurface}.
 * @returns the snapshot text, ending in one newline.
 */
export function renderModelSurface(title: string, request: ModelSurfaceRequest): string {
  const sizes = modelSurfaceSizes(request)
  const out: string[] = [
    `# Model surface: ${title}`,
    '',
    '<!-- Generated by a model-surface test; update with `-u`, never by hand. -->',
    '',
    '| Measure | Value |',
    '|---|---|',
    `| System prompt chars | ${String(sizes.systemChars)} |`,
    `| Tool JSON chars | ${String(sizes.toolJsonChars)} |`,
    `| Context message chars | ${String(sizes.contextChars)} |`,
    `| Tools | ${String(sizes.toolCount)} |`,
    '',
    '## Request settings',
    '',
    fenced(JSON.stringify({
      reasoningEffort: request.reasoningEffort ?? null,
      maxTokens: request.maxTokens ?? null,
      temperature: request.temperature ?? null,
      stop: request.stop ?? null,
    }, null, 2), 'json'),
  ]
  if (request.system !== undefined) out.push('', '## System', '', fenced(request.system))
  out.push('', '## Messages')
  request.messages.forEach((message, index) => {
    out.push('', `### ${String(index + 1)}. ${message.role}`, '', fenced(messageText(message)))
  })
  out.push('', `## Tools (${String(sizes.toolCount)})`)
  for (const tool of request.tools ?? []) {
    out.push('', `### ${tool.name}`, '')
    if (tool.deferLoading === true) out.push('Declared with `deferLoading`.', '')
    out.push(fenced(tool.description), '', fenced(JSON.stringify(tool.parameters, null, 2), 'json'))
  }
  const history = request.toolHistory
  if (history !== undefined) {
    out.push('', '## Tool history', '')
    out.push(sameJson(history.tools, request.tools ?? []) && history.updates.length === 0
      ? 'Same declarations as the tools above, with no updates.'
      : fenced(JSON.stringify(history, null, 2), 'json'))
  }
  if (request.toolUpdates !== undefined) {
    out.push('', '## Tool updates', '', fenced(JSON.stringify(request.toolUpdates, null, 2), 'json'))
  }
  return `${out.join('\n')}\n`
}
