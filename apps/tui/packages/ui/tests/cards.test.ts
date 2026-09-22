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
    expect(failed.result).toEqual({ title: '', detail: [{ text: 'boom' }, { text: 'exit 2' }] })
    const passed = round({ presentResult: () => ({ card: 'terminal', output: 'fine\n', exitCode: 0 }) }, '{}', ok)
    expect(passed.result).toEqual({ title: '', detail: [{ text: 'fine' }] })
  })

  it('marks only the changed span of a hunk, leaving its context unmarked', () => {
    const { result } = round({ presentResult: () => ({
      card: 'diff',
      diffs: [{ path: 'a.ts', oldText: 'keep\nold\ntail', newText: 'keep\nnew\ntail' }],
    }) }, '{}', ok)
    expect(result?.detail).toEqual([
      { text: 'a.ts' },
      { text: '  keep' },
      { text: '- old', emphasis: 'removed' },
      { text: '+ new', emphasis: 'added' },
      { text: '  tail' },
    ])
  })

  it('marks every line of a created file as added', () => {
    const { result } = round({ presentResult: () => ({
      card: 'diff', diffs: [{ path: 'new.ts', oldText: null, newText: 'one\ntwo\n' }],
    }) }, '{}', ok)
    expect(result?.detail).toEqual([
      { text: 'new.ts' },
      { text: '+ one', emphasis: 'added' },
      { text: '+ two', emphasis: 'added' },
    ])
  })

  it('captions a partial read window and leaves a whole file uncaptioned', () => {
    const window = round({ presentResult: () => ({
      card: 'read', path: 'a.ts', offset: 9, totalLines: 40,
      lines: [{ number: 9, text: 'nine' }, { number: 10, text: 'ten' }],
    }) }, '{}', ok)
    expect(window.result?.detail).toEqual([
      { text: ' 9  nine' }, { text: '10  ten' }, { text: `9–10/40 ${copy.cardLines}` },
    ])
    const whole = round({ presentResult: () => ({
      card: 'read', path: 'a.ts', offset: 1, totalLines: 1, lines: [{ number: 1, text: 'only' }],
    }) }, '{}', ok)
    expect(whole.result?.detail).toEqual([{ text: '1  only' }])
  })

  it('never presents a capped search as a complete one', () => {
    const { result } = round({ presentResult: () => ({
      card: 'search', shape: 'matches', truncated: true, total: 90,
      files: [{ path: 'a.ts', matches: [{ lineNumber: 7, line: 'hit' }] }],
    }) }, '{}', ok)
    expect(result?.detail).toEqual([
      { text: 'a.ts' },
      { text: '  7  hit' },
      { text: `90 ${copy.cardMatches} (${copy.cardTruncated})` },
    ])
  })

  it('reports a fetch by status and url', () => {
    const { result } = round({ presentResult: () => ({
      card: 'web', kind: 'fetch', url: 'https://example.com', statusCode: 200, truncated: false,
    }) }, '{}', ok)
    expect(result?.detail).toEqual([{ text: '200 https://example.com' }])
  })

  it('words its counts in the reader locale', () => {
    const zh = new ToolCards(() => ({
      presentResult: () => ({ card: 'search', shape: 'paths', paths: ['a.ts'], truncated: false, total: 1 }),
    }), dictionaries.zh)
    zh.call('c1', 'glob', '{}')
    expect(zh.result('c1', ok)?.detail.at(-1)).toEqual({ text: `1 ${dictionaries.zh.cardPaths}` })
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
