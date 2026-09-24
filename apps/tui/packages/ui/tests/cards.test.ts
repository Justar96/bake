/**
 * Tool card mapping. Runs under `bun test` because the module under test is
 * pure over the presenters it is handed — no registry, no harness, no clock.
 */

import { describe, expect, it } from 'bun:test'
import { ToolCards, type ToolPresenters } from '../src/cards.ts'
import { dictionaries } from '../src/copy.ts'

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

  it('words its counts in the reader locale', () => {
    const zh = new ToolCards(() => ({
      presentResult: () => ({ card: 'search', shape: 'paths', paths: ['a.ts'], truncated: false, total: 1 }),
    }), dictionaries.zh)
    zh.call('c1', 'glob', '{}')
    expect(zh.result('c1', ok)?.detail.at(-1)).toEqual({ text: `1 ${dictionaries.zh.cardPath}`, summary: 'count' })
  })

  it('has nothing to present for a result whose call it never saw', () => {
    expect(seam({ presentResult: () => ({ card: 'generic', title: 'x' }) }).result('unknown', ok))
      .toBeUndefined()
  })

  it('releases a call once its result arrives, so a retry presents nothing twice', () => {
    const cards = seam({ presentResult: () => ({ card: 'generic', title: 'done' }) })
    cards.call('c1', 'tool', '{}')
    expect(cards.result('c1', ok)).toEqual({ title: 'done', detail: [] })
    expect(cards.result('c1', ok)).toBeUndefined()
  })
})
