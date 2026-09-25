/**
 * What a picker shows for a query. Which choices match, where in each label,
 * and which of them fit the rows it has.
 *
 * Pure, so the picker's behaviour is testable without a terminal, and the
 * same rules can serve any list a user filters by typing.
 *
 * @module @dsh-tui/ui/choices
 */

/** The text of a choice that a query is matched against. */
export interface Searchable {
  readonly value: string
  readonly label: string
  readonly description?: string
}

/** A choice that matched, and the half-open ranges of its label to emphasise. */
export interface Match<T extends Searchable> {
  readonly choice: T
  readonly ranges: readonly (readonly [number, number])[]
}

/**
 * Filter choices by every word of a query, in any order.
 *
 * A word matches anywhere in the label, the value, or the description, case
 * folded. `v4 flash` finds `deepseek/deepseek-v4-flash`, and an id typed in
 * full finds its session. The order is the caller's and is never re-ranked.
 * A list sorted newest first stays that way while it narrows, so the row the
 * user was reaching for does not jump.
 *
 * @param choices - candidates in display order.
 * @param query - what the user typed.
 * @returns the matches, with the label ranges each word matched.
 */
export function filterChoices<T extends Searchable>(choices: readonly T[], query: string): readonly Match<T>[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(word => word !== '')
  return choices.flatMap(choice => {
    const label = choice.label.toLowerCase()
    const haystack = `${label}\n${choice.value.toLowerCase()}\n${(choice.description ?? '').toLowerCase()}`
    if (!words.every(word => haystack.includes(word))) return []
    const ranges = words.flatMap(word => {
      const at = label.indexOf(word)
      return at < 0 ? [] : [[at, at + word.length] as const]
    })
    return [{ choice, ranges: merged(ranges) }]
  })
}

/**
 * Overlapping or touching ranges as one, in order.
 * @param ranges - half-open ranges in any order.
 * @returns disjoint ranges, sorted.
 */
function merged(ranges: readonly (readonly [number, number])[]): readonly (readonly [number, number])[] {
  const sorted = [...ranges].sort((left, right) => left[0] - right[0])
  const out: [number, number][] = []
  for (const [start, end] of sorted) {
    const last = out.at(-1)
    if (last !== undefined && start <= last[1]) last[1] = Math.max(last[1], end)
    else out.push([start, end])
  }
  return out
}

/** The rows of a scrolled list. The first item shown, how many, and what each edge hides. */
export interface Scroll {
  readonly top: number
  readonly count: number
  /** Items above the first shown; drawn as a row when non-zero. */
  readonly above: number
  /** Items below the last shown; drawn as a row when non-zero. */
  readonly below: number
}

/**
 * Scroll a list so the selection stays in view, moving as little as possible.
 *
 * The list scrolls only when the selection would leave it, so moving up and
 * down inside the window leaves every row where it is. An edge that hides
 * items spends one of the rows saying how many, so the user knows the list
 * continues. At two rows or fewer there is no room for that, and only the
 * selection is kept.
 *
 * @param previous - the top the list was last drawn from.
 * @param selected - the selected index, or -1 for none.
 * @param total - items in the list.
 * @param rows - rows the list may use, edge rows included.
 * @returns what to draw.
 */
export function scrollTo(previous: number, selected: number, total: number, rows: number): Scroll {
  const room = Math.max(1, rows)
  if (total <= room) return { top: 0, count: total, above: 0, below: 0 }
  if (room <= 2) {
    const top = Math.min(Math.max(0, selected), total - room)
    return { top, count: room, above: 0, below: 0 }
  }
  // Past `last`, the rest of the list fits under an "above" row and needs no
  // "below" row. Before it, a window away from the top has both.
  const last = total - room + 1
  const countAt = (top: number): number =>
    top >= last ? total - top : top === 0 ? room - 1 : room - 2
  let top = Math.min(Math.max(0, previous), last)
  if (selected >= 0 && selected < top) top = selected
  else if (selected >= top + countAt(top)) top = Math.min(last, Math.max(0, selected - (room - 2) + 1))
  const count = countAt(top)
  return { top, count, above: top, below: total - top - count }
}
