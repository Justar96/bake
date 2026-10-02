/**
 * Schema checks for the MCP values this bridge reads, records, or shows the
 * model.
 *
 * pi-mcp checks only the envelope of each result. These parsers apply the MCP
 * 2025-11-25 schema to tool definitions, tool results, and resource results,
 * and rebuild each object in schema field order. Unknown fields are dropped
 * except inside JSON Schema objects and `_meta`, so tool schemas, recorded
 * tool values, and resource results keep the bytes that earlier releases
 * produced for the same server output.
 *
 * Every parser throws a `TypeError` whose message names the failing path,
 * such as `content[0].data must be base64`; `result` names the whole value.
 *
 * @module
 */

import type { JsonValue } from '@deepseek-ai/dsh-util-values'

/** A protocol object with unknown provenance. */
type ProtocolObject = Record<string, unknown>

/** Parse one field value at a diagnostic path. */
type FieldParser = (value: unknown, path: string) => unknown

/** One schema field: its parser and whether the field may be absent. */
interface Field {
  readonly parse: FieldParser
  readonly optional: boolean
}

/** Schema fields in output order. */
type Shape = Readonly<Record<string, Field>>

/** The tool fields the bridge consumes, in MCP 2025-11-25 schema form. */
export interface ListedTool {
  /** The server's own tool name. */
  name: string
  /** Model-facing description, when the server supplies one. */
  description?: string
  /** JSON Schema for the tool arguments, ordered `type`, `properties`, `required`, then other keywords. */
  inputSchema: Record<string, unknown>
  /** JSON Schema for structured results, ordered like {@link inputSchema}. */
  outputSchema?: Record<string, unknown>
  /** Task execution support declared by the server. */
  execution?: { taskSupport?: 'required' | 'optional' | 'forbidden' }
}

/** The tool-result fields the bridge consumes, with content blocks in schema form. */
export interface ParsedCallToolResult {
  /** Ordered content blocks; an absent `content` field parses as an empty list. */
  content: JsonValue[]
  /** Structured result value, when the server returned one. */
  structuredContent?: JsonValue
  /** Whether the server reported a tool-level error. */
  isError?: boolean
}

/** RFC 3339 date-time with seconds and a `Z` or numeric offset, as the MCP schema requires. */
const DATE_TIME = /^(\d{4})-(\d{2})-(\d{2})T(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/

function isObject(value: unknown): value is ProtocolObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function fail(path: string, expectation: string): never {
  throw new TypeError(`${path === '' ? 'result' : path} ${expectation}`)
}

function join(path: string, key: string): string {
  return path === '' ? key : `${path}.${key}`
}

/** Add an own enumerable property, including a `__proto__` key from JSON input. */
function define(target: ProtocolObject, key: string, value: unknown): void {
  Object.defineProperty(target, key, { value, enumerable: true, writable: true, configurable: true })
}

function required(parse: FieldParser): Field {
  return { parse, optional: false }
}

function optional(parse: FieldParser): Field {
  return { parse, optional: true }
}

/**
 * Parse an object against a shape, emitting shape fields first in shape order.
 * @param value - candidate object.
 * @param path - diagnostic path of the candidate.
 * @param shape - known fields in output order.
 * @param rest - whether unknown fields are dropped or appended in input order.
 * @returns a new object holding the parsed fields.
 */
function parseObject(value: unknown, path: string, shape: Shape, rest: 'strip' | 'keep' = 'strip'): ProtocolObject {
  if (!isObject(value)) fail(path, 'must be an object')
  const output: ProtocolObject = {}
  for (const [key, field] of Object.entries(shape)) {
    const child = Object.hasOwn(value, key) ? value[key] : undefined
    if (child === undefined) {
      if (!field.optional) fail(join(path, key), 'is required')
      continue
    }
    define(output, key, field.parse(child, join(path, key)))
  }
  if (rest === 'keep') {
    for (const [key, child] of Object.entries(value)) {
      if (!Object.hasOwn(shape, key)) define(output, key, child)
    }
  }
  return output
}

function parseArray(value: unknown, path: string, item: FieldParser): unknown[] {
  if (!Array.isArray(value)) fail(path, 'must be an array')
  return value.map((child, index) => item(child, `${path}[${index}]`))
}

function string(value: unknown, path: string): string {
  if (typeof value !== 'string') fail(path, 'must be a string')
  return value
}

function boolean(value: unknown, path: string): boolean {
  if (typeof value !== 'boolean') fail(path, 'must be a boolean')
  return value
}

function number(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) fail(path, 'must be a finite number')
  return value
}

