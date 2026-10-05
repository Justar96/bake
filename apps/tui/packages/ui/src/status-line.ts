/**
 * The status line's fields, and the order they give way in as the row narrows.
 *
 * Pure. What the session reports in, fields out, and a fit of those fields
 * to a width, so every narrowing rule is testable without rendering.
 *
 * The row reads left to right as `deepseek-v4-flash  think high  ctx ~11%
 * (15.2k/128k)  ⎇ main +2 ~3  in 42.3k  out 3.1k  cache hit 81%  ~/bake`:
 * lowercase labels without colons, each a dim word beside its value. It is one
 * layout in every mode. Narrowing never reorders it; each field keeps its
 * place and gives way at its own rank ({@link RANK}), whole or to a shorter
 * complete reading, never cut mid-number.
 *
 * @module bake-tui-ui/status-line
 */

import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { cacheHit, compactPercent, contextPercent, formatContext, formatTotals, type ContextUsage, type TokenTotals } from './format.ts'
import { gitField, type GitState } from './git.ts'
import { cacheTone, CONTEXT_FULL, CONTEXT_WARN, contextTone, PALETTE, thinkingTone, type PaletteColor } from './palette.ts'
import { compactModel } from './present.ts'

/** One run of a status field, in its own tone. */
export interface StatusPart {
  readonly text: string
  /** Semantic tone; absent, the part is in the normal foreground unless `dim`. */
  readonly color?: PaletteColor | undefined
  /** Supporting text, drawn dim. */
  readonly dim?: boolean
}

/** One complete reading of a field, as its runs. */
export type StatusForm = readonly StatusPart[]

/**
 * One status-line field, and how it gives way.
 *
 * `forms` run from the widest reading to the narrowest, each complete.
 * `yields[i]` is the {@link RANK} at which form `i` gives way, to form
 * `i + 1`, or past the last form to nothing; a form with no rank never
 * yields. A field that `shrink`s is cut instead: from its end, at its rank,
 * down to `min` cells (the model); or, as the row's filler, from its start
 * into whatever the other fields leave, drawn while at least `min` cells of
 * it fit, the fields ranked before it giving way to keep those cells and
 * the ones ranked after it not (the working directory).
 */
export interface StatusField {
  readonly forms: readonly StatusForm[]
  readonly yields: readonly number[]
  readonly shrink?: { readonly kind: 'end' | 'fill', readonly rank: number, readonly min: number }
}

/**
 * The order fields give way in, first to yield first.
 *
 * The context reading's absolute count, then the in and out totals, which
 * leave the cache hit behind, and the compaction mark's words, which leave
 * `▸80%`, all to keep a few cells for the working directory; then the
 * working directory, which always takes only what the others leave; the
 * update notice; the git counts, then the cache hit, the compaction mark, the
 * branch, and the thinking level. The model is cut last, to
 * {@link MODEL_MIN} cells. `ctx ~N%` never yields. From
 * {@link CONTEXT_WARN} the absolute count holds until the thinking level has
 * gone, and from {@link CONTEXT_FULL} it takes cells from the model.
 */
export const RANK = {
  contextAbsolute: 1,
  totals: 2,
  contextMark: 2.5,
  cwd: 3,
  update: 4,
  gitCounts: 5,
  cacheHit: 6,
  contextMarkDrop: 6.5,
  branch: 7,
  thinking: 8,
  contextAbsoluteWarm: 8.5,
  model: 9,
  contextAbsoluteFull: 9.5,
} as const

/** Fewest cells the model is cut to. Fewer name no model. */
export const MODEL_MIN = 8

/** Fewest cells of the working directory worth drawing: a shorter tail such as `…ake` names nothing. */
export const CWD_MIN = 6

/** Two spaces between fields. A separator glyph would be another width to measure. */
export const FIELD_GAP = 2

/** The compaction mark's short form: `▸80%`. */
const MARK = '\u25b8'

/** What the status line reports, as the application supplies it. */
export interface StatusInput {
  /**
   * `provider/model`, or a bare model name; the provider is left out.
   * Absent, no model is selected yet, and the row says how to get one.
   */
  readonly model?: string | undefined
  readonly thinkingLevel?: string | undefined
  readonly context?: ContextUsage | undefined
  readonly git?: GitState | undefined
  readonly usage?: TokenTotals | undefined
  readonly update?: { readonly version: string, readonly installed: boolean } | undefined
  /** The working directory as it is to be read, already shortened against home by the application. */
  readonly cwd: string
  /** `ascii` where the terminal draws the classic frame. */
  readonly glyphs: 'unicode' | 'ascii'
}

/**
 * The status line's fields, in display order.
 * @param input - the session's readings.
 * @param copy - locale-owned labels.
 * @returns the fields, each with the ranks it gives way at.
 */
