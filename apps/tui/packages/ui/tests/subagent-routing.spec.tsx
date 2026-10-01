/** Routing provenance belongs to the child; the parent's model and compact chrome stay unchanged. */
import React from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { renderToString } from 'ink'
import stringWidth from 'string-width'
import { cleanup, renderAt } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { PALETTE } from '../src/palette.ts'
import { Sheet } from '../src/sheet.tsx'
import { subagentLine, subagentSheet, type SubagentEntry, type SubagentRouting } from '../src/subagents.tsx'
import { emptyTranscript } from '../src/transcript.ts'

afterEach(cleanup)

const routed: SubagentRouting = {
  source: 'auto', route: { provider: 'deepseek-official', model: 'tui-picked-model', reasoningEffort: 'high' },
  router: { reason: 'Integration checks need stronger reasoning.', fallback: false,
    assessment: { policy: '2026-10-01', status: 'normal', difficulty: 0.74, reasons: [] } },
}
const cautious: SubagentRouting = {
  ...routed, router: { ...routed.router!,
    assessment: { ...routed.router!.assessment!, status: 'cautious', reasons: ['limited benchmark support'] } },
}
const missing: SubagentRouting = {
  source: 'fallback', route: { provider: 'deepseek-official', model: 'deepseek-v4-flash' },
  router: { reason: 'Objective needs earlier context.', fallback: true,
    assessment: { policy: '2026-10-01', status: 'needs_context', difficulty: 0.2, reasons: ['missing conversation context'] } },
}

function children(): readonly SubagentEntry[] {
  return [
    { id: 'review', label: 'Review', routing: routed },
    { id: 'coverage', label: 'Coverage', routing: cautious },
    { id: 'continue', label: 'Continue', routing: missing },
    { id: 'selected', label: 'Selected model', routing: { source: 'explicit' as const, route: routed.route! } },
    { id: 'default', label: 'Default model', routing: { source: 'default' as const, route: missing.route! } },
    { id: 'legacy', label: 'Older child' },
  ].map(entry => ({ ...entry, state: 'working', detail: 'One-shot', inspectable: true }))
}

const lineText = (line: ReturnType<typeof subagentSheet>[number]): string =>
  line.parts?.map(part => part.text).join('') ?? line.text