function literal(expected: string): FieldParser {
  return (value, path) => {
    if (value !== expected) fail(path, `must be "${expected}"`)
    return value
  }
}

function oneOf(values: readonly string[]): FieldParser {
  return (value, path) => {
    if (typeof value !== 'string' || !values.includes(value)) {
      fail(path, `must be one of ${values.map(item => `"${item}"`).join(', ')}`)
    }
    return value
  }
}

function stringArray(value: unknown, path: string): unknown[] {
  return parseArray(value, path, string)
}

/** A `_meta` record or JSON Schema keyword map: any object, copied with its keys in input order. */
function record(value: unknown, path: string): ProtocolObject {
  return parseObject(value, path, {}, 'keep')
}

/** JSON keeps every base64 payload as a string; `atob` accepts what the MCP schema accepts. */
function base64(value: unknown, path: string): string {
  const text = string(value, path)
  try {
    atob(text)
  } catch {
    fail(path, 'must be base64')
  }
  return text
}

function dateTime(value: unknown, path: string): string {
  const text = string(value, path)
  const match = DATE_TIME.exec(text)
  if (match === null) fail(path, 'must be an RFC 3339 date-time with an offset')
  const [year, month, day] = match.slice(1, 4).map(Number) as [number, number, number]
  const date = new Date(0)
  date.setUTCFullYear(year, month - 1, day)
  if (date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day) {
    fail(path, 'must be an RFC 3339 date-time with an offset')
  }
  return text
}

function priority(value: unknown, path: string): number {
  const parsed = number(value, path)
  if (parsed < 0 || parsed > 1) fail(path, 'must be between 0 and 1')
  return parsed
}

const ANNOTATIONS: Shape = {
  audience: optional((value, path) => parseArray(value, path, oneOf(['user', 'assistant']))),
  priority: optional(priority),
  lastModified: optional(dateTime),
}

function annotations(value: unknown, path: string): ProtocolObject {
  return parseObject(value, path, ANNOTATIONS)
}

const ICON: Shape = {
  src: required(string),
  mimeType: optional(string),
  sizes: optional(stringArray),
  theme: optional(oneOf(['light', 'dark'])),
}

function icons(value: unknown, path: string): unknown[] {
  return parseArray(value, path, (icon, iconPath) => parseObject(icon, iconPath, ICON))
}

/** Fields shared by `Tool`, `Resource`, and `ResourceTemplate`. */
const METADATA: Shape = {
  name: required(string),
  title: optional(string),
  icons: optional(icons),
}

const RESOURCE: Shape = {
  ...METADATA,
  uri: required(string),
  description: optional(string),
  mimeType: optional(string),
  size: optional(number),
  annotations: optional(annotations),
  _meta: optional(record),
}

const RESOURCE_TEMPLATE: Shape = {
  ...METADATA,
  uriTemplate: required(string),
  description: optional(string),
  mimeType: optional(string),
  annotations: optional(annotations),
  _meta: optional(record),
}

const RESOURCE_CONTENTS: Shape = {
  uri: required(string),
  mimeType: optional(string),
  _meta: optional(record),
}

const TEXT_RESOURCE_CONTENTS: Shape = { ...RESOURCE_CONTENTS, text: required(string) }
const BLOB_RESOURCE_CONTENTS: Shape = { ...RESOURCE_CONTENTS, blob: required(base64) }

/** Text contents win when a server sends both `text` and `blob`, as in the MCP schema union. */
function resourceContents(value: unknown, path: string): ProtocolObject {
  if (isObject(value) && typeof value.text === 'string') return parseObject(value, path, TEXT_RESOURCE_CONTENTS)
  return parseObject(value, path, BLOB_RESOURCE_CONTENTS)
}

const CONTENT_BLOCKS: Readonly<Record<string, Shape>> = {
  text: {
    type: required(literal('text')),
    text: required(string),
    annotations: optional(annotations),
    _meta: optional(record),
  },
  image: {
    type: required(literal('image')),
    data: required(base64),
    mimeType: required(string),
    annotations: optional(annotations),
    _meta: optional(record),
  },
  audio: {
    type: required(literal('audio')),
    data: required(base64),
    mimeType: required(string),
    annotations: optional(annotations),
    _meta: optional(record),
  },
  resource_link: {
    ...RESOURCE,
    type: required(literal('resource_link')),
  },
  resource: {
    type: required(literal('resource')),
    resource: required(resourceContents),
    annotations: optional(annotations),
    _meta: optional(record),
  },
}

