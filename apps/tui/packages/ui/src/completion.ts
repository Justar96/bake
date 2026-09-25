/** Pure completion presentation using the Harness's path grammar. */
import { activeAtToken, formatFileMention } from '@deepseek-ai/dsh-file-reference/grammar'
import type { FileReferenceCandidate } from '@deepseek-ai/dsh-file-reference/types'

/** One Harness-owned command or user-invocable skill. */
export interface Completion {
  readonly name: string
  readonly description: string
  readonly kind: 'command' | 'skill'
  /** The command's advertised argument placeholder; `<…>` marks required input. */
  readonly hint?: string
  /** Whether the command service advertises argument choices. */
  readonly choices?: boolean
}

/** One active command argument query and its observed choices. */
export interface ArgumentCatalog {
  readonly name: string
  readonly partial: string
  readonly entries: readonly (string | { readonly value: string; readonly requiresInput?: boolean })[]
  readonly loading: boolean
  readonly error: string | undefined
}

/** Presentation snapshot of the session's discovery catalogs. */
export interface CompletionCatalog {
  readonly entries: readonly Completion[]
  readonly loading: boolean
  readonly error: string | undefined
  readonly argument?: ArgumentCatalog | undefined
}

/** Query-tagged paths observed by the application; never file contents. */
export interface FileCatalog {
  readonly query: string | undefined
  readonly entries: readonly FileReferenceCandidate[]
  readonly loading: boolean
  readonly error: string | undefined
}

/** One selectable row and the exact draft it inserts. */
export interface CompletionChoice {
  readonly name: string
  readonly description: string
  readonly kind: Completion['kind'] | FileReferenceCandidate['kind']
  readonly hint?: string
  readonly argumentRequiresInput?: boolean
  readonly draft: string
  readonly cursor: number
}

/** Visible choices and discovery feedback for one composer token. */
export interface CompletionMenu {
  readonly kind: 'slash' | 'argument' | 'file'
  readonly query: string | undefined
  readonly entries: readonly CompletionChoice[]
  readonly loading: boolean
  readonly error: string | undefined
}

const frequentCommands = ['model', 'resume', 'new', 'clear'] as const
const commandRank = (entry: Completion): number => {
  const index = entry.kind === 'command' ? frequentCommands.findIndex(name => name === entry.name) : -1
  return index < 0 ? frequentCommands.length : index
}

/**
 * Build a menu at the cursor; earlier path queries cannot supply choices.
 * @param commands - scoped command and skill metadata.
 * @param files - application-owned path discovery.
 * @param draft - complete composer text.
 * @param cursor - UTF-16 insertion position, defaulting to the end.
 * @returns the active menu, or undefined outside a completion token.
 */
export function completionMenu(commands: CompletionCatalog, files: FileCatalog, draft: string, cursor = draft.length): CompletionMenu | undefined {
  const slash = completions(commands.entries, draft.slice(0, cursor))
  if (slash !== undefined) {
    const end = draft.search(/\s/u)
    return { ...commands, kind: 'slash', query: undefined, entries: slash.map(entry => ({ ...entry, name: `/${entry.name}`,
      ...replaceToken(draft, 0, end < 0 ? draft.length : end, `/${entry.name}`, false),
    })) }
  }
  const argument = argumentQuery(commands.entries, draft, cursor)
  if (argument !== undefined) {
    const observed = commands.argument
    const current = observed?.name === argument.name && observed.partial === argument.partial
    const entries = current ? observed.entries.filter(choice => (typeof choice === 'string' ? choice : choice.value)
      .toLowerCase().startsWith(argument.partial.toLowerCase())) : []
    if (current && entries.length === 0 && !observed.loading && observed.error === undefined) return undefined
    return { kind: 'argument', query: undefined, loading: !current || observed.loading, error: current ? observed.error : undefined,
      entries: entries.map(choice => ({ name: typeof choice === 'string' ? choice : choice.value, description: '', kind: 'command',
        ...typeof choice === 'string' || choice.requiresInput !== true ? {} : { argumentRequiresInput: true },
        ...replaceToken(draft, argument.start, argument.end, typeof choice === 'string' ? choice : choice.value, false),
      })) }
  }
  const token = activeAtToken(draft, cursor)
  if (token === undefined) return undefined
  const start = cursor - token.prefix.length
  const quoted = draft.startsWith('@"', start)
  const closing = quoted ? draft.indexOf('"', start + 2) : -1
  if (closing >= 0 && cursor > closing) return undefined
  if (files.query !== token.query) return { kind: 'file', query: token.query, entries: [], loading: true, error: undefined }
  const remainder = draft.slice(cursor)
  const boundary = remainder.search(quoted ? /\n/u : /\s/u)
  const end = closing >= 0 ? closing + 1 : boundary < 0 ? draft.length : cursor + boundary
  const entries = files.entries.flatMap(candidate => {
    const mention = formatFileMention(candidate, token.quoted)
    if (mention === undefined) return []
    return [{ name: mention, description: '', kind: candidate.kind,
      ...replaceToken(draft, start, end, mention, candidate.kind === 'directory'),
    }]
  })
  return { ...files, kind: 'file', query: token.query, entries }
}

