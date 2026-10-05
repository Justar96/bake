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
 * Parsing is the maintained `jsonc-parser` package, VS Code's own reader:
 * `parseTree` for the offset-keeping nodes and `getNodeValue` for their
 * values, its error list mapped back to the `SyntaxError` the module has
 * always failed with. The splice helpers stay local, because they place
 * pre-rendered text — the comment above a new entry included — where the
 * package's `modify` can only place a JSON value.
 *
 * The comment and trailing-comma rules are VS Code's "JSON with Comments"
 * mode: https://code.visualstudio.com/docs/languages/json#_json-with-comments
 *
 * @module bake-tui-app/jsonc
 */
// The package's bare specifier resolves to its UMD main, whose lazy
// `require('./impl/format')` cannot resolve from the bundled runner; the ESM
// build bundles statically.
import { getNodeValue, parseTree, stripComments, ParseErrorCode, type Node, type ParseError } from 'jsonc-parser/lib/esm/main.js'

/** A parsed value, with the source span `[offset, offset + length)` it came from. */
export type JsonNode = Node

/** How each of the package's error codes reads, in the wording callers saw before it. */
const UNREADABLE: Readonly<Record<ParseErrorCode, string>> = {
  [ParseErrorCode.InvalidSymbol]: 'unexpected character',
  [ParseErrorCode.InvalidNumberFormat]: 'unexpected character',
  [ParseErrorCode.PropertyNameExpected]: 'expected a key',
  [ParseErrorCode.ValueExpected]: 'unexpected character',
  [ParseErrorCode.ColonExpected]: 'expected :',
  [ParseErrorCode.CommaExpected]: 'expected ,',
  [ParseErrorCode.CloseBraceExpected]: 'expected , or }',
  [ParseErrorCode.CloseBracketExpected]: 'expected , or ]',
  [ParseErrorCode.EndOfFileExpected]: 'unexpected content after the document',
  [ParseErrorCode.InvalidCommentToken]: 'unexpected character',
  [ParseErrorCode.UnexpectedEndOfComment]: 'unterminated comment',
  [ParseErrorCode.UnexpectedEndOfString]: 'unterminated string',
  [ParseErrorCode.UnexpectedEndOfNumber]: 'unexpected character',
  [ParseErrorCode.InvalidUnicode]: 'invalid string',
  [ParseErrorCode.InvalidEscapeCharacter]: 'invalid string',
  [ParseErrorCode.InvalidCharacter]: 'invalid string',
}

/**
 * Parse one JSONC document.
 * @param text - the whole file. A leading byte-order mark is skipped.
 * @returns the root node, or undefined when the document holds only whitespace and comments.
 * @throws SyntaxError at the first offset that is not JSONC.
 */
export function parseJsonc(text: string): JsonNode | undefined {
  if (/^\s*$/u.test(stripComments(text))) return undefined
  const errors: ParseError[] = []
  const root = parseTree(text, errors, { allowTrailingComma: true })
  // The package has no byte-order-mark concept: its scanner reads a leading
  // one as a single invalid symbol. The module's contract skips it, and every
  // other report is one the package repaired around but a JSONC file may not
  // hold — the reader this replaced failed on the first of them.
  const first = (text.charCodeAt(0) === 0xfeff
    ? errors.filter(error => error.error !== ParseErrorCode.InvalidSymbol || error.offset !== 0 || error.length !== 1)
    : errors)[0]
  if (first === undefined && root !== undefined) return root
  throw new SyntaxError(
    `JSONC: ${first === undefined ? UNREADABLE[ParseErrorCode.ValueExpected] : UNREADABLE[first.error]} at offset ${first?.offset ?? 0}`,
  )
}

/**
 * @param node - a parsed node.
 * @returns the plain JavaScript value it denotes.
 */
export function valueOf(node: JsonNode): unknown {
  return getNodeValue(node)
}

/**
 * @param object - an object node.
 * @param key - a member name.
 * @returns the last member of that name, which is the one a JSON reader keeps.
 *
 * The package's own `findNodeAtLocation` stops at the first member of the
 * name, so the lookup walks the members itself.
 */
export function member(object: JsonNode, key: string): JsonNode | undefined {
  return object.type === 'object'
    ? object.children?.findLast(entry => entry.type === 'property' && entry.children?.[0]?.value === key)?.children?.[1]
    : undefined
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
export function appendItem(text: string, array: JsonNode, render: (indent: string) => string, layout: Layout): string {
  const last = array.children?.at(-1)
  return insertBefore(text, array.offset, array.offset + array.length - 1, last?.offset,
    last === undefined ? undefined : last.offset + last.length, render, layout)
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
export function appendMember(text: string, object: JsonNode, key: string, render: (indent: string) => string, layout: Layout): string {
  const last = object.children?.at(-1)
  const lastKey = last?.children?.[0]
  const lastValue = last?.children?.[1]
  return insertBefore(text, object.offset, object.offset + object.length - 1, lastKey?.offset,
    lastValue === undefined ? undefined : lastValue.offset + lastValue.length,
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
