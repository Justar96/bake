/**
 * Tool card mapping. Runs under `bun test` because the module under test is
 * pure over the presenters it is handed — no registry, no harness, no clock.
 */

import { describe, expect, it } from 'bun:test'
import { ToolCards, type ToolPresenters } from '../src/cards.ts'
import { dictionaries } from '../src/copy.ts'
import { formatRow } from '../src/plain.ts'
import { present } from '../src/present.ts'

const copy = dictionaries.en

/** A card seam over one tool, which every call in these tests resolves to. */
const seam = (presenters: ToolPresenters): ToolCards => new ToolCards(() => presenters, copy)

/** Drive one call and its result through the seam, as the projection does. */
function round(presenters: ToolPresenters, args: string, result: Parameters<ToolCards['result']>[1]) {
  const cards = seam(presenters)
  const call = cards.call('c1', 'tool', args)
  return { call, result: cards.result('c1', result) }
}

const ok = { content: [{ type: 'text' as const, text: 'raw' }], isError: false }

describe('call cards', () => {
  it('heads a terminal card with the command and its description', () => {
    const cards = seam({ presentCall: args => ({
      card: 'terminal', title: (args as { command: string }).command, description: 'List files',
    }) })
    expect(cards.call('c1', 'bash', '{"command":"ls"}'))
      .toEqual({ title: 'ls', detail: [{ text: 'List files' }] })
  })

  it('leaves the proposed diff to the result, so a hunk prints once', () => {
    const cards = seam({ presentCall: () => ({
      card: 'diff', title: 'Write a.txt', diffs: [{ path: 'a.txt', oldText: null, newText: 'x' }],
    }) })
    expect(cards.call('c1', 'write', '{}')).toEqual({ title: 'Write a.txt', detail: [] })
  })

  it('draws a fenced block as its code, and a raw input the title repeats not at all', () => {
    const cards = seam({ presentCall: () => ({
      card: 'generic', title: 'Grep TODO in src', rawInput: 'TODO', content: [{ type: 'text', text: '```console\nnope\n```' }],
    }) })
    expect(cards.call('c1', 'grep', '{}')).toEqual({ title: 'Grep TODO in src', detail: [{ text: 'nope' }] })
  })

  it('draws a structured raw input a field or an item to a line', () => {
    const todos = seam({ presentCall: () => ({ card: 'generic', title: 'Update todo list', rawInput: [
      { content: 'Find the bug', status: 'completed' }, { content: 'Fix it', status: 'in_progress', tags: ['a'] },
    ] }) })
    expect(todos.call('c1', 'todo_write', '{}')?.detail).toEqual([
      { text: 'Find the bug  completed' }, { text: '{"content":"Fix it","status":"in_progress","tags":["a"]}' },
    ])
    const manage = seam({ presentCall: () => ({ card: 'generic', title: 'Manage profile plugins', rawInput: { action: 'list', names: ['x'] } }) })
    expect(manage.call('c1', 'plugins', '{}')?.detail).toEqual([{ text: 'action: list' }, { text: 'names: ["x"]' }])
  })

  it('keeps the raw arguments when they are not JSON', () => {
    const cards = seam({ presentCall: () => ({ card: 'generic', title: 'never reached' }) })
    expect(cards.call('c1', 'bash', '{"command": tru')).toBeUndefined()
  })

  it('renders a card kind this build does not know at its title alone', () => {
    const cards = seam({ presentCall: () => ({ card: 'hologram', title: 'Project it' } as never) })
    expect(cards.call('c1', 'future', '{}')).toEqual({ title: 'Project it', detail: [] })
  })
})

