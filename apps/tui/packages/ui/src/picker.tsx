/**
 * Filterable terminal choices. The application owns acceptance and cancellation.
 *
 * Every selection the application asks for — a model, a reasoning effort, a
 * session, a sign-in target — is this one panel, so one set of keys and one
 * layout serve them all. Top to bottom, in the order it is used. What is
 * being chosen, the filter being typed, the choices under any group
 * headings, the selected choice's levels where the prompt has them, what the
 * selected choice is for where the choices say, and the keys.
 *
 * @module @dsh-tui/ui/picker
 */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste, useWindowSize } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import type { TuiCopy } from './copy.ts'
import { filterChoices, scrollTo, type Match } from './choices.ts'
import { MARKER } from './layout.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { caretCell } from './caret.ts'
import { composerText, eraseLast } from './editor.ts'

/** A choice's standing, drawn in its own aligned column. `Current`, `Configured`. */
export interface ChoiceStatus {
  readonly text: string
  /** Tone of the text; absent, it is dim. */
  readonly tone?: 'done' | 'waiting' | 'failed'
}

/** One step of a choice's levels. */
export interface ChoiceLevel {
  readonly value: string
  readonly label: string
  /** The level's tone, such as a reasoning effort's; absent, the picker's own. */
  readonly color?: PaletteColor
}

/** Values ←→ steps through for one choice, such as a model's reasoning efforts. */
export interface ChoiceLevels {
  /** In the order they are drawn and stepped. */
  readonly items: readonly ChoiceLevel[]
  /** The level shown until the user steps to one this choice also offers. */
  readonly initial: string
}

/** A provider-owned value with its display metadata. */
export interface Choice {
  readonly value: string
  readonly label: string
  /** Secondary text, dim beside the label. */
  readonly description?: string
  /**
   * The heading of the run of choices this one belongs to. A heading is drawn
   * once above each run, and the filter keeps the runs it matches.
   */
  readonly group?: string
  /** Short facts in aligned columns after the label, such as `272k`; an empty string leaves its column blank. */
  readonly facts?: readonly string[]
  /** What the prompt's levels row offers while this choice is selected. */
  readonly levels?: ChoiceLevels
  /** The value already in force; drawn as a `Current` status unless `status` says otherwise. */
  readonly current?: boolean
  readonly status?: ChoiceStatus
  /**
   * Kept on screen below the scrolled list, whatever the filter scrolls past.
   * An action such as starting a new session must not be hidden by a long
   * history. It still has to match the filter to be shown.
   */
  readonly pinned?: boolean
  /**
   * Listed only while the filter has text. A panel of sections can offer
   * every setting inside them to a search without listing them all at once.
   */
  readonly searchOnly?: boolean
  /** Session-only presentation role; the selected value remains the session id. */
  readonly role?: 'session-new' | 'session-current' | 'session-saved'
  /** A glyph in the rail before the label, such as a setting's changed mark. Drawn where the prompt has `marks`. */
  readonly mark?: ChoiceMark
  /** What the choice is for, drawn under the list while it is selected. */
  readonly detail?: string
}

/** A choice's glyph in the rail before its label. */
export interface ChoiceMark {
  readonly glyph: string
  /** Its colour; absent, the picker's own. */
  readonly tone?: 'done' | 'waiting' | 'failed'
}

/** One page beside the prompt's own, reached with Tab. */
export interface ChoiceTab {
  readonly value: string
  readonly label: string
}

/**
 * Pages beside the prompt's own, drawn as a row under the title. Tab and
 * Shift-Tab answer the next or previous tab's value, wrapping; the
 * application opens that page.
 */
export interface ChoiceTabs {
  readonly items: readonly ChoiceTab[]
  /** The prompt's own page; absent, Tab answers the first and Shift-Tab the last. */
  readonly active?: string
}

