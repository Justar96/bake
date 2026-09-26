/** Completion replaces the cursor's token while preserving the rest of the draft. */
import { expect, it } from 'bun:test'
import { argumentQuery, commandUsage, completionMenu, completions, requiresInput, suggestCommand, type CompletionCatalog, type FileCatalog } from '../src/completion.ts'

const commands: CompletionCatalog = { loading: false, error: undefined, entries: [{ name: 'help', description: 'Help', kind: 'command' }] }
const files = (query: string, path: string, kind: 'file' | 'directory' = 'file'): FileCatalog => ({
  query, entries: [{ path, kind }], loading: false, error: undefined,
})

it('puts common commands first while retaining other registered commands and skills', () => {
  const entries: CompletionCatalog['entries'] = [
    { name: 'help', description: 'Help', kind: 'command' },
    { name: 'compact', description: 'Compact', kind: 'command' },
    { name: 'clear', description: 'Clear', kind: 'command' },
    { name: 'review', description: 'Review', kind: 'skill' },
    { name: 'resume', description: 'Resume', kind: 'command' },
    { name: 'new', description: 'New', kind: 'command' },
    { name: 'model', description: 'Model', kind: 'command' },
  ]
  expect(completions(entries, '/')?.map(entry => entry.name)).toEqual([
    'model', 'resume', 'new', 'clear', 'help', 'compact', 'review',
  ])
  expect(completions(entries, '/c')?.map(entry => entry.name)).toEqual(['clear', 'compact'])
})

it('replaces the entire slash token and retains existing arguments', () => {
  const choice = completionMenu(commands, files('', ''), '/heXX later', 3)?.entries[0]
  expect(choice).toMatchObject({ draft: '/help later', cursor: 6 })
})

it('replaces a middle file token without appending its old suffix or another separator', () => {
  const draft = 'Read @src/wrong.ts carefully'
  const choice = completionMenu(commands, files('src/wr', 'src/right.ts'), draft, 'Read @src/wr'.length)?.entries[0]
  expect(choice).toMatchObject({ draft: 'Read @src/right.ts carefully', cursor: 'Read @src/right.ts '.length })
})

it('drills into a quoted directory while retaining its closing quote and following text', () => {
  const draft = 'Read @"old path/file.txt" next'
  const directory = completionMenu(commands, files('old', 'notes folder', 'directory'), draft, 'Read @"old'.length)?.entries[0]
  expect(directory).toMatchObject({ draft: 'Read @"notes folder/" next', cursor: 'Read @"notes folder/'.length })
  const file = completionMenu(commands, files('notes folder/', 'notes folder/read me.txt'), directory!.draft, directory!.cursor)?.entries[0]
  expect(file).toMatchObject({ draft: 'Read @"notes folder/read me.txt" next', cursor: 'Read @"notes folder/read me.txt" '.length })
})

it('does not open completion after a closing quote or select stale query results', () => {
  expect(completionMenu(commands, files('old', 'old.txt'), 'Read @new', 9)?.entries).toEqual([])
  expect(completionMenu(commands, files('old', 'old.txt'), 'Read @"old"')).toBeUndefined()
  expect(completionMenu(commands, files('', ''), 'email@example.com')).toBeUndefined()
})

it('treats only a leading <placeholder> as required input', () => {
  expect(requiresInput({ kind: 'command', hint: '<path>' })).toBe(true)
  expect(requiresInput({ kind: 'command', hint: '[provider/model [effort]]' })).toBe(false)
  expect(requiresInput({ kind: 'command' })).toBe(false)
  expect(requiresInput({ kind: 'skill', hint: '<text>' })).toBe(false)
})

it('names the command whose arguments are being typed', () => {
  const entries: CompletionCatalog['entries'] = [
    { name: 'attach', description: 'Attach', kind: 'command', hint: '<path>' },
    { name: 'help', description: 'Help', kind: 'command' },
  ]
  expect(commandUsage(entries, '/attach ')?.name).toBe('attach')
  expect(commandUsage(entries, '/attach ./notes.md')?.name).toBe('attach')
  // Still naming the command: the menu is open, not the usage line.
  expect(commandUsage(entries, '/attach')).toBeUndefined()
  // No hint, nothing to show.
  expect(commandUsage(entries, '/help ')).toBeUndefined()
  expect(commandUsage(entries, '/nope x')).toBeUndefined()
})

it('matches first-argument choices and preserves usage when the list has no match', () => {
  const catalog: CompletionCatalog = { loading: false, error: undefined,
    entries: [{ name: 'goal', description: 'Goal', kind: 'command', hint: '[objective|clear]', choices: true }],
    argument: { name: 'goal', partial: 'cl', entries: ['clear', { value: 'edit', requiresInput: true }], loading: false, error: undefined },
  }
  expect(argumentQuery(catalog.entries, '/goal cl')).toMatchObject({ name: 'goal', partial: 'cl' })
  expect(completionMenu(catalog, files('', ''), '/goal cl')?.entries).toMatchObject([{ name: 'clear', draft: '/goal clear ' }])
  const editing = completionMenu({ ...catalog, argument: { ...catalog.argument!, partial: 'ed' } }, files('', ''), '/goal ed')
  expect(editing?.entries).toMatchObject([{ name: 'edit', draft: '/goal edit ', argumentRequiresInput: true }])
  expect(argumentQuery(catalog.entries, '/goal clear now')).toBeUndefined()
  expect(completionMenu({ ...catalog, argument: { ...catalog.argument!, partial: 'zz' } }, files('', ''), '/goal zz')).toBeUndefined()
  expect(commandUsage(catalog.entries, '/goal zz')?.name).toBe('goal')
})

it('suggests the nearest command for a typo and nothing for a distant or ambiguous one', () => {
  const names = ['model', 'help', 'compact', 'clear', 'clear-pending', 'changelog']
  expect(suggestCommand(names, 'modle')).toBe('model')
  expect(suggestCommand(names, 'hlep')).toBe('help')
  expect(suggestCommand(names, 'Model')).toBe('model')
  expect(suggestCommand(names, 'comp')).toBe('compact')
  expect(suggestCommand(names, 'tmp')).toBeUndefined()
  // `c` prefixes several names and is one edit from none.
  expect(suggestCommand(names, 'c')).toBeUndefined()
})