it.each(['en', 'zh'] as const)('shows recorded provenance, effective route, and evidence in %s', async locale => {
  const copy = dictionaries[locale]
  const entries = children()
  const lines = subagentSheet(entries, 1, copy)
  const text = lines.map(lineText).join('\n')
  expect(text).toContain(`${copy.subagentRoutingAuto} · ${copy.subagentRoutingCautious}`)
  expect(text).toContain(`${copy.subagentRoutingDefault} · ${copy.subagentRoutingNeedsContext}`)
  expect(text).toContain(copy.subagentRoutingSelected)
  expect(text).toContain(`deepseek-official/tui-picked-model · ${copy.think} high`)
  expect(text).toContain(`${copy.subagentRoutingDifficulty} 0.74`)
  expect(text).toContain('Integration checks need stronger reasoning.\nlimited benchmark support')
  expect(text).not.toContain('Objective needs earlier context.')
  expect(lines.find(line => line.parts?.[0]?.text === 'Older child')?.parts).toHaveLength(2)
  const frame = renderToString(<Sheet tabs={[{ label: copy.subagentsTitle, color: PALETTE.asking, current: true }]}
    color={PALETTE.asking} lines={lines} keys={copy.subagentsOpen} columns={80} limit={24} offset={0}
    follow={subagentLine(1, 0, entries[1]!.routing)} frame="round" />, { columns: 80 })
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/subagent-routing.${locale}.txt`)
})

it('keeps routing warnings distinct from a successful child outcome and uses words without colour', () => {
  const entry: SubagentEntry = { id: 'done', label: 'Finished', detail: 'One-shot', inspectable: true,
    state: 'saved', outcome: 'completed', routing: cautious }
  const lines = subagentSheet([entry], -1, dictionaries.en)
  const row = lines.at(-1)!
  expect(lineText(row)).toBe('Finished  Completed · Saved · Auto · cautious')
  expect(row.parts?.at(-1)).toEqual({ text: ' · Auto · cautious', color: PALETTE.waiting })
})

it('distinguishes a retained fallback from an ordinary default without requiring colour', () => {
  const entry = children()[0]!
  const normal = subagentSheet([{ ...entry, routing: { source: 'default' } }], -1, dictionaries.en).at(-1)!
  const fallback = subagentSheet([{ ...entry, routing: { source: 'fallback' } }], -1, dictionaries.en).at(-1)!
  expect(lineText(normal)).toBe('Review  Working · Default')
  expect(lineText(fallback)).toBe('Review  Working · Default · fallback')
})

it.each(['auto', 'explicit'] as const)('keeps recorded %s provenance when optional router metadata contradicts it', source => {
  const entry = children()[0]!
  const routing: SubagentRouting = { ...routed, source, router: { ...routed.router!, fallback: source === 'explicit',
    assessment: { ...routed.router!.assessment!, status: 'fallback' } } }
  const row = subagentSheet([{ ...entry, routing }], -1, dictionaries.en).at(-1)!
  expect(lineText(row)).toBe(`Review  Working · ${source === 'auto' ? 'Auto' : 'Selected'} · fallback`)
})

it.each([40, 80])('wraps the selected route and explanation within %i columns', async columns => {
  const entries = children()
  const lines = subagentSheet(entries, 2, dictionaries.en)
  const frame = renderToString(<Sheet tabs={[{ label: 'Subagents', color: PALETTE.asking, current: true }]}
    color={PALETTE.asking} lines={lines} keys="Enter opens" columns={columns} limit={24} offset={0}
    follow={subagentLine(2, 0, entries[2]!.routing)} frame="round" />, { columns })
  for (const line of frame.split('\n')) expect(stringWidth(line)).toBeLessThanOrEqual(columns)
  expect(frame).toContain('Difficulty 0.20')
  expect(frame).toContain('Objective needs earlier context.')
  expect(frame).not.toContain('think high')
  if (columns === 40) await expect(frame + '\n').toMatchFileSnapshot('./expected/subagent-routing.40.txt')
})

it('keeps the selected child and all of its logical details in the followed range', () => {
  const entries = children()
  for (const [index, entry] of entries.entries()) {
    const lines = subagentSheet(entries, index, dictionaries.en)
    const [first, last] = subagentLine(index, 0, entry.routing)
    expect(lines[first]?.selected).toBe(true)
    expect(lines[last]?.selected).toBe(false)
    expect(lines[last + 1]?.parts?.[0]?.text).toBe(entries[index + 1]?.label)
  }
})

it('leaves long routing evidence reachable when the sheet is shorter than the selected details', () => {
  const entry = children()[0]!
  const routing: SubagentRouting = { ...cautious, router: { ...cautious.router!,
    reason: `${'Independent integration evidence. '.repeat(20)}Evidence tail.` } }
  const lines = subagentSheet([{ ...entry, routing }], 0, dictionaries.en)
  const sheet = (offset: number, follow?: readonly [number, number]) => renderToString(
    <Sheet tabs={[{ label: 'Subagents', color: PALETTE.asking, current: true }]} color={PALETTE.asking}
      lines={lines} keys="PgDn more" columns={40} limit={10} offset={offset} follow={follow} frame="round" />, { columns: 40 })
  expect(sheet(0, subagentLine(0, 0, routing))).toContain('Review  Working · Auto')
  expect(sheet(1000)).toContain('tail.')
  expect(sheet(1000)).toContain('limited benchmark support')
})

it('treats router and provider controls as display data and deduplicates repeated reasons', () => {
  const entry = children()[0]!
  const routing: SubagentRouting = { source: 'auto',
    route: { provider: '\x1b[31mdeepseek-official\x1b[0m', model: 'model\r\nname', reasoningEffort: 'high\x07' },
    router: { fallback: false, reason: 'first\nsecond\x1b[2J',
      assessment: { policy: 'test', status: 'normal', difficulty: 0.5, reasons: ['first\nsecond', 'extra\x00detail'] } },
  }
  const lines = subagentSheet([{ ...entry, routing }], 0, dictionaries.en)
  const details = lines.slice(5).map(lineText)
  expect(details).toEqual(['deepseek-official/model name · think high\\x07', 'Difficulty 0.50 · Auto', 'first second', 'extra\\x00detail'])
  expect(details.join('')).not.toMatch(/[\x00-\x1f\x7f-\x9f]/)
})

function appProps(): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'deepseek-official/deepseek-v4-flash', cwd: '/workspace', sessionId: 'parent', copy: dictionaries.en,
    frame: 'round', quitting: false, context: undefined, subagents: children(),
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(),
  }
}

it('follows a newly selected child including its routing details without changing the parent model', async () => {
  const ui = renderAt(<App {...appProps()} />, 80, 32)
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('deepseek-official/tui-picked-model'))
  ui.stdin.write('\x1b[B\x1b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Objective needs earlier context.'))
  expect(ui.lastFrame()).toContain('Difficulty 0.20 · Default · needs context')
  expect(ui.lastFrame()).not.toContain('Integration checks need stronger reasoning.')
  expect(ui.lastFrame()?.split('\n').at(-1)).toContain('deepseek-v4-flash')
})

it('names paging at 40 columns and reaches all 8 routing reasons without moving to another child', async () => {
  const entry = children()[0]!
  const routing: SubagentRouting = { ...cautious, router: { ...cautious.router!, reason: 'Main explanation.',
    assessment: { ...cautious.router!.assessment!,
      reasons: Array.from({ length: 8 }, (_, index) => `Evidence ${index}: ${'word '.repeat(38)}tail-${index}`) } } }
  const ui = renderAt(<App {...appProps()} subagents={[{ ...entry, routing }]} />, 40, 16)
  ui.stdin.write('\x07')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('PgUp/PgDn scroll'))
  const first = ui.lastFrame()!
  const seen = new Set<number>()
  for (let page = 0; page < 100; page++) {
    const frame = ui.lastFrame()!
    for (let index = 0; index < 8; index++) if (frame.includes(`tail-${index}`)) seen.add(index)
    expect(frame).toContain('PgUp/PgDn scroll')
    expect(frame.split('\n').length).toBeLessThanOrEqual(16)
    for (const line of frame.split('\n')) expect(stringWidth(line)).toBeLessThanOrEqual(40)
    if (seen.size === 8) break
    ui.stdin.write('\x1b[6~')
    await vi.waitFor(() => expect(ui.lastFrame()).not.toBe(frame))
  }
  expect(seen.size).toBe(8)
  await expect(`FIRST PAGE\n${first}\n\nLAST PAGE\n${ui.lastFrame()}\n`)
    .toMatchFileSnapshot('./expected/subagent-routing-pages.40.txt')
})