/** Application-owned choices and the initial cursor position. */
export interface ChoicePrompt {
  readonly title: string
  readonly choices: readonly Choice[]
  readonly initial: string
  readonly warning?: string
  readonly tabs?: ChoiceTabs
  /** Alignment of each facts column, in order; absent, left. */
  readonly factAlign?: readonly ('left' | 'right')[]
  /**
   * A row under the list naming the selected choice's levels, which ←→ step
   * through; Enter answers the choice with its level. `none` is what the row
   * says for a choice without levels.
   */
  readonly levels?: { readonly label: string; readonly none: string }
  /** The key line, in place of the default one. */
  readonly help?: string
  /** Take up to half the screen, for a list worth scanning, instead of a menu's rows. */
  readonly tall?: boolean
  /**
   * Keep a rail before the labels for the choices' marks, even while none
   * has one, so a mark appearing after a change does not shift every row.
   */
  readonly marks?: boolean
}

/**
 * The tab Tab or Shift-Tab answers.
 * @param tabs - the prompt's tabs.
 * @param back - Shift-Tab.
 * @returns the neighbour's value, or undefined without tabs.
 */
export function stepTab(tabs: ChoiceTabs | undefined, back: boolean): string | undefined {
  const items = tabs?.items ?? []
  if (items.length === 0) return undefined
  const at = items.findIndex(item => item.value === tabs?.active)
  const next = at === -1 ? (back ? items.length - 1 : 0) : (at + (back ? -1 : 1) + items.length) % items.length
  return items[next]!.value
}

/** Panel border and padding, in columns. */
const FRAME = 4
/** Gap between columns. */
const GAP = 2
/** Widest share of the row the label column takes, so descriptions keep room. */
const LABEL_SHARE = 0.55
/** Columns a label keeps before facts give way, when it is that wide. */
const LABEL_FLOOR = 20

const TONES: Readonly<Record<NonNullable<ChoiceStatus['tone']>, PaletteColor>> = {
  done: PALETTE.done,
  waiting: PALETTE.waiting,
  failed: PALETTE.failed,
}

/**
 * Filter and choose a value without submitting text to the conversation.
 *
 * Typing filters by every word, anywhere in a choice, and the matched letters
 * are underlined. ↑↓, or Ctrl-P and Ctrl-N, move one row and wrap. PgUp and
 * PgDn move a page. Home and End jump to either end. The list scrolls only
 * when the selection would leave it, and an edge that hides choices says how
 * many. Choices marked `pinned` stay below the list however far it scrolls.
 * Choices marked `searchOnly` are listed only once the filter has text.
 * With `tabs`, Tab and Shift-Tab answer a neighbouring tab's value.
 *
 * Choices with a `group` are drawn under its heading, one per run. Scrolled
 * past the heading, the top edge names the group instead, so a row never
 * loses what it belongs to. With `levels`, ←→ step the selected choice's
 * level, and a level stepped to stays chosen on every other choice that
 * offers it. Facts give way from the right on a narrow terminal before a
 * label is cut too short to tell apart.
 *
 * When any choice has a `detail`, the rows under the list say what the
 * selected one is for: two where the list leaves room for them, one where it
 * would otherwise scroll, and blank for a choice without one, so moving the
 * selection never moves the keys.
 *
 * @param props - choices, localized labels, row limit, and the acceptance callback.
 * @param props.limit - rows for the choices, including their headings, the
 *   levels row, and the rows that say how many are hidden. The title, filter,
 *   and key rows are outside it.
 * @returns a bounded keyboard picker. Escape remains owned by the application.
 */
