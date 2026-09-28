/**
 * JSON with comments, read and extended without reformatting it.
 *
 * VS Code's `keybindings.json` and Windows Terminal's `settings.json` are
 * JSONC: `//` and `/* *\/` comments and trailing commas are allowed. A
 * parse-and-stringify round trip would drop the user's comments and layout,
 * so this module parses to nodes that keep their source offsets, and edits
 * splice new text in at those offsets. Everything outside the insertion
 * stays byte for byte.
 *
 * The comment and trailing-comma rules are VS Code's "JSON with Comments"
 * mode: https://code.visualstudio.com/docs/languages/json#_json-with-comments
 *
 * @module @dsh-tui/app/jsonc
 */

/** A parsed value with the source span `[start, end)` it came from. */
export type JsonNode = JsonObject | JsonArray | JsonScalar

/** An object and its members in source order. */
export interface JsonObject {
  readonly kind: 'object'
  readonly start: number
  readonly end: number
  readonly members: readonly JsonMember[]
}
/** One `"key": value` pair. */
export interface JsonMember { readonly key: string; readonly keyStart: number; readonly value: JsonNode }
/** An array and its items in source order. */
export interface JsonArray {
  readonly kind: 'array'
  readonly start: number
  readonly end: number
  readonly items: readonly JsonNode[]
}
/** A string, number, boolean, or null. */
export interface JsonScalar {
  readonly kind: 'scalar'
  readonly start: number
  readonly end: number
  readonly value: string | number | boolean | null
}

/**
 * Parse one JSONC document.
 * @param text - the whole file. A leading byte-order mark is skipped.
 * @returns the root node, or undefined when the document holds only whitespace and comments.
 * @throws SyntaxError at the first offset that is not JSONC.
 */
export function parseJsonc(text: string): JsonNode | undefined {
  let at = 0
  const fail = (what: string): never => { throw new SyntaxError(`JSONC: ${what} at offset ${at}`) }
  const skip = (): void => {
    for (;;) {
      const char = text[at]
      if (char === ' ' || char === '\t' || char === '\n' || char === '\r' || char === '\uFEFF') at += 1
      else if (text.startsWith('//', at)) { const end = text.indexOf('\n', at); at = end < 0 ? text.length : end }
      else if (text.startsWith('/*', at)) {
        const end = text.indexOf('*/', at + 2)
        if (end < 0) fail('unterminated comment')
        at = end + 2
      }
      else return
    }
  }
  const string = (): string => {
    const start = at
    at += 1
    for (;;) {
      const char = text[at]
      if (char === undefined || char === '\n') fail('unterminated string')
      if (char === '\\') at += 2
      else if (char === '"') { at += 1; break } else at += 1
    }
    try { return JSON.parse(text.slice(start, at)) as string } catch { return fail('invalid string') }
  }
  const value = (): JsonNode => {
    skip()
    const start = at
    const char = text[at]
    if (char === '{') {
      at += 1
      const members: JsonMember[] = []
      for (;;) {
        skip()
        if (text[at] === '}') { at += 1; return { kind: 'object', start, end: at, members } }
        if (members.length > 0) {
          if (text[at] !== ',') fail('expected , or }')
          at += 1
          skip()
          // A trailing comma before the closing brace.
          if (text[at] === '}') { at += 1; return { kind: 'object', start, end: at, members } }
        }
        if (text[at] !== '"') fail('expected a key')
        const keyStart = at
        const key = string()
        skip()
        if (text[at] !== ':') fail('expected :')
        at += 1
        members.push({ key, keyStart, value: value() })
      }
    }
    if (char === '[') {
      at += 1
      const items: JsonNode[] = []
      for (;;) {
        skip()
        if (text[at] === ']') { at += 1; return { kind: 'array', start, end: at, items } }
        if (items.length > 0) {
          if (text[at] !== ',') fail('expected , or ]')
          at += 1
          skip()
          if (text[at] === ']') { at += 1; return { kind: 'array', start, end: at, items } }
        }
        items.push(value())
      }
    }
    if (char === '"') {
      const decoded = string()
      return { kind: 'scalar', start, end: at, value: decoded }
    }
    const literal = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/u.exec(text.slice(at, at + 64))?.[0]
    if (literal === undefined) return fail('unexpected character')
    at += literal.length
    return { kind: 'scalar', start, end: at, value: JSON.parse(literal) as number | boolean | null }
  }
  skip()
  if (at >= text.length) return undefined
  const root = value()
  skip()
  if (at < text.length) fail('unexpected content after the document')
  return root
}

/**
 * @param node - a parsed node.
 * @returns the plain JavaScript value it denotes.
 */
export function valueOf(node: JsonNode): unknown {
  switch (node.kind) {
    case 'scalar': return node.value
    case 'array': return node.items.map(valueOf)
    case 'object': return Object.fromEntries(node.members.map(member => [member.key, valueOf(member.value)]))
  }
}

/**
 * @param object - an object node.
 * @param key - a member name.
 * @returns the last member of that name, which is the one a JSON reader keeps.
 */