export function statusFields(input: StatusInput, copy: TuiCopy): readonly StatusField[] {
  const hit = input.usage === undefined ? undefined : cacheHit(input.usage)
  const fields: StatusField[] = [
    // The model leads because it is what the row exists to say; it needs no label.
    input.model === undefined
      // Nothing is selected until a sign-in: the way to start stays until the model's own rank.
      ? { forms: [[{ text: copy.noModel, color: PALETTE.waiting }, { text: `  ${copy.noModelHint}`, dim: true }],
        [{ text: copy.noModel, color: PALETTE.waiting }]], yields: [RANK.model] }
      : { forms: [[{ text: compactModel(input.model) }]], yields: [], shrink: { kind: 'end' as const, rank: RANK.model, min: MODEL_MIN } },
    ...input.thinkingLevel === undefined ? [] : [{
      forms: [[{ text: `${copy.think} `, dim: true }, { text: input.thinkingLevel, ...thinkingTone(input.thinkingLevel) }]], yields: [RANK.thinking],
    }],
    ...input.context === undefined ? [] : [contextField(input.context, copy)],
    // The branch and its changes. It narrows to the branch alone before it
    // gives way, and it outlasts the cost readings.
    ...input.git === undefined ? [] : [gitField(input.git, input.glyphs)],
    ...input.usage === undefined ? [] : [tokensField(input.usage, hit, copy)],
    // Last of the bounded fields: it drops before any reading of the session.
    ...input.update === undefined ? [] : [{
      forms: [[{ text: `${copy.updateLabel} `, dim: true },
        { text: `v${input.update.version} \u00b7 ${input.update.installed ? copy.updateRestart : '/update'}`, color: PALETTE.waiting }]],
      yields: [RANK.update],
    }],
    ...input.cwd === '' ? [] : [{ forms: [[{ text: input.cwd, dim: true }]], yields: [], shrink: { kind: 'fill' as const, rank: RANK.cwd, min: CWD_MIN } }],
  ]
  return fields
}

/**
 * The context reading: `ctx ~62% (79k/128k) · compacts at 80%`.
 *
 * The percentage leads, since it is what a user compacts on, and it never
 * yields. The absolute count in brackets goes first, except from
 * {@link CONTEXT_WARN}, where it is among the last. The compaction mark,
 * when the route's threshold is known, narrows to `▸80%` and then goes.
 * Only the reading carries the ramp's colour; its label and the mark are dim.
 *
 * @param usage - the occupancy, and the compaction threshold when known.
 * @param copy - locale-owned labels.
 * @returns the field and its narrower readings.
 */
function contextField(usage: ContextUsage, copy: TuiCopy): StatusField {
  const percent = contextPercent(usage)
  const at = compactPercent(usage)
  const color = contextTone(percent, at)
  const label: StatusPart = { text: `${copy.context} `, dim: true }
  const reading = (absolute: boolean): StatusPart =>
    ({ text: `~${percent}%${absolute ? ` (${formatContext(usage)})` : ''}`, color })
  const absoluteRank = percent >= CONTEXT_FULL ? RANK.contextAbsoluteFull : percent >= CONTEXT_WARN ? RANK.contextAbsoluteWarm : RANK.contextAbsolute
  if (at === undefined) {
    return { forms: [[label, reading(true)], [label, reading(false)]], yields: [absoluteRank] }
  }
  // At or past the threshold, the next request compacts first.
  const due = usage.compactAt !== undefined && usage.used >= usage.compactAt
  const marks: readonly (StatusPart | undefined)[] = [
    { text: ` \u00b7 ${due ? copy.contextCompactsNext : `${copy.contextCompactsAt} ${at}%`}`, dim: true },
    { text: ` ${MARK}${at}%`, dim: true },
    undefined,
  ]
  // The absolute count and the mark give way independently; the readings
  // follow their ranks in order, one change at a time.
  const changes = [
    { rank: absoluteRank, absolute: true },
    { rank: RANK.contextMark, absolute: false },
    { rank: RANK.contextMarkDrop, absolute: false },
  ].sort((a, b) => a.rank - b.rank)
  let absolute = true
  let mark = 0
  const forms: StatusForm[] = [[label, reading(true), marks[0]!]]
  for (const change of changes) {
    if (change.absolute) absolute = false
    else mark++
    const shown = marks[mark]
    forms.push([label, reading(absolute), ...shown === undefined ? [] : [shown]])
  }
  return { forms, yields: changes.map(change => change.rank) }
}

/**
 * The billed totals and the cache hit, one field: `in 42.3k  out 3.1k  cache hit 81%`.
 *
 * The totals are what the session cost; the cache hit is whether the prompt
 * cache still works, which a user can act on. So the field narrows to the
 * cache hit, and only that goes later. A provider that reports no cache
 * traffic has no cache hit, and its totals go whole.
 *
 * @param totals - the provider-reported totals.
 * @param hit - the cache hit, when the provider reports cache traffic.
 * @param copy - locale-owned labels.
 * @returns the field.
 */
