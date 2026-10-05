/**
 * The editable fields of a registered settings namespace, read from the
 * serialized schema its owner registered, so `/settings` can offer every
 * plugin's settings without knowing the plugin.
 * @module bake-tui-app/schema-fields
 */

/** How the panel edits a field. */
export type FieldKind =
  /** On or off. */
  | 'boolean'
  /** A number, within the schema's bounds. */
  | 'number'
  /** Free text. */
  | 'string'
  /** One of the schema's constants. */
  | 'choice'
  /** Text the panel never shows, such as an API key. */
  | 'secret'
  /** A list, a map, or a union the panel cannot edit; the settings file can. */
  | 'other'

/** One field of a namespace, flattened from nested objects to its path. */
export interface SchemaField {
  readonly path: readonly string[]
  readonly kind: FieldKind
  /** The schema's description of the field, when it has one. */
  readonly description?: string
  /** The value the field takes without the user's, from the schema; the composition may still set another. */
  readonly default?: unknown
  readonly min?: number
  readonly max?: number
  readonly step?: number
  /** The constants a `choice` offers, labelled by their descriptions. */
  readonly choices?: readonly { readonly value: string, readonly label: string }[]
  /** The resolved value in force. */
  readonly value: unknown
  /** Whether the user's section sets the field, so a reset has something to remove. */
  readonly overridden: boolean
}

/** One node of a serialized schemastery schema (`schema.toJSON()`). */
interface SchemaNode {
  readonly type?: string
  readonly value?: unknown
  readonly list?: readonly unknown[]
  readonly dict?: Readonly<Record<string, unknown>>
  readonly meta?: {
    readonly description?: unknown
    readonly default?: unknown
    readonly role?: unknown
    readonly min?: unknown
    readonly max?: unknown
    readonly step?: unknown
    readonly hidden?: unknown
  }
}

/** A plain object's own field, or undefined. */
function at(value: unknown, key: string): unknown {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[key] : undefined
}

/** Whether a plain object has the field at all. */
function has(value: unknown, key: string): boolean {
  return typeof value === 'object' && value !== null && !Array.isArray(value) && Object.hasOwn(value, key)
}

/**
 * The fields of a namespace, in schema order, nested objects flattened.
 * @param schema - the descriptor's `schema`, as `toJSON()` wrote it.
 * @param value - the descriptor's resolved value.
 * @param user - the descriptor's raw user section, if any.
 * @returns every field; empty when the schema is not an object.
 */
export function schemaFields(schema: unknown, value: unknown, user: unknown): readonly SchemaField[] {
  const refs = (schema as { refs?: Readonly<Record<string, SchemaNode>> } | undefined)?.refs
  const uid = (schema as { uid?: unknown } | undefined)?.uid
  if (refs === undefined) return []
  const node = (ref: unknown): SchemaNode | undefined => typeof ref === 'number' ? refs[String(ref)] : ref as SchemaNode | undefined
  const fields: SchemaField[] = []
  const walk = (object: SchemaNode, path: readonly string[], resolved: unknown, stored: unknown): void => {
    for (const [key, ref] of Object.entries(object.dict ?? {})) {
      const field = node(ref)
      if (field === undefined || field.meta?.hidden === true) continue
      const here = [...path, key]
      if (field.type === 'object' && field.dict !== undefined) {
        walk(field, here, at(resolved, key), at(stored, key))
        continue
      }
      const meta = field.meta ?? {}
      const description = typeof meta.description === 'string' && meta.description !== '' ? meta.description : undefined
      const number = (bound: unknown): number | undefined => typeof bound === 'number' && Number.isFinite(bound) ? bound : undefined
      const choices = field.type === 'union' ? constants(field.list ?? [], node) : undefined
      const kind: FieldKind = field.type === 'boolean' ? 'boolean'
        : field.type === 'number' ? 'number'
          : field.type === 'string' ? meta.role === 'secret' ? 'secret' : 'string'
            : choices !== undefined ? 'choice' : 'other'
      fields.push({
        path: here, kind, value: at(resolved, key), overridden: has(stored, key),
        ...description === undefined ? {} : { description },
        ...meta.default === undefined ? {} : { default: meta.default },
        ...number(meta.min) === undefined ? {} : { min: number(meta.min)! },
        ...number(meta.max) === undefined ? {} : { max: number(meta.max)! },
        ...number(meta.step) === undefined ? {} : { step: number(meta.step)! },
        ...choices === undefined ? {} : { choices },
      })
    }
  }
  const root = node(uid)
  if (root?.type !== 'object') return []
  walk(root, [], value, user)
  return fields
}

