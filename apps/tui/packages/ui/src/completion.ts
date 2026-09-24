/** Pure completion presentation using the Harness's path grammar. */
import { activeAtToken, formatFileMention } from '@deepseek-ai/dsh-file-reference/grammar'
import type { FileReferenceCandidate } from '@deepseek-ai/dsh-file-reference/types'

/** One Harness-owned command or user-invocable skill. */
export interface Completion {
  readonly name: string
  readonly description: string
  readonly kind: 'command' | 'skill'
}

/** Presentation snapshot of the session's discovery catalogs. */
export interface CompletionCatalog {
  readonly entries: readonly Completion[]
  readonly loading: boolean
  readonly error: string | undefined
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
  readonly draft: string
  readonly cursor: number
}

/** Visible choices and discovery feedback for one composer token. */
export interface CompletionMenu {
  readonly kind: 'slash' | 'file'
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
