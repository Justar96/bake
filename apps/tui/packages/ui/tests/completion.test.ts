/** Completion replaces the cursor's token while preserving the rest of the draft. */
import { expect, it } from 'bun:test'
import { completionMenu, completions, type CompletionCatalog, type FileCatalog } from '../src/completion.ts'

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