/**
 * A union's choices, when every member is a string constant.
 * @returns the choices, labelled by their descriptions, or undefined for any other union.
 */
function constants(list: readonly unknown[], node: (ref: unknown) => SchemaNode | undefined): readonly { value: string, label: string }[] | undefined {
  const choices: { value: string, label: string }[] = []
  for (const item of list) {
    const choice = node(item)
    if (choice?.type !== 'const' || typeof choice.value !== 'string') return undefined
    const description = choice.meta?.description
    choices.push({ value: choice.value, label: typeof description === 'string' && description !== '' ? description : choice.value })
  }
  return choices.length === 0 ? undefined : choices
}

/**
 * Read a typed number within a field's bounds.
 * @param text - what the user typed.
 * @param field - the bounds and step.
 * @param unit - `ms` accepts `500ms`, `30s`, `2m`, `1h`; `bytes` accepts `512`, `64KB`, `1MB`; `tokens` accepts `8192`, `8k`, `1m`; a plain number is in the unit.
 * @returns the number, or a reason it is not one the field takes.
 */
export function parseNumber(text: string, field: Pick<SchemaField, 'min' | 'max' | 'step'>, unit?: 'ms' | 'bytes' | 'tokens'):
  { readonly value: number } | { readonly problem: 'number' | 'min' | 'max' | 'step' } {
  const match = /^\s*(-?\d+(?:\.\d+)?)\s*([a-z]*)\s*$/iu.exec(text)
  if (match === null) return { problem: 'number' }
  const scale = unit === undefined ? { '': 1 }
    : unit === 'ms' ? { '': 1, ms: 1, s: 1_000, m: 60_000, h: 3_600_000 }
      : unit === 'tokens' ? { '': 1, k: 1_000, m: 1_000_000 }
      : { '': 1, b: 1, kb: 1_000, k: 1_000, mb: 1_000_000, m: 1_000_000, gb: 1_000_000_000, g: 1_000_000_000 }
  const factor = (scale as Record<string, number>)[match[2]!.toLowerCase()]
  if (factor === undefined) return { problem: 'number' }
  const value = Number(match[1]) * factor
  if (!Number.isFinite(value)) return { problem: 'number' }
  if (field.min !== undefined && value < field.min) return { problem: 'min' }
  if (field.max !== undefined && value > field.max) return { problem: 'max' }
  if (field.step !== undefined && field.step > 0 && !Number.isInteger(Math.round(value / field.step * 1e9) / 1e9)) return { problem: 'step' }
  return { value }
}

/**
 * A duration as a reader says it: `500ms`, `30s`, `2m`, `1h 30m`.
 * @param ms - milliseconds.
 * @returns the shortest exact reading.
 */
export function formatDuration(ms: number): string {
  if (ms < 1_000 || ms % 1_000 !== 0) return `${ms}ms`
  const seconds = ms / 1_000
  if (seconds < 60 || seconds % 60 !== 0) return `${seconds}s`
  const minutes = seconds / 60
  if (minutes < 60) return `${minutes}m`
  return minutes % 60 === 0 ? `${minutes / 60}h` : `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

/**
 * A byte count in decimal units, exact where it can be: `64 KB`, `1 MB`.
 * @param bytes - the count.
 * @returns the reading.
 */
export function formatBytes(bytes: number): string {
  for (const [unit, size] of [['GB', 1e9], ['MB', 1e6], ['KB', 1e3]] as const) {
    if (bytes >= size) {
      const value = bytes / size
      return `${Number.isInteger(value) ? value : value.toFixed(1)} ${unit}`
    }
  }
  return `${bytes} B`
}
