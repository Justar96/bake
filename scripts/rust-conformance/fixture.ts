/**
 * Strict parsing and validation for the synthetic conformance documents: the
 * runner input, the runner observation, and the shared fixture that pairs an
 * input with hand-authored expected values. Both the Bun runner and the driver
 * use these checks, so one rule set decides what each side accepts.
 */

export const INPUT_SCHEMA = 'bake/synthetic-conformance/input'
export const OBSERVATION_SCHEMA = 'bake/synthetic-conformance/observation'
export const FIXTURE_SCHEMA = 'bake/synthetic-conformance/fixture'

/** Largest stdin document or fixture file, in bytes. */
export const MAX_DOCUMENT_BYTES = 256 * 1024
/** Most prompts, events, permissions, writes, or files in one vector. */
export const MAX_ITEMS = 64
/** Largest string or file, in UTF-8 bytes. */
export const MAX_TEXT_BYTES = 64 * 1024
/** Longest relative path, in UTF-8 bytes. */
export const MAX_PATH_BYTES = 200
/** Deepest event nesting, counting the event object itself as one container. */
export const MAX_EVENT_DEPTH = 32

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }
export type SyntheticEvent = { [key: string]: JsonValue }
export type Decision = 'allow' | 'deny'
export interface Permission { id: string; path: string; decision: Decision }
export interface Write { path: string; text: string; permission: string }
export interface FileEntry { path: string; hex: string }

export interface SyntheticInput {
  schema: typeof INPUT_SCHEMA
  version: 1
  prompts: string[]
  events: SyntheticEvent[]
  permissions: Permission[]
  writes: Write[]
}

export interface Observation {
  schema: typeof OBSERVATION_SCHEMA
  version: 1
  prompts: string[]
  events: SyntheticEvent[]
  permissions: Permission[]
}

export interface Fixture {
  schema: typeof FIXTURE_SCHEMA
  version: 1
  id: string
  input: SyntheticInput
  initialFiles: FileEntry[]
  protectedFiles: string[]
  expected: {
    prompts: string[]
    events: SyntheticEvent[]
    permissions: Permission[]
    files: FileEntry[]
  }
}

/** A document that breaks the contract; the runner reports it with exit 2. */
export class ConformanceInputError extends Error {
  override name = 'ConformanceInputError'
}

const fail = (where: string, message: string): never => {
  throw new ConformanceInputError(`${where}: ${message}`)
}

const utf8Bytes = (text: string): number => Buffer.byteLength(text, 'utf8')

const INTEGER_TOKEN = /^-?(?:0|[1-9][0-9]*)$/u

/**
 * Parse one JSON document under the contract's text rules: valid UTF-8 with no
 * byte order mark, well-formed Unicode strings and keys, no duplicate object
 * keys, and only plain integer tokens within the safe range. `-0`, fractions,
 * and exponents are rejected rather than rounded, so no value is normalized.
 * @param bytes - the raw document.
 * @param where - the label used in diagnostics.
 */
export function parseStrictJson(bytes: Uint8Array, where: string): JsonValue {
  if (bytes.byteLength > MAX_DOCUMENT_BYTES) fail(where, `exceeds ${MAX_DOCUMENT_BYTES} bytes`)
  let text: string
  try {
    text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes)
  } catch {
    return fail(where, 'is not valid UTF-8')
  }
  let value: JsonValue
  try {
    value = JSON.parse(text, function (key: string, parsed: unknown, context?: { source?: string }) {
      if (!key.isWellFormed()) fail(where, 'contains an unpaired surrogate in a key')
      if (typeof parsed === 'string' && !parsed.isWellFormed()) fail(where, 'contains an unpaired surrogate')
      if (typeof parsed === 'number') {
        const source = context?.source
        if (source === undefined) return fail(where, 'number source text is unavailable on this runtime')
        if (!INTEGER_TOKEN.test(source) || source === '-0' || !Number.isSafeInteger(parsed)) {
          fail(where, 'has a number that is not a plain safe integer')
        }
      }
      return parsed
    }) as JsonValue
  } catch (error) {
    if (error instanceof ConformanceInputError) throw error
    return fail(where, 'is not valid JSON')
  }
  rejectDuplicateKeys(text, where)
  return value
}