/** Active first argument of a command advertising choices, including an empty partial. */
export function argumentQuery(entries: readonly Completion[], draft: string, cursor = draft.length): { name: string; partial: string; start: number; end: number } | undefined {
  const head = /^\/([a-z][a-z0-9_-]*)[\t ]+/iu.exec(draft)
  if (head === null || cursor < head[0].length) return undefined
  const entry = entries.find(item => item.kind === 'command' && item.choices && item.name === head[1]?.toLowerCase())
  if (entry === undefined) return undefined
  const start = head[0].length
  const rest = draft.slice(start)
  const end = start + (rest.search(/[\t \n\r]/u) < 0 ? rest.length : rest.search(/[\t \n\r]/u))
  if (cursor > end || draft.slice(start, cursor).includes('\n')) return undefined
  return { name: entry.name, partial: draft.slice(start, cursor), start, end }
}

function replaceToken(draft: string, start: number, end: number, mention: string, directory: boolean): { draft: string; cursor: number } {
  const prefix = draft.slice(0, start) + mention
  const suffix = draft.slice(end)
  if (directory) return { draft: prefix + (mention.startsWith('@"') ? '"' : '') + suffix, cursor: prefix.length }
  const separator = /^\s/u.test(suffix) ? '' : ' '
  return { draft: prefix + separator + suffix, cursor: prefix.length + 1 }
}

/**
 * Match a leading slash token before its argument separator.
 * @param entries - effective commands and user-invocable skills, with unique names.
 * @param draft - complete composer text.
 * @returns prefix matches, or undefined outside the slash menu.
 */
export function completions(entries: readonly Completion[], draft: string): readonly Completion[] | undefined {
  const token = /^\/([a-z0-9_-]*)$/i.exec(draft)
  if (token === null) return undefined
  const prefix = token[1]!.toLowerCase()
  return entries.filter(entry => entry.name.startsWith(prefix))
    .sort((left, right) => commandRank(left) - commandRank(right))
}

/**
 * Whether a command cannot run without arguments. Hints follow the usage
 * convention: `<required>` and `[optional]`.
 * @param entry - a catalog entry.
 * @returns true when its hint opens with a required placeholder.
 */
export function requiresInput(entry: { readonly kind: string; readonly hint?: string }): boolean {
  return entry.kind === 'command' && entry.hint?.trimStart().startsWith('<') === true
}

/**
 * The command whose arguments are being typed, once the menu has closed.
 * @param entries - effective commands and skills.
 * @param draft - complete composer text.
 * @returns the command named before the first separator, when it has a hint.
 */
export function commandUsage(entries: readonly Completion[], draft: string): Completion | undefined {
  const token = /^\/([a-z][a-z0-9_-]*)[\t ]/u.exec(draft)
  if (token === null) return undefined
  const entry = entries.find(candidate => candidate.kind === 'command' && candidate.name === token[1])
  return entry?.hint === undefined ? undefined : entry
}

/**
 * The closest known name to a mistyped one: a prefix match, or at most two
 * edits away. Nothing when the typed name is ambiguous or too far from all.
 * @param names - effective command and skill names.
 * @param typed - the name as submitted, without its slash.
 * @returns the suggestion, or undefined.
 */
export function suggestCommand(names: readonly string[], typed: string): string | undefined {
  const lower = typed.toLowerCase()
  if (names.includes(lower)) return lower
  const prefixed = names.filter(name => name.startsWith(lower))
  if (prefixed.length === 1) return prefixed[0]
  let best: string | undefined
  let bestDistance = Math.min(2, Math.floor(lower.length / 2))
  let tied = false
  for (const name of names) {
    const distance = editDistance(lower, name)
    if (distance < bestDistance || (distance === bestDistance && best === undefined)) { best = name; bestDistance = distance; tied = false }
    else if (distance === bestDistance) tied = true
  }
  return tied ? undefined : best
}

/** Levenshtein distance counting an adjacent transposition as one edit. */
function editDistance(left: string, right: string): number {
  const rows = Array.from({ length: left.length + 1 }, (_, index) => [index, ...Array<number>(right.length).fill(0)])
  for (let column = 1; column <= right.length; column++) rows[0]![column] = column
  for (let row = 1; row <= left.length; row++) {
    for (let column = 1; column <= right.length; column++) {
      const cost = left[row - 1] === right[column - 1] ? 0 : 1
      let value = Math.min(rows[row - 1]![column]! + 1, rows[row]![column - 1]! + 1, rows[row - 1]![column - 1]! + cost)
      if (row > 1 && column > 1 && left[row - 1] === right[column - 2] && left[row - 2] === right[column - 1]) {
        value = Math.min(value, rows[row - 2]![column - 2]! + 1)
      }
      rows[row]![column] = value
    }
  }
  return rows[left.length]![right.length]!
}
