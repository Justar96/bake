/** The status line's fields and the order they give way in. Pure, so it runs under `bun test`. */
import { describe, expect, it } from 'bun:test'
import { dictionaries } from '../src/copy.ts'
import { AGENT_TONES, CONTEXT_RAMP, PALETTE } from '../src/palette.ts'
import { CWD_MIN, fitStatus, formWidth, MODEL_MIN, RANK, statusFields, type StatusField, type StatusInput } from '../src/status-line.ts'

const copy = dictionaries.en
const text = (parts: readonly { readonly text: string }[]): string => parts.map(part => part.text).join('')
/** The row's text as fitted, each cut field ending or opening on an ellipsis as the renderer draws it (ASCII fields only). */
const row = (fields: readonly StatusField[], room: number): string => fitStatus(fields, room).map(field => {
  const whole = text(field.parts)
  return field.cut === 'start' ? `…${whole.slice(-(field.width - 1))}` : field.cut === 'end' ? `${whole.slice(0, field.width - 1)}…` : whole
}).join('  ')
const base: StatusInput = { model: 'deepseek-official/deepseek-v4-flash', cwd: '~/bake', glyphs: 'unicode' }

describe('statusFields', () => {
  it('names the model without its provider or a label, and every other field by a lowercase word', () => {
    const fields = statusFields({ ...base, thinkingLevel: 'high', context: { used: 15_200, window: 128_000 },
      usage: { input: 42_300, output: 3_100, cached: 34_500 } }, copy)
    expect(fields.map(field => text(field.forms[0]!))).toEqual([
      'deepseek-v4-flash', 'think high', 'ctx ~11% (15.2k/128k)', 'in 42.3k  out 3.1k  cache hit 81%', '~/bake'])
    // Labels are dim; values keep the normal foreground while they are healthy.
    const [, think, context, tokens] = fields
    expect(think!.forms[0]).toEqual([{ text: 'think ', dim: true }, { text: 'high', color: PALETTE.asking }])
    expect(context!.forms[0]).toEqual([{ text: 'ctx ', dim: true }, { text: '~11% (15.2k/128k)', color: undefined }])
    expect(tokens!.forms[0]!.at(-1)).toEqual({ text: '81%', color: undefined })
  })

  it('warms the thinking level as the effort rises, and keeps an effort it does not know plain', () => {
    const level = (thinkingLevel: string) => statusFields({ ...base, thinkingLevel }, copy)[1]!.forms[0]![1]
    expect(['minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'Turbo'].map(level)).toEqual([
      { text: 'minimal', dim: true }, { text: 'low', dim: true }, { text: 'medium' },
      { text: 'high', color: PALETTE.asking }, { text: 'xhigh', color: CONTEXT_RAMP[2] },
      { text: 'max', color: AGENT_TONES[1] }, { text: 'Turbo' },
    ])
  })

  it('narrows the totals to the cache hit, which outlasts them, and drops totals with no cache whole', () => {
    const [, cached] = statusFields({ ...base, usage: { input: 900, output: 100, cached: 200 } }, copy)
    expect(cached!.forms.map(text)).toEqual(['in 900  out 100  cache hit 22%', 'cache hit 22%'])
    expect(cached!.yields).toEqual([RANK.totals, RANK.cacheHit])
    // A poor hit is the one reading here that warns.
    expect(cached!.forms[1]!.at(-1)).toEqual({ text: '22%', color: PALETTE.failed })
    const [, uncached] = statusFields({ ...base, usage: { input: 900, output: 100 } }, copy)
    expect(uncached!.forms.map(text)).toEqual(['in 900  out 100'])
    expect(uncached!.yields).toEqual([RANK.totals])
  })

  it('leads the context with its percentage, and holds the absolute count longer as the window fills', () => {
    const context = (used: number): StatusField => statusFields({ ...base, context: { used, window: 100_000 } }, copy)[1]!
    expect(context(40_000).forms.map(text)).toEqual(['ctx ~40% (40k/100k)', 'ctx ~40%'])
    expect(context(40_000).yields).toEqual([RANK.contextAbsolute])
    expect(context(75_000).yields).toEqual([RANK.contextAbsoluteWarm])
    expect(context(95_000).yields).toEqual([RANK.contextAbsoluteFull])
    // The reading carries the ramp's tone; its label never does.
    const hot = context(95_000).forms[0]!
    expect(hot).toEqual([{ text: 'ctx ', dim: true }, { text: '~95% (95k/100k)', color: CONTEXT_RAMP[3] }])
  })

  it('marks automatic compaction in full, then short, then not at all, and says when it is next', () => {
    const marked = statusFields({ ...base, context: { used: 62_000, window: 100_000, compactAt: 80_000 } }, copy)[1]!
    expect(marked.forms.map(text)).toEqual([
      'ctx ~62% (62k/100k) · compacts at 80%', 'ctx ~62% · compacts at 80%', 'ctx ~62% ▸80%', 'ctx ~62%'])
    expect(marked.yields).toEqual([RANK.contextAbsolute, RANK.contextMark, RANK.contextMarkDrop])
    // The mark is dim, and the ramp turns orange ten points below it.
    expect(marked.forms[0]!.at(-1)).toEqual({ text: ' · compacts at 80%', dim: true })
    expect(marked.forms[0]![1]!.color).toBe(CONTEXT_RAMP[1])
    // Filling, the absolute count outlasts the mark.
    const warm = statusFields({ ...base, context: { used: 75_000, window: 100_000, compactAt: 80_000 } }, copy)[1]!
    expect(warm.forms.map(text)).toEqual([
      'ctx ~75% (75k/100k) · compacts at 80%', 'ctx ~75% (75k/100k) ▸80%', 'ctx ~75% (75k/100k)', 'ctx ~75%'])
    expect(warm.forms[0]![1]!.color).toBe(CONTEXT_RAMP[2])
    const due = statusFields({ ...base, context: { used: 81_000, window: 100_000, compactAt: 80_000 } }, copy)[1]!
    expect(text(due.forms[0]!)).toBe('ctx ~81% (81k/100k) · compacts next')
  })

  it('names an update, and leaves out what the session has not reported', () => {
    expect(statusFields(base, copy).map(field => text(field.forms[0]!))).toEqual(['deepseek-v4-flash', '~/bake'])
    const fields = statusFields({ ...base, update: { version: '0.2.0', installed: true } }, copy)
    expect(fields.map(field => text(field.forms[0]!))).toEqual(['deepseek-v4-flash', 'update v0.2.0 · restart to use', '~/bake'])
    expect(statusFields({ ...base, cwd: '' }, copy)).toHaveLength(1)
  })
})