export function Picker({ prompt, copy, limit, onSelect }: {
  readonly prompt: ChoicePrompt
  readonly copy: TuiCopy
  readonly limit: number
  /** The chosen value, and its level when the prompt has levels and the choice offers any. */
  readonly onSelect: (value: string, level?: string) => void
}): React.ReactElement {
  const { columns } = useWindowSize()
  const [selected, setSelected] = useState(prompt.initial)
  const cursor = useRef(selected)
  const completed = useRef(false)
  const [query, setQuery] = useState('')
  const draft = useRef(query)
  const top = useRef(0)
  // The level last stepped to; a choice that offers it shows it over its own initial.
  const [, setStepped] = useState<string | undefined>(undefined)
  const stepped = useRef<string | undefined>(undefined)
  const levelOf = (choice: Choice | undefined): string | undefined => {
    const items = prompt.levels === undefined ? undefined : choice?.levels?.items
    if (items === undefined || items.length === 0) return undefined
    const wanted = stepped.current !== undefined && items.some(item => item.value === stepped.current) ? stepped.current : choice!.levels!.initial
    return items.some(item => item.value === wanted) ? wanted : items[0]!.value
  }
  // Display order. The scrolled list, then the pinned choices under it.
  const ordered = (text: string): readonly Match<Choice>[] => {
    const matches = filterChoices(visible(prompt.choices, text), text)
    return [...matches.filter(match => match.choice.pinned !== true), ...matches.filter(match => match.choice.pinned === true)]
  }
  const indexOf = (matches: readonly Match<Choice>[]) => Math.max(0, matches.findIndex(match => match.choice.value === cursor.current))
  const move = (to: (index: number, count: number) => number): void => {
    const matches = ordered(draft.current)
    if (matches.length === 0) return
    cursor.current = matches[Math.max(0, Math.min(matches.length - 1, to(indexOf(matches), matches.length)))]!.choice.value
    setSelected(cursor.current)
  }
  const accept = (text: string): void => {
    const matches = ordered(text)
    const match = matches[indexOf(matches)]
    if (completed.current || match === undefined) return
    completed.current = true
    const level = levelOf(match.choice)
    if (level === undefined) onSelect(match.choice.value)
    else onSelect(match.choice.value, level)
  }
  // Levels stop at either end, as a slider does, rather than wrapping.
  const step = (by: number): void => {
    const matches = ordered(draft.current)
    const choice = matches[indexOf(matches)]?.choice
    const items = choice?.levels?.items
    const at = levelOf(choice)
    if (items === undefined || at === undefined) return
    const next = items[Math.max(0, Math.min(items.length - 1, items.findIndex(item => item.value === at) + by))]!.value
    stepped.current = next
    setStepped(next)
  }
  const update = (value: string): void => { draft.current = value; setQuery(value) }

  const matches = ordered(query)
  const pinned = matches.filter(match => match.choice.pinned === true)
  const listed = matches.length - pinned.length
  const selectedIndex = matches.length === 0 ? -1 : indexOf(matches)
  const page = Math.max(1, limit - pinned.length - 2)
  usePaste(text => update(draft.current + composerText(text)))
  useInput((text, key) => {
    if (key.escape || key.meta || completed.current) return
    if (key.ctrl) {
      if (text === 'p') move((index, count) => (index - 1 + count) % count)
      else if (text === 'n') move((index, count) => (index + 1) % count)
      return
    }
    if (key.upArrow) move((index, count) => (index - 1 + count) % count)
    else if (key.downArrow) move((index, count) => (index + 1) % count)
    else if (key.pageUp) move(index => index - page)
    else if (key.pageDown) move(index => index + page)
    else if (key.home) move(() => 0)
    else if (key.end) move((_, count) => count - 1)
    else if (key.leftArrow) step(-1)
    else if (key.rightArrow) step(1)
    else if (key.return) accept(draft.current)
    else if (key.tab) {
      const tab = stepTab(prompt.tabs, key.shift)
      if (tab === undefined || completed.current) return
      completed.current = true
      onSelect(tab)
    }
    else if (key.backspace || key.delete) update(eraseLast(draft.current))
    else {
      for (const [index, part] of composerText(text).split('\n').entries()) {
        if (index > 0) { accept(draft.current); if (completed.current) return }
        update(draft.current + part)
      }
    }
  })

  const leveled = prompt.levels !== undefined && prompt.choices.some(choice => (choice.levels?.items.length ?? 0) > 0)
  // Sized over the unfiltered list, so typing a filter never changes the panel's height.
  const unfiltered = visible(prompt.choices, '')
  const scrolled = unfiltered.filter(choice => choice.pinned !== true)
  const detailed = prompt.choices.some(choice => choice.detail !== undefined && choice.detail !== '')
  const spare = limit - (leveled ? 1 : 0) - (unfiltered.length - scrolled.length) - linesOf(filterChoices(scrolled, ''), scrolled.length).length
  const detailRows = !detailed ? 0 : spare >= 2 ? 2 : 1
  // Pinned rows, the levels row, and the detail rows come out of the list's budget, but the list keeps at least one row.
  const rows = Math.max(1, limit - pinned.length - (leveled ? 1 : 0) - detailRows)
  const lines = linesOf(matches, listed)
  const selectedLine = selectedIndex < listed ? lines.findIndex(line => line.kind === 'choice' && line.index === selectedIndex) : -1
  let scroll = scrollTo(top.current, selectedLine, lines.length, rows)
  // The first choice of a run brings its heading into view when both fit.
  if (selectedLine > 0 && lines[selectedLine - 1]!.kind === 'group' && scroll.top === selectedLine) {
    const lifted = scrollTo(selectedLine - 1, selectedLine - 1, lines.length, rows)
    if (selectedLine < lifted.top + lifted.count) scroll = lifted
  }
  top.current = scroll.top
  const windowed = lines.slice(scroll.top, scroll.top + scroll.count)
  const choicesIn = (part: readonly Line[]): number => part.filter(line => line.kind === 'choice').length
  const hiddenAbove = choicesIn(lines.slice(0, scroll.top))
  const hiddenBelow = choicesIn(lines.slice(scroll.top + scroll.count))
  // Scrolled into a run, the top edge carries the heading the window cut off.
  const carried = scroll.above > 0 && windowed[0]?.kind === 'choice' ? windowed[0].match.choice.group : undefined
  const shownPinned = pinned.slice(0, Math.max(0, limit - scroll.count - (scroll.above > 0 ? 1 : 0) - (scroll.below > 0 ? 1 : 0) - (leveled ? 1 : 0) - detailRows))
    .map((match, index) => ({ kind: 'choice' as const, match, index: listed + index }))
  // Size columns over every choice a filter may show, not only the visible
  // ones, so the columns stay put while the list scrolls or narrows.
  const sized = visible(prompt.choices, query)
  const glyphs = prompt.marks === true || sized.some(choice => choice.role !== undefined || choice.mark !== undefined)
  const statusWidth = Math.max(0, ...sized.map(choice => stringWidth(statusOf(choice, copy)?.text ?? '')))
  const widestLabel = Math.max(1, ...sized.map(choice => stringWidth(choice.label)))
  const roomFor = (facts: readonly number[]): number => Math.max(8, columns - FRAME - MARKER_WIDTH - (glyphs ? MARKER_WIDTH : 0)
    - (statusWidth > 0 ? statusWidth + GAP : 0) - facts.reduce((sum, width) => sum + (width > 0 ? width + GAP : 0), 0))
  // Facts give way from the right before a label is cut too short to tell apart.
  let factWidths = Array.from({ length: Math.max(0, ...sized.map(choice => choice.facts?.length ?? 0)) },
    (_, column) => Math.max(0, ...sized.map(choice => stringWidth(choice.facts?.[column] ?? ''))))
  while (factWidths.length > 0 && Math.floor(roomFor(factWidths) * LABEL_SHARE) < Math.min(widestLabel, LABEL_FLOOR)) {
    factWidths = factWidths.slice(0, -1)
  }
  const factsWidth = factWidths.reduce((sum, width) => sum + (width > 0 ? width + GAP : 0), 0)
  const room = roomFor(factWidths)
  const labelWidth = Math.min(Math.floor(room * LABEL_SHARE), widestLabel) + GAP
  const hasDescription = sized.some(choice => choice.description !== undefined && choice.description !== '')
  const columnsOf: Columns = { glyphs, labelWidth: hasDescription || statusWidth > 0 || factsWidth > 0 ? labelWidth : undefined,
    statusWidth, factWidths, factAlign: prompt.factAlign ?? [] }
  const selectedChoice = selectedIndex < 0 ? undefined : matches[selectedIndex]?.choice

  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting} wrap="truncate-end">{prompt.title}</Text>
    {prompt.tabs !== undefined && prompt.tabs.items.length > 0 && <Tabs tabs={prompt.tabs} />}
    {prompt.warning !== undefined && <Text color={PALETTE.waiting}>{prompt.warning}</Text>}
    <Text wrap="truncate-end">
      <Text bold color={PALETTE.asking}>{`${MARKER.prompt} `}</Text>
      {query}{caretCell('')}{query === '' && <Text dimColor>{copy.pickerFilter}</Text>}
    </Text>
    {scroll.above > 0 && <Edge arrow="↑" count={hiddenAbove} copy={copy} {...carried === undefined ? {} : { group: carried }} />}
    {[...windowed, ...shownPinned].map(line => line.kind === 'group'
      ? <Heading key={`group:${line.group}:${line.at}`} group={line.group} count={line.count} width={columns - FRAME} />
      : <Row key={line.match.choice.value} match={line.match} active={line.index === selectedIndex} copy={copy} columns={columnsOf} />)}
    {scroll.below > 0 && <Edge arrow="↓" count={hiddenBelow} copy={copy} />}
    {matches.length === 0 && <Text dimColor>{`${' '.repeat(MARKER_WIDTH)}${copy.noChoices}`}</Text>}
    {leveled && <Levels label={prompt.levels!.label} none={prompt.levels!.none} width={columns - FRAME}
      {...selectedChoice?.levels === undefined ? {} : { levels: selectedChoice.levels.items }}
      {...levelOf(selectedChoice) === undefined ? {} : { value: levelOf(selectedChoice)! }} />}
    {detailRows > 0 && <Box height={detailRows} flexDirection="column">
      {detailLines(selectedChoice?.detail ?? '', columns - FRAME - MARKER_WIDTH, detailRows).map((line, index) =>
        <Text key={index} dimColor wrap="truncate-end">{`${' '.repeat(MARKER_WIDTH)}${line}`}</Text>)}
    </Box>}
    <Box flexDirection="row">
      <Box flexGrow={1} flexShrink={1}><Text dimColor wrap="truncate-end">{prompt.help ?? (prompt.tabs === undefined ? copy.pickerHelp : copy.pickerTabsHelp)}</Text></Box>
      {matches.length > 0 && <Box flexShrink={0} marginLeft={GAP}><Text dimColor>{`${selectedIndex + 1}/${matches.length}`}</Text></Box>}
    </Box>
  </Box>
}