function contentBlock(value: unknown, path: string): ProtocolObject {
  if (!isObject(value)) fail(path, 'must be an object')
  const shape = typeof value.type === 'string' && Object.hasOwn(CONTENT_BLOCKS, value.type)
    ? CONTENT_BLOCKS[value.type]
    : undefined
  if (shape === undefined) fail(join(path, 'type'), `must be one of ${Object.keys(CONTENT_BLOCKS).map(type => `"${type}"`).join(', ')}`)
  return parseObject(value, path, shape)
}

/** A JSON Schema object schema: `type: "object"` plus any other keywords. */
const OBJECT_SCHEMA: Shape = {
  type: required(literal('object')),
  properties: optional(record),
  required: optional(stringArray),
}

function objectSchema(value: unknown, path: string): ProtocolObject {
  return parseObject(value, path, OBJECT_SCHEMA, 'keep')
}

const TOOL: Shape = {
  ...METADATA,
  description: optional(string),
  inputSchema: required(objectSchema),
  outputSchema: optional(objectSchema),
  annotations: optional((value, path) => parseObject(value, path, {
    title: optional(string),
    readOnlyHint: optional(boolean),
    destructiveHint: optional(boolean),
    idempotentHint: optional(boolean),
    openWorldHint: optional(boolean),
  })),
  execution: optional((value, path) => parseObject(value, path, {
    taskSupport: optional(oneOf(['required', 'optional', 'forbidden'])),
  })),
  _meta: optional(record),
}

/**
 * Parse one `tools/list` entry.
 * @param value - one tool from the server's list.
 * @param path - diagnostic path, such as `tools[3]`.
 * @returns the tool in schema form.
 */
export function parseListedTool(value: unknown, path: string): ListedTool {
  return parseObject(value, path, TOOL) as unknown as ListedTool
}

const CALL_TOOL_RESULT: Shape = {
  _meta: optional(record),
  content: optional((value, path) => parseArray(value, path, contentBlock)),
  structuredContent: optional(value => value),
  isError: optional(boolean),
}

/**
 * Parse one `tools/call` result. Fields the bridge does not read are dropped.
 * @param value - the server's result.
 * @returns content blocks in schema form plus the structured and error fields.
 */
export function parseCallToolResult(value: unknown): ParsedCallToolResult {
  const parsed = parseObject(value, '', CALL_TOOL_RESULT)
  return {
    content: (parsed.content ?? []) as JsonValue[],
    ...parsed.structuredContent === undefined ? {} : { structuredContent: parsed.structuredContent as JsonValue },
    ...parsed.isError === undefined ? {} : { isError: parsed.isError as boolean },
  }
}

/**
 * Build a `resources/list` result from one page or the aggregated pages.
 * @param resources - resource entries from pi-mcp.
 * @param nextCursor - continuation cursor of a single page.
 * @returns the result in schema form.
 */
export function resourceListResult(resources: readonly unknown[], nextCursor?: string): JsonValue {
  return {
    ...nextCursor === undefined ? {} : { nextCursor },
    resources: parseArray(resources, 'resources', (value, path) => parseObject(value, path, RESOURCE)),
  } as JsonValue
}

/**
 * Build a `resources/templates/list` result from one page or the aggregated pages.
 * @param templates - resource template entries from pi-mcp.
 * @param nextCursor - continuation cursor of a single page.
 * @returns the result in schema form.
 */
export function resourceTemplateListResult(templates: readonly unknown[], nextCursor?: string): JsonValue {
  return {
    ...nextCursor === undefined ? {} : { nextCursor },
    resourceTemplates: parseArray(
      templates,
      'resourceTemplates',
      (value, path) => parseObject(value, path, RESOURCE_TEMPLATE),
    ),
  } as JsonValue
}

const READ_RESOURCE_RESULT: Shape = {
  _meta: optional(record),
  contents: required((value, path) => parseArray(value, path, resourceContents)),
}

/**
 * Parse one `resources/read` result, keeping unknown top-level fields.
 * @param value - the server's result.
 * @returns the result in schema form.
 */
export function parseReadResourceResult(value: unknown): JsonValue {
  return parseObject(value, '', READ_RESOURCE_RESULT, 'keep') as JsonValue
}