/** Scan already-valid JSON text for an object that repeats a key. */
function rejectDuplicateKeys(text: string, where: string): void {
  const stack: (Set<string> | null)[] = []
  for (let index = 0; index < text.length; index++) {
    const char = text[index]
    if (char === '{') stack.push(new Set())
    else if (char === '[') stack.push(null)
    else if (char === '}' || char === ']') stack.pop()
    else if (char === '"') {
      const start = index
      for (index++; text[index] !== '"'; index++) if (text[index] === '\\') index++
      let next = index + 1
      while (/\s/u.test(text[next] ?? '')) next++
      const keys = stack.at(-1)
      if (text[next] === ':' && keys) {
        const key = JSON.parse(text.slice(start, index + 1)) as string
        if (keys.has(key)) fail(where, 'repeats an object key')
        keys.add(key)
      }
    }
  }
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

function exactRecord(value: unknown, keys: readonly string[], where: string): Record<string, unknown> {
  if (!isRecord(value)) return fail(where, 'must be an object')
  for (const key of Object.keys(value)) if (!keys.includes(key)) fail(where, `has unknown field ${JSON.stringify(key)}`)
  for (const key of keys) if (!(key in value)) fail(where, `is missing field ${JSON.stringify(key)}`)
  return value
}

function text(value: unknown, where: string): string {
  if (typeof value !== 'string') return fail(where, 'must be a string')
  if (utf8Bytes(value) > MAX_TEXT_BYTES) fail(where, `exceeds ${MAX_TEXT_BYTES} UTF-8 bytes`)
  return value
}

function vector<T>(value: unknown, where: string, item: (entry: unknown, where: string) => T): T[] {
  if (!Array.isArray(value)) return fail(where, 'must be an array')
  if (value.length > MAX_ITEMS) fail(where, `has more than ${MAX_ITEMS} entries`)
  return value.map((entry, index) => item(entry, `${where}[${index}]`))
}

function header(record: Record<string, unknown>, schema: string, where: string): void {
  if (record.schema !== schema) fail(`${where}.schema`, `must be ${JSON.stringify(schema)}`)
  if (record.version !== 1) fail(`${where}.version`, 'must be 1')
}

// A device stem stays reserved with spaces or an extension after it.
const RESERVED = /^(?:con|prn|aux|nul|com[0-9¹²³]|lpt[0-9¹²³]|conin\$|conout\$) *(?:\..*)?$/iu

/**
 * Check a workspace-relative POSIX path. Rejects anything that could escape the
 * workspace or that one supported host would read differently from another.
 * @param value - the candidate path.
 * @param where - the label used in diagnostics.
 */
export function safePath(value: unknown, where: string): string {
  if (typeof value !== 'string') return fail(where, 'must be a string')
  if (value.length === 0 || utf8Bytes(value) > MAX_PATH_BYTES) fail(where, `must be 1 to ${MAX_PATH_BYTES} UTF-8 bytes`)
  if (/[\u0000-\u001f\u007f\\:<>"|?*]/u.test(value)) fail(where, 'contains a control character or a character Windows forbids')
  for (const segment of value.split('/')) {
    if (segment === '' || segment === '.' || segment === '..') fail(where, 'has an empty, dot, or dot-dot segment')
    if (/[. ]$/u.test(segment)) fail(where, 'has a segment ending in a dot or space')
    if (RESERVED.test(segment)) fail(where, 'names a Windows reserved device')
  }
  return value
}

function event(value: unknown, where: string): SyntheticEvent {
  if (!isRecord(value)) return fail(where, 'must be an object')
  const visit = (entry: unknown, at: string, depth: number): void => {
    if (typeof entry === 'string') text(entry, at)
    if (typeof entry !== 'object' || entry === null) return
    if (depth > MAX_EVENT_DEPTH) fail(where, `nests deeper than ${MAX_EVENT_DEPTH} containers`)
    if (Array.isArray(entry)) entry.forEach((item, index) => visit(item, `${at}[${index}]`, depth + 1))
    else for (const [key, item] of Object.entries(entry)) visit(item, `${at}.${text(key, `${at} key`)}`, depth + 1)
  }
  visit(value, where, 1)
  return value as SyntheticEvent
}

function permission(value: unknown, where: string): Permission {
  const record = exactRecord(value, ['id', 'path', 'decision'], where)
  const id = text(record.id, `${where}.id`)
  if (id.length === 0) fail(`${where}.id`, 'must not be empty')
  if (record.decision !== 'allow' && record.decision !== 'deny') fail(`${where}.decision`, 'must be "allow" or "deny"')
  return { id, path: safePath(record.path, `${where}.path`), decision: record.decision as Decision }
}

function permissions(value: unknown, where: string): Permission[] {
  const list = vector(value, where, permission)
  const ids = new Set<string>()
  for (const entry of list) {
    if (ids.has(entry.id)) fail(where, `repeats permission id ${JSON.stringify(entry.id)}`)
    ids.add(entry.id)
  }
  return list
}

const prompts = (value: unknown, where: string): string[] => vector(value, where, text)
const events = (value: unknown, where: string): SyntheticEvent[] => vector(value, where, event)

/**
 * Validate a parsed runner input. Every write must reference a known
 * permission whose path matches its own.
 * @param value - the parsed document.
 * @param where - the label used in diagnostics.
 */
export function validateInput(value: unknown, where = 'input'): SyntheticInput {
  const record = exactRecord(value, ['schema', 'version', 'prompts', 'events', 'permissions', 'writes'], where)
  header(record, INPUT_SCHEMA, where)
  const granted = permissions(record.permissions, `${where}.permissions`)
  const writes = vector(record.writes, `${where}.writes`, (entry, at): Write => {
    const write = exactRecord(entry, ['path', 'text', 'permission'], at)
    const result = { path: safePath(write.path, `${at}.path`), text: text(write.text, `${at}.text`), permission: text(write.permission, `${at}.permission`) }
    const match = granted.find(candidate => candidate.id === result.permission)
    if (!match) return fail(`${at}.permission`, 'references an unknown permission')
    if (match.path !== result.path) fail(`${at}.path`, 'differs from its permission path')
    return result
  })
  return {
    schema: INPUT_SCHEMA, version: 1,
    prompts: prompts(record.prompts, `${where}.prompts`),
    events: events(record.events, `${where}.events`),
    permissions: granted, writes,
  }
}

/**
 * Validate a parsed runner observation.
 * @param value - the parsed document.
 * @param where - the label used in diagnostics.
 */
export function validateObservation(value: unknown, where = 'observation'): Observation {
  const record = exactRecord(value, ['schema', 'version', 'prompts', 'events', 'permissions'], where)
  header(record, OBSERVATION_SCHEMA, where)
  return {
    schema: OBSERVATION_SCHEMA, version: 1,
    prompts: prompts(record.prompts, `${where}.prompts`),
    events: events(record.events, `${where}.events`),
    permissions: permissions(record.permissions, `${where}.permissions`),
  }
}

const foldCase = (path: string): string => path.toUpperCase().toLowerCase()

function files(value: unknown, where: string): FileEntry[] {
  const list = vector(value, where, (entry, at): FileEntry => {
    const record = exactRecord(entry, ['path', 'hex'], at)
    // The text bound applies to the encoded bytes, not to the hex characters.
    const hex = record.hex
    if (typeof hex !== 'string' || !/^(?:[0-9a-f]{2})*$/u.test(hex)) return fail(`${at}.hex`, 'must be lowercase hex bytes')
    if (hex.length / 2 > MAX_TEXT_BYTES) fail(`${at}.hex`, `exceeds ${MAX_TEXT_BYTES} bytes`)
    return { path: safePath(record.path, `${at}.path`), hex }
  })
  // A file may not share a case-folded name with another or sit where another needs a directory.
  const folded = list.map(entry => foldCase(entry.path))
  for (const [index, entry] of list.entries()) {
    const path = folded[index]
    if (folded.indexOf(path ?? '') !== index) fail(where, `repeats path ${JSON.stringify(entry.path)} ignoring case`)
    if (folded.some(other => other.startsWith(`${path}/`))) fail(where, `uses file ${JSON.stringify(entry.path)} as a directory`)
  }
  return list
}

/**
 * Validate a parsed shared fixture.
 * @param value - the parsed document.
 * @param where - the label used in diagnostics.
 */
export function validateFixture(value: unknown, where = 'fixture'): Fixture {
  const record = exactRecord(value, ['schema', 'version', 'id', 'input', 'initialFiles', 'protectedFiles', 'expected'], where)
  header(record, FIXTURE_SCHEMA, where)
  const id = text(record.id, `${where}.id`)
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/u.test(id)) fail(`${where}.id`, 'must be lowercase letters, digits, and hyphens')
  const initialFiles = files(record.initialFiles, `${where}.initialFiles`)
  const protectedFiles = vector(record.protectedFiles, `${where}.protectedFiles`, (entry, at) => {
    const path = safePath(entry, at)
    if (!initialFiles.some(file => file.path === path)) fail(at, 'is not an initial file')
    return path
  })
  if (new Set(protectedFiles).size !== protectedFiles.length) fail(`${where}.protectedFiles`, 'repeats a path')
  const expected = exactRecord(record.expected, ['prompts', 'events', 'permissions', 'files'], `${where}.expected`)
  return {
    schema: FIXTURE_SCHEMA, version: 1, id,
    input: validateInput(record.input, `${where}.input`),
    initialFiles, protectedFiles,
    expected: {
      prompts: prompts(expected.prompts, `${where}.expected.prompts`),
      events: events(expected.events, `${where}.expected.events`),
      permissions: permissions(expected.permissions, `${where}.expected.permissions`),
      files: files(expected.files, `${where}.expected.files`),
    },
  }
}