/**
 * A detail wrapped to the rows it has, each starting flush at the labels'
 * edge. The last row is cut where the text runs past it.
 */
function detailLines(text: string, width: number, rows: number): readonly string[] {
  if (text === '') return []
  const lines = wrapAnsi(text, Math.max(1, width), { hard: true, trim: true }).split('\n')
  return lines.length <= rows ? lines : [...lines.slice(0, rows - 1), lines.slice(rows - 1).join(' ')]
}

/**
 * The choices a filter may match: all of them once it has text, and only
 * those not kept for a search before.
 */
function visible(choices: readonly Choice[], query: string): readonly Choice[] {
  return query.trim() === '' ? choices.filter(choice => choice.searchOnly !== true) : choices
}

/** The tab row. The prompt's own page is drawn reversed; the others are dim. */
function Tabs({ tabs }: { readonly tabs: ChoiceTabs }): React.ReactElement {
  return <Text wrap="truncate-end">
    {tabs.items.map((item, index) => <React.Fragment key={item.value}>
      {index > 0 && ' '}
      {item.value === tabs.active
        ? <Text bold inverse color={PALETTE.asking}>{` ${item.label} `}</Text>
        : <Text dimColor>{` ${item.label} `}</Text>}
    </React.Fragment>)}
  </Text>
}