describe('result cards', () => {
  it('shows a non-zero exit and hides a zero one', () => {
    const failed = round({ presentResult: () => ({ card: 'terminal', output: 'boom\n', exitCode: 2 }) }, '{}', ok)
    expect(failed.result).toEqual({ title: '', detail: [{ text: 'boom' }, { text: 'exit 2', summary: 'failure' }] })
    const passed = round({ presentResult: () => ({ card: 'terminal', output: 'fine\n', exitCode: 0 }) }, '{}', ok)
    expect(passed.result).toEqual({ title: '', detail: [{ text: 'fine' }] })
  })

  const diffCard = (diffs: readonly unknown[]) =>
    round({ presentResult: () => ({ card: 'diff', diffs }) } as never, '{}', ok).result?.detail

  it('shows only the changed lines of a hunk, each numbered on its own side', () => {
    expect(diffCard([{ path: 'a.ts', oldText: 'keep\nold\ntail', newText: 'keep\nnew\ntail', oldStart: 10, newStart: 10 }])).toEqual([
      { text: '- old', emphasis: 'removed', source: 'a.ts', number: 11 },
      { text: '+ new', emphasis: 'added', source: 'a.ts', number: 11 },
    ])
  })

  it('numbers each side past the lines the other side added or removed', () => {
    const detail = diffCard([{ path: 'a.ts', oldText: 'a\nb\nc', newText: 'a\nx\ny\nb\nc', oldStart: 4, newStart: 4 }])
    expect(detail).toEqual([
      { text: '+ x', emphasis: 'added', source: 'a.ts', number: 5 },
      { text: '+ y', emphasis: 'added', source: 'a.ts', number: 6 },
    ])
  })

  it('draws no numbers when the producer did not say where a hunk sits', () => {
    expect(diffCard([{ path: 'a.ts', oldText: 'old', newText: 'new' }])).toEqual([
      { text: '- old', emphasis: 'removed', source: 'a.ts' },
      { text: '+ new', emphasis: 'added', source: 'a.ts' },
    ])
  })

  it('separates changes a hunk joined, and hunks of one file, with a gap', () => {
    const detail = diffCard([
      { path: 'a.ts', oldText: 'a\nB\nc\nd\nE\nf', newText: 'a\nb\nc\nd\ne\nf', oldStart: 1, newStart: 1 },
      { path: 'a.ts', oldText: 'x\nY', newText: 'x\ny', oldStart: 30, newStart: 30 },
    ])
    expect(detail?.map(line => [line.text, line.number])).toEqual([
      ['- B', 2], ['+ b', 2], ['⋯', undefined], ['- E', 5], ['+ e', 5], ['⋯', undefined], ['- Y', 31], ['+ y', 31],
    ])
    expect(detail?.[2]).toEqual({ text: '⋯', emphasis: 'gap' })
  })

  it('heads each file with its path only when the edit touched several', () => {
    const detail = diffCard([
      { path: 'a.ts', oldText: 'a', newText: 'b', oldStart: 1, newStart: 1 },
      { path: 'b.ts', oldText: 'c', newText: 'd', oldStart: 1, newStart: 1 },
    ])
    expect(detail?.map(line => line.text)).toEqual(['a.ts', '- a', '+ b', 'b.ts', '- c', '+ d'])
  })

  it('marks the words an edited line changed, and not those of a replaced one', () => {
    const [removed, added] = diffCard([{ path: 'a.ts', oldText: 'const home = env.HOME ?? fallback()', newText: 'const home = resolved ?? env.HOME ?? fallback()' }])!
    expect(removed?.changed).toBeUndefined()
    expect(added?.changed?.map(([from, to]) => added.text.slice(from, to))).toEqual(['resolved ?? '])
    const edited = diffCard([{ path: 'a.ts', oldText: 'return undefined', newText: 'return null' }])!
    expect(edited.map(line => line.changed?.map(([from, to]) => line.text.slice(from, to)))).toEqual([['undefined'], ['null']])
    const replaced = diffCard([{ path: 'a.ts', oldText: 'import { a } from "b"', newText: 'export default function main() {}' }])!
    expect(replaced.map(line => line.changed)).toEqual([undefined, undefined])
  })

  it('marks every line of a created file as added, numbered from its first', () => {
    expect(diffCard([{ path: 'new.ts', oldText: null, newText: 'one\ntwo\n' }])).toEqual([
      { text: '+ one', emphasis: 'added', source: 'new.ts', number: 1 },
      { text: '+ two', emphasis: 'added', source: 'new.ts', number: 2 },
    ])
  })

  it('draws a file\'s controls as display text, and marks changed words where they are drawn', () => {
    const words = (line: { text: string, changed?: readonly (readonly [number, number])[] } | undefined) =>
      line?.changed?.map(([from, to]) => line.text.slice(from, to))
    // A tab is expanded before words are compared, so the run lands on the drawn text.
    const [, tab] = diffCard([{ path: 'a.ts', oldText: '\tconst limit = 1', newText: '\tconst limit = 2' }])!
    expect(tab).toEqual({ text: '+     const limit = 2', emphasis: 'added', source: 'a.ts', changed: [[20, 21]] })
    expect(words(tab)).toEqual(['2'])
    // Colour codes in a file are stripped, not sent to the terminal, and offsets
    // count only what is left.
    const ansi = diffCard([{ path: 'a.ts', oldText: '\x1b[31mconst value = 1\x1b[0m', newText: '\x1b[31mconst value = 2\x1b[0m' }])!
    expect(ansi.map(line => [line.text, line.changed])).toEqual([['- const value = 1', [[16, 17]]], ['+ const value = 2', [[16, 17]]]])
    // A CRLF ending is dropped, and a stray carriage return is escaped rather than
    // moving the cursor back over the line.
    const crlf = diffCard([{ path: 'a.ts', oldText: 'a = 1\r\n', newText: 'a = 2\r\nb\rc\r\n', oldStart: 1, newStart: 1 }])!
    expect(crlf.map(line => [line.text, line.number])).toEqual([['- a = 1', 1], ['+ a = 2', 1], ['+ b\\x0dc', 2]])
    expect(diffCard([{ path: 'n.ts', oldText: null, newText: '\x1b[1mx\x1b[0m\ty\x07\n' }])?.map(line => line.text)).toEqual(['+ x    y\\x07'])
  })

  it('captions a partial read window and leaves a whole file uncaptioned', () => {
    const window = round({ presentResult: () => ({
      card: 'read', path: 'a.ts', offset: 9, totalLines: 40,
      lines: [{ number: 9, text: 'nine' }, { number: 10, text: 'ten' }],
    }) }, '{}', ok)
    expect(window.result?.detail).toEqual([
      { text: ' 9  nine', source: 'a.ts', codeOffset: 4, number: 9, codeStart: true }, { text: '10  ten', source: 'a.ts', codeOffset: 4, number: 10 }, { text: `9–10/40 ${copy.cardLines}`, summary: 'count' },
    ])
    const whole = round({ presentResult: () => ({
      card: 'read', path: 'a.ts', offset: 1, totalLines: 1, lines: [{ number: 1, text: 'only' }],
    }) }, '{}', ok)
    expect(whole.result?.detail).toEqual([{ text: '1  only', source: 'a.ts', codeOffset: 3, number: 1, codeStart: true }])
  })

  it('never presents a capped search as a complete one', () => {
    const { result } = round({ presentResult: () => ({
      card: 'search', shape: 'matches', truncated: true, total: 90,
      files: [{ path: 'a.ts', matches: [{ lineNumber: 7, line: 'hit' }] }],
    }) }, '{}', ok)
    expect(result?.detail).toEqual([
      { text: 'a.ts' },
      { text: '  7  hit', source: 'a.ts', codeOffset: 5, number: 7, codeStart: true },
      { text: `90 ${copy.cardMatches} (${copy.cardTruncated})`, summary: 'count' },
    ])
    const one = round({ presentResult: () => ({
      card: 'search', shape: 'matches', truncated: false, total: 1,
      files: [{ path: 'a.ts', matches: [{ lineNumber: 7, line: 'hit' }] }],
    }) }, '{}', ok)
    expect(one.result?.detail.at(-1)).toEqual({ text: `1 ${copy.cardMatch}`, summary: 'count' })
  })

  it('reports a fetch by status and url', () => {
    const { result } = round({ presentResult: () => ({
      card: 'web', kind: 'fetch', url: 'https://example.com', statusCode: 200, truncated: false,
    }) }, '{}', ok)
    expect(result?.detail).toEqual([{ text: '200', summary: 'count' }, { text: 'https://example.com' }])
  })

  it('leaves out an address the call already names, and marks a failed status', () => {
    const presenters: ToolPresenters = {
      presentCall: () => ({ card: 'generic', title: 'https://example.com', rawInput: 'https://example.com' }),
      presentResult: () => ({ card: 'web', kind: 'fetch', url: 'https://example.com', statusCode: 404, truncated: false }),
    }
    const { call, result } = round(presenters, '{}', ok)
    expect(call).toEqual({ title: 'https://example.com', detail: [] })
    expect(result?.detail).toEqual([{ text: '404', summary: 'failure' }])
  })

  it('has nothing to present for a result whose call it never saw', () => {
    expect(seam({ presentResult: () => ({ card: 'generic', title: 'x' }) }).result('unknown', ok))
      .toBeUndefined()
  })

  it('releases a call once its result arrives, so a retry presents nothing twice', () => {
    const cards = seam({ presentResult: () => ({ card: 'generic', title: 'done', content: [{ type: 'text', text: 'ok' }] }) })
    cards.call('c1', 'tool', '{}')
    expect(cards.result('c1', ok)).toEqual({ title: 'done', detail: [{ text: 'ok' }] })
    expect(cards.result('c1', ok)).toBeUndefined()
  })

  it('keeps the raw result under a generic card that omits its content, and not under one that empties it', () => {
    // The contract: an omitted `content` renders the raw result content.
    expect(round({ presentResult: () => ({ card: 'generic' }) }, '{}', ok).result)
      .toEqual({ title: '', detail: [], raw: true })
    expect(round({ presentResult: () => ({ card: 'generic', title: 'Done' }) }, '{}', ok).result)
      .toEqual({ title: 'Done', detail: [], raw: true })
    // An empty list is the presenter's choice to show nothing.
    expect(round({ presentResult: () => ({ card: 'generic', content: [] }) }, '{}', ok).result)
      .toEqual({ title: '', detail: [] })
  })

  it('keeps the raw result under a card kind this build does not know', () => {
    expect(round({ presentResult: () => ({ card: 'hologram', title: 'Projected' } as never) }, '{}', ok).result)
      .toEqual({ title: 'Projected', detail: [], raw: true })
  })
})