function tokensField(totals: TokenTotals, hit: number | undefined, copy: TuiCopy): StatusField {
  const sums: StatusPart = { text: formatTotals(totals, { input: copy.tokensIn, output: copy.tokensOut }).join('  '), dim: true }
  if (hit === undefined) return { forms: [[sums]], yields: [RANK.totals] }
  const cache: StatusForm = [{ text: `${copy.cacheHit} `, dim: true }, { text: `${hit}%`, color: cacheTone(hit) }]
  return { forms: [[{ ...sums, text: `${sums.text}  ` }, ...cache], cache], yields: [RANK.totals, RANK.cacheHit] }
}

/** A field as it is drawn: the reading kept, its cells, and how it is cut when it is. */
export interface FittedField {
  readonly parts: StatusForm
  readonly width: number
  /** Where the reading is cut to `width`, when it is: its end for the model, its start for the filler. */
  readonly cut?: 'end' | 'start'
}

/** A reading's cells. */
export const formWidth = (form: StatusForm): number => stringWidth(form.map(part => part.text).join(''))

/**
 * Fit the fields to `room` cells, giving way in rank order until they fit.
 *
 * Fields keep their order. At each rank, while the row is too wide, every
 * field that yields there moves to its next reading or goes. The filler
 * always takes only what the others leave, cut from its start and drawn
 * while at least its minimum fits; until its own rank the fields ranked
 * before it give way to keep that minimum for it. The field cut from its end
 * counts at its minimum after its rank, and gets back whatever is left.
 * Below the widths every rank leaves, the row is clipped by the caller rather
 * than wrapped.
 *
 * @param fields - the fields, in display order.
 * @param room - cells available.
 * @returns the fields to draw, in order, each with its width.
 */
export function fitStatus(fields: readonly StatusField[], room: number): readonly FittedField[] {
  const form = fields.map(() => 0)
  const shrunk = fields.map(() => false)
  const shown = (index: number): boolean => form[index]! < fields[index]!.forms.length
  const full = (index: number): number => formWidth(fields[index]!.forms[form[index]!]!)
  const filler = (index: number): boolean => fields[index]!.shrink?.kind === 'fill'
  // Cells a field is counted at while the ranks are applied: the filler its
  // minimum until its rank and nothing after it, a field cut from its end its
  // minimum after its rank, and every other field its reading.
  const demand = (index: number): number => {
    const shrink = fields[index]!.shrink
    if (shrink === undefined) return full(index)
    if (shrink.kind === 'fill') return shrunk[index] ? 0 : Math.min(full(index), shrink.min)
    return shrunk[index] ? Math.min(full(index), shrink.min) : full(index)
  }
  const counted = (index: number): boolean => shown(index) && demand(index) > 0
  const total = (): number => {
    let used = 0
    let count = 0
    fields.forEach((_, index) => {
      if (!counted(index)) return
      used += demand(index)
      count++
    })
    return used + Math.max(0, count - 1) * FIELD_GAP
  }
  const ranks = [...new Set(fields.flatMap(field => [...field.yields, ...field.shrink === undefined ? [] : [field.shrink.rank]]))]
    .sort((a, b) => a - b)
  for (const rank of ranks) {
    if (total() <= room) break
    fields.forEach((field, index) => {
      if (!shown(index)) return
      if (field.yields[form[index]!] === rank) form[index]!++
      else if (field.shrink?.rank === rank && form[index] === field.forms.length - 1) shrunk[index] = true
    })
  }
  // Lay out every field but the filler at its reading, give the one cut from
  // its end back what is left, then put the filler in the rest.
  const placed = (index: number): boolean => shown(index) && !filler(index) && demand(index) > 0
  const count = fields.filter((_, index) => placed(index)).length
  const fixed = fields.reduce((sum, _, index) => sum + (placed(index) ? demand(index) : 0), 0) + Math.max(0, count - 1) * FIELD_GAP
  const widths = fields.map((field, index) => !placed(index) ? 0
    : shrunk[index] && field.shrink?.kind === 'end' ? Math.max(0, Math.min(full(index), demand(index) + room - fixed)) : demand(index))
  const used = widths.reduce((sum, width) => sum + width, 0) + Math.max(0, count - 1) * FIELD_GAP
  const fitted: FittedField[] = []
  fields.forEach((field, index) => {
    if (!shown(index)) return
    const parts = field.forms[form[index]!]!
    if (filler(index)) {
      const left = room - used - (used > 0 ? FIELD_GAP : 0)
      if (left >= Math.min(field.shrink!.min, full(index))) {
        fitted.push({ parts, width: Math.min(left, full(index)), ...left < full(index) ? { cut: 'start' as const } : {} })
      }
      return
    }
    const width = widths[index]!
    if (width <= 0) return
    fitted.push({ parts, width, ...width < full(index) ? { cut: 'end' as const } : {} })
  })
  return fitted
}