/** Columns of the pointer rail and the session glyph rail. */
const MARKER_WIDTH = 2

/** One row of the scrolled list: a run's heading, or a choice at its index among the matches. */
type Line =
  | { readonly kind: 'group'; readonly group: string; readonly count: number; readonly at: number }
  | { readonly kind: 'choice'; readonly match: Match<Choice>; readonly index: number }

/**
 * The scrolled list's rows: each listed choice, after its run's heading.
 * @param matches - matches in display order, the listed ones first.
 * @param listed - how many lead the matches before the pinned ones.
 * @returns headings and choices, top to bottom.
 */
function linesOf(matches: readonly Match<Choice>[], listed: number): readonly Line[] {
  const lines: Line[] = []
  for (let index = 0; index < listed; index++) {
    const match = matches[index]!
    const group = match.choice.group
    if (group !== undefined && (index === 0 || matches[index - 1]!.choice.group !== group)) {
      let end = index
      while (end < listed && matches[end]!.choice.group === group) end++
      lines.push({ kind: 'group', group, count: end - index, at: index })
    }
    lines.push({ kind: 'choice', match, index })
  }
  return lines
}

/**
 * A run's heading, with how many of its choices the filter kept, and a dim
 * rule to the panel's edge that sets the run apart without spending a row.
 */