describe('the files a command changed', () => {
  const terminal = (changes: unknown) => {
    const cards = new ToolCards(() => ({ presentResult: () => ({ card: 'terminal', output: 'ok\n', exitCode: 1, changes }) } as never), dictionaries.en)
    cards.call('c1', 'bash', '{}')
    return cards.result('c1', ok)
  }
  const hunk = (path: string, oldText: string | null, newText: string) => ({ path, oldText, newText, oldStart: 1, newStart: 1 })
  const files = [
    { path: 'a.js', status: 'modified', added: 1, removed: 1, hunks: [hunk('a.js', 'const retries = 3\n', 'const retries = 5\n')] },
    { path: 'b.js', status: 'created', added: 1, removed: 0, hunks: [hunk('b.js', null, 'module.exports = {}\n')] },
    { path: 'c.js', status: 'deleted', added: 0, removed: 2 },
    { path: 'd.js', status: 'renamed', from: 'old/d.js', added: 0, removed: 0 },
    { path: 'e.png', status: 'binary', added: 0, removed: 0 },
    { path: 'f.log', status: 'too-large', added: 0, removed: 0 },
    { path: 'run.sh', status: 'mode', added: 0, removed: 0 },
    { path: 'link', status: 'symlink', added: 0, removed: 0 },
    { path: 'g.js', status: 'unknown-before', added: 3, removed: 1 },
  ]

  it('keeps the output and exit in the detail, and the files beside them', () => {
    const card = terminal({ files })
    expect(card?.detail).toEqual([{ text: 'ok' }, { text: 'exit 1', summary: 'failure' }])
    expect(card?.changes?.files[0]).toEqual({ path: 'a.js', added: 1, removed: 1, lines: [
      { text: '- const retries = 3', emphasis: 'removed', source: 'a.js', number: 1, changed: [[18, 19]] },
      { text: '+ const retries = 5', emphasis: 'added', source: 'a.js', number: 1, changed: [[18, 19]] },
    ] })
    expect(card?.changes?.files[1]?.lines).toEqual([{ text: '+ module.exports = {}', emphasis: 'added', source: 'b.js', number: 1 }])
  })

  it('says how each file changed unless it was a plain edit, in plain words', () => {
    expect(terminal({ files })?.changes?.files.map(file => [file.path, file.status, file.lines.length])).toEqual([
      ['a.js', undefined, 2], ['b.js', 'new', 1], ['c.js', 'deleted', 0], ['d.js', 'renamed from old/d.js', 0], ['e.png', 'binary', 0],
      ['f.log', 'too large', 0], ['run.sh', 'mode', 0], ['link', 'symlink', 0], ['g.js', undefined, 0],
    ])
  })

  it('carries the files the producer left out, and its caveats', () => {
    expect(terminal({ files: files.slice(0, 1), omittedFiles: 4, concurrent: true, timedOut: true })?.changes)
      .toMatchObject({ omitted: 4, notes: [copy.changeConcurrent, copy.changeTimedOut] })
    expect(terminal({ files: files.slice(0, 1) })?.changes).not.toHaveProperty('notes')
  })

  it('adds nothing when no file is listed', () => {
    expect(terminal({ files: [] })).toEqual({ title: '', detail: [{ text: 'ok' }, { text: 'exit 1', summary: 'failure' }] })
  })

  it('draws a path as one line of text', () => {
    const [file] = terminal({ files: [{ path: 'odd\nname\x1b[2J.txt', status: 'renamed', from: 'was\rhere', added: 0, removed: 0 }] })!.changes!.files
    expect([file?.path, file?.status]).toEqual(['odd\\x0aname.txt', 'renamed from was\\x0dhere'])
  })

  it('is bounded by changed lines, keeps each drawn file\'s path, and counts the files left out', () => {
    const many = Array.from({ length: 5 }, (_, index) => ({ path: `f${index}.js`, status: 'modified', added: 2, removed: 0,
      hunks: [hunk(`f${index}.js`, '', 'a\nb\n')] }))
    const card = terminal({ files: many, omittedFiles: 3 })!
    const lines = present({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'gen', result: { ok: false, text: '', detail: card.detail, changes: card.changes! } },
      { lines: 4, unit: copy.cardLines, more: copy.moreLines, files: copy.cardFiles, moreFiles: copy.moreFiles, moreFile: copy.moreFile })
    expect(lines.slice(2).map(line => [line.verb, line.text])).toEqual([
      ['\u23bf', 'ok'],
      ['edited', 'f0.js'], ['', '+ a'], ['', '+ b'],
      ['edited', 'f1.js'], ['', '+ a'], ['', '+ b'],
      ['', `+6 ${copy.moreFiles}`],
    ])
  })

  it('joins the files onto a plain row after the output', () => {
    const card = terminal({ files: [files[0], files[1], files[8]], concurrent: true })!
    expect(formatRow({ kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'gen', result: { ok: false, text: '', detail: card.detail, changes: card.changes! } }))
      .toBe(`⚙ bash [c1](gen) ← error ok · exit 1 · edited a.js · - const retries = 3 · + const retries = 5 · edited b.js  new · + module.exports = {} · edited g.js  +3 −1 · ${copy.changeConcurrent}`)
  })
})