export function member(object: JsonObject, key: string): JsonNode | undefined {
  return object.members.findLast(entry => entry.key === key)?.value
}

/** How new text is laid out to match the file around it. */
export interface Layout {
  /** One indentation step: the file's own, or four spaces, VS Code's default. */
  readonly unit: string
  /** `\r\n` when the file uses it, otherwise `\n`. */
  readonly eol: string
}

/**
 * @param text - an existing file, or the empty string.
 * @returns the indentation step and line ending the file already uses.
 */
export function layoutOf(text: string): Layout {
  const indented = /^([ \t]+)\S/mu.exec(text)?.[1]
  return { unit: indented ?? '    ', eol: text.includes('\r\n') ? '\r\n' : '\n' }
}

/**
 * Append one item to an array, after any comments already at its end.
 * @param text - the file.
 * @param array - an array node parsed from `text`.
 * @param render - the item's text for a given indentation; lines after the first carry it themselves.
 * @param layout - the file's indentation and line ending.
 * @returns the file with the item added and every other byte unchanged.
 */
export function appendItem(text: string, array: JsonArray, render: (indent: string) => string, layout: Layout): string {
  const last = array.items.at(-1)
  return insertBefore(text, array.start, array.end - 1, last?.start, last?.end, render, layout)
}

/**
 * Add one member at the end of an object.
 * @param text - the file.
 * @param object - an object node parsed from `text`.
 * @param key - the new member's name; the caller has checked it is absent.
 * @param render - the member value's text for the member's indentation.
 * @param layout - the file's indentation and line ending.
 * @returns the file with the member added and every other byte unchanged.
 */
export function appendMember(text: string, object: JsonObject, key: string, render: (indent: string) => string, layout: Layout): string {
  const last = object.members.at(-1)
  return insertBefore(text, object.start, object.end - 1, last?.keyStart, last?.value.end,
    indent => `${JSON.stringify(key)}: ${render(indent)}`, layout)
}

/**
 * Insert an entry before a container's closing bracket, separating it from
 * the last entry with a comma unless the file already has a trailing one.
 */
function insertBefore(text: string, open: number, close: number, lastStart: number | undefined, lastEnd: number | undefined,
  render: (indent: string) => string, layout: Layout): string {
  const outer = indentAt(text, open)
  // An entry that starts its own line sets the indentation; otherwise one step in from the bracket.
  const indent = lastStart !== undefined && /^[ \t]*$/u.test(text.slice(lineStart(text, lastStart), lastStart))
    ? indentAt(text, lastStart) : outer + layout.unit
  const entry = render(indent)
  const closeLine = lineStart(text, close)
  // The closing bracket on a line of its own: the entry takes a new line above it.
  const own = closeLine > open && /^[ \t]*$/u.test(text.slice(closeLine, close))
  const inserted = own ? `${indent}${entry}${layout.eol}` : `${layout.eol}${indent}${entry}${layout.eol}${outer}`
  const at = own ? closeLine : close
  if (lastEnd === undefined || trailingComma(text, lastEnd, close)) return text.slice(0, at) + inserted + text.slice(at)
  return `${text.slice(0, lastEnd)},${text.slice(lastEnd, at)}${inserted}${text.slice(at)}`
}

/** Whether a comma, outside comments, follows the last entry before the bracket. */
function trailingComma(text: string, from: number, close: number): boolean {
  let at = from
  while (at < close) {
    if (text[at] === ',') return true
    // The document parsed, so every comment before the bracket is terminated.
    const end = text.startsWith('//', at) ? text.indexOf('\n', at) : text.startsWith('/*', at) ? text.indexOf('*/', at + 2) + 1 : at
    if (end < at) return false
    at = end + 1
  }
  return false
}

function lineStart(text: string, at: number): number { return text.lastIndexOf('\n', at - 1) + 1 }

function indentAt(text: string, at: number): string {
  return /^[ \t]*/u.exec(text.slice(lineStart(text, at)))![0]
}

/**
 * Lay a value out as JSON on one line, with a space inside braces and after
 * separators, as settings files are usually written by hand.
 * @param value - plain JSON data.
 * @returns the text; control characters are escaped the way `JSON.stringify` escapes them.
 */
export function inlineJson(value: unknown): string {
  if (Array.isArray(value)) return value.length === 0 ? '[]' : `[ ${value.map(inlineJson).join(', ')} ]`
  if (value !== null && typeof value === 'object') {
    const entries = Object.entries(value)
    return entries.length === 0 ? '{}' : `{ ${entries.map(([key, entry]) => `${JSON.stringify(key)}: ${inlineJson(entry)}`).join(', ')} }`
  }
  return JSON.stringify(value)
}

/**
 * Lay a value out as indented JSON whose first line starts at `indent`.
 * @param value - plain JSON data.
 * @param indent - the indentation of the line the value starts on.
 * @param layout - the indentation step and line ending.
 * @returns the text, without the first line's indentation.
 */
export function blockJson(value: unknown, indent: string, layout: Layout): string {
  return JSON.stringify(value, null, layout.unit).split('\n').join(`${layout.eol}${indent}`)
}