function Heading({ group, count, width }: { readonly group: string, readonly count: number, readonly width: number }): React.ReactElement {
  const rule = Math.max(0, width - stringWidth(`${group} ${count} `))
  return <Text wrap="truncate-end"><Text bold>{group}</Text><Text dimColor>{` ${count} ${'\u2500'.repeat(rule)}`}</Text></Text>
}

/**
 * The selected choice's levels, the current one reversed as a tab is, each in
 * its own tone where it has one and the others dim. Where the whole row does
 * not fit, only the current level is named, between the arrows that can
 * still move it, with its place among them.
 */
function Levels({ label, none, levels, value, width }: {
  readonly label: string
  readonly none: string
  readonly levels?: readonly ChoiceLevel[]
  readonly value?: string
  readonly width: number
}): React.ReactElement {
  const lead = `${label}${' '.repeat(GAP)}`
  if (levels === undefined || levels.length === 0 || value === undefined) return <Text dimColor wrap="truncate-end">{`${lead}${none}`}</Text>
  const at = Math.max(0, levels.findIndex(level => level.value === value))
  const full = stringWidth(lead) + levels.reduce((sum, level) => sum + stringWidth(level.label) + 3, -1)
  if (full > width) {
    return <Text wrap="truncate-end">
      <Text dimColor>{`${lead}${at > 0 ? '‹ ' : '  '}`}</Text>
      <Text bold color={levels[at]!.color ?? PALETTE.asking}>{levels[at]!.label}</Text>
      <Text dimColor>{`${at < levels.length - 1 ? ' ›' : '  '}  ${at + 1}/${levels.length}`}</Text>
    </Text>
  }
  return <Text wrap="truncate-end">
    <Text dimColor>{lead}</Text>
    {levels.map((level, index) => <React.Fragment key={level.value}>
      {index > 0 && ' '}
      {index === at
        ? <Text bold inverse color={level.color ?? PALETTE.asking}>{` ${level.label} `}</Text>
        : <Text dimColor {...level.color === undefined ? {} : { color: level.color }}>{` ${level.label} `}</Text>}
    </React.Fragment>)}
  </Text>
}

/** How every row lays out its columns, sized over all the choices a filter may show. */
interface Columns {
  readonly glyphs: boolean
  /** Fixed label column; absent when nothing follows the label. */
  readonly labelWidth: number | undefined
  readonly statusWidth: number
  /** One width per facts column; a zero-width column is not drawn. */
  readonly factWidths: readonly number[]
  readonly factAlign: readonly ('left' | 'right')[]
}

/**
 * The status a choice shows, if any. Its own, or `Current` for the value in force.
 * @param choice - the choice.
 * @param copy - localized labels.
 * @returns the status to draw.
 */
function statusOf(choice: Choice, copy: TuiCopy): ChoiceStatus | undefined {
  if (choice.status !== undefined) return choice.status
  return choice.current === true || choice.role === 'session-current' ? { text: copy.currentSelection, tone: 'done' } : undefined
}