describe('fitStatus', () => {
  const field = (widths: readonly string[], yields: readonly number[]): StatusField =>
    ({ forms: widths.map(form => [{ text: form }]), yields })

  it('keeps every field whole while they fit, two cells apart', () => {
    const fields = [field(['aaaa'], []), field(['bb', 'b'], [2])]
    expect(fitStatus(fields, 8).map(fit => fit.width)).toEqual([4, 2])
    expect(row(fields, 8)).toBe('aaaa  bb')
  })

  it('gives way rank by rank, lowest first, whatever the display order, and stops once the row fits', () => {
    const fields = [field(['first-long', 'first'], [3]), field(['second-long', 'second'], [1]), field(['third'], [2])]
    expect(row(fields, 40)).toBe('first-long  second-long  third')
    expect(row(fields, 28)).toBe('first-long  second  third')
    expect(row(fields, 23)).toBe('first-long  second')
    expect(row(fields, 13)).toBe('first  second')
    // Past every rank the row is as narrow as it gets; the renderer clips it.
    expect(row(fields, 6)).toBe('first  second')
  })

  it('cuts a shrinking field from its end, no further than its floor until nothing else can go', () => {
    const fields: StatusField[] = [
      { forms: [[{ text: 'deepseek-v4-flash' }]], yields: [], shrink: { kind: 'end', rank: RANK.model, min: MODEL_MIN } },
      field(['ctx ~11%'], []),
    ]
    expect(fitStatus(fields, 30)[0]).toEqual({ parts: [{ text: 'deepseek-v4-flash' }], width: 17 })
    expect(fitStatus(fields, 22)[0]).toMatchObject({ width: 12, cut: 'end' })
    expect(fitStatus(fields, 18)[0]).toMatchObject({ width: MODEL_MIN, cut: 'end' })
    // The reading that never yields keeps its cells; the model gives up the rest.
    expect(fitStatus(fields, 14)[0]).toMatchObject({ width: 4, cut: 'end' })
    expect(fitStatus(fields, 10).map(fit => text(fit.parts))).toEqual(['ctx ~11%'])
  })

  it('keeps a few cells for the filler until its rank, and gives it whatever is left, cut from its start', () => {
    const path: StatusField = { forms: [[{ text: '~/projects/bake' }]], yields: [], shrink: { kind: 'fill', rank: RANK.cwd, min: CWD_MIN } }
    const fields = [field(['model'], []), field(['in 1k  cache hit 9%', 'cache hit 9%'], [RANK.totals]), field(['⎇ main +1', '⎇ main'], [RANK.gitCounts]), path]
    expect(row(fields, 60)).toBe('model  in 1k  cache hit 9%  ⎇ main +1  ~/projects/bake')
    // Nothing ranked gives way while the filler has its few cells: it is cut instead.
    expect(row(fields, 50)).toBe('model  in 1k  cache hit 9%  ⎇ main +1  …jects/bake')
    expect(fitStatus(fields, 50).at(-1)).toMatchObject({ width: 11, cut: 'start' })
    // Ranked before the filler, the totals give way to keep them.
    expect(row(fields, 44)).toBe('model  cache hit 9%  ⎇ main +1  …ojects/bake')
    // After its rank, nothing gives way for it; a tail shorter than its floor names nothing, so it is left out.
    expect(row(fields, 36)).toBe('model  cache hit 9%  ⎇ main +1')
    expect(row(fields, 29)).toBe('model  cache hit 9%  ⎇ main')
    // Something ranked later giving way frees cells the filler takes back.
    const update = [field(['model'], []), field(['update v0.2.0'], [RANK.update]), path]
    expect(row(update, 18)).toBe('model  …jects/bake')
    expect(fitStatus(update, 18).at(-1)).toMatchObject({ width: 11, cut: 'start' })
  })

  it('measures in terminal cells', () => {
    expect(formWidth([{ text: '上下文 ' }, { text: '~11%' }])).toBe(11)
    const fields = [field(['上下文 ~11%'], []), field(['思考 high'], [1])]
    expect(row(fields, 20)).toBe('上下文 ~11%')
  })
})