/**
 * The row an edge of a scrolled list spends saying how much it hides. At the
 * top, scrolled into a run, it leads with the run's heading.
 */
function Edge({ arrow, count, copy, group }: {
  readonly arrow: string
  readonly count: number
  readonly copy: TuiCopy
  readonly group?: string
}): React.ReactElement {
  const more = count === 0 ? '' : `${arrow} ${count} ${copy.pickerMore}`
  if (group === undefined) return <Text dimColor wrap="truncate-end">{`${' '.repeat(MARKER_WIDTH)}${more}`}</Text>
  return <Text wrap="truncate-end"><Text bold>{group}</Text><Text dimColor>{more === '' ? '' : `${' '.repeat(GAP)}${more}`}</Text></Text>
}

/**
 * One choice. The pointer, the session glyph, the label with its matched
 * letters underlined, the facts, the status, and the description.
 */
function Row({ match, active, copy, columns }: {
  readonly match: Match<Choice>
  readonly active: boolean
  readonly copy: TuiCopy
  readonly columns: Columns
}): React.ReactElement {
  const { glyphs, labelWidth, statusWidth, factWidths, factAlign } = columns
  const { choice, ranges } = match
  const status = statusOf(choice, copy)
  const role = choice.role
  const tone = active ? { color: PALETTE.asking } : {}
  return <Box flexDirection="row">
    <Box width={MARKER_WIDTH} flexShrink={0}>
      <Text bold color={PALETTE.asking}>{active ? MARKER.selected : MARKER.none}</Text>
    </Box>
    {glyphs && <Box width={MARKER_WIDTH} flexShrink={0}>
      {choice.mark !== undefined && role === undefined && <Text bold
        color={choice.mark.tone === undefined ? PALETTE.asking : TONES[choice.mark.tone]}>{choice.mark.glyph}</Text>}
      {role !== undefined && <Text
        {...role === 'session-saved' ? { dimColor: true } : { color: role === 'session-current' ? PALETTE.done : PALETTE.asking }}>
        {role === 'session-new' ? '+' : role === 'session-current' ? '●' : '○'}
      </Text>}
    </Box>}
    {/* A label cut short keeps the gap before the next column. */}
    <Box flexShrink={0} {...labelWidth === undefined ? { flexGrow: 1 } : { width: labelWidth, paddingRight: GAP }}>
      <Text bold={active} wrap="truncate-end" {...tone}>{spans(choice.label, ranges)}</Text>
    </Box>
    {factWidths.map((width, column) => {
      if (width === 0) return null
      const fact = choice.facts?.[column] ?? ''
      const padded = factAlign[column] === 'right' ? `${' '.repeat(Math.max(0, width - stringWidth(fact)))}${fact}` : fact
      return <Box key={column} width={width + GAP} flexShrink={0}><Text dimColor={!active} wrap="truncate-end">{padded}</Text></Box>
    })}
    {statusWidth > 0 && <Box width={statusWidth + GAP} flexShrink={0}>
      {status !== undefined && <Text wrap="truncate-end" {...status.tone === undefined ? { dimColor: true } : { color: TONES[status.tone] }}>{status.text}</Text>}
    </Box>}
    {choice.description !== undefined && choice.description !== '' && <Box flexGrow={1} flexShrink={1}>
      <Text dimColor={!active} wrap="truncate-end">{choice.description}</Text>
    </Box>}
  </Box>
}

/**
 * A label with its matched ranges underlined.
 * @param label - the label.
 * @param ranges - disjoint, sorted half-open ranges.
 * @returns the label as text runs.
 */
function spans(label: string, ranges: readonly (readonly [number, number])[]): React.ReactNode {
  if (ranges.length === 0) return label
  const parts: React.ReactNode[] = []
  let at = 0
  for (const [start, end] of ranges) {
    if (start > at) parts.push(label.slice(at, start))
    parts.push(<Text key={start} underline>{label.slice(start, end)}</Text>)
    at = end
  }
  if (at < label.length) parts.push(label.slice(at))
  return parts
}
