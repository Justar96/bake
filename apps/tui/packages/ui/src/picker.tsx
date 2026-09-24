/**
 * Filterable terminal choices; the application owns acceptance and cancellation.
 *
 * Every selection the application asks for — a model, a reasoning effort, a
 * session, a sign-in target — is this one panel, so one set of keys and one
 * layout serve them all. It reads top to bottom in the order it is used: what
 * is being chosen, the filter being typed, the choices, and the keys.
 *
 * @module @dsh-tui/ui/picker
 */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste, useWindowSize } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { filterChoices, scrollTo, type Match } from './choices.ts'
import { MARKER } from './layout.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { composerText, eraseLast } from './editor.ts'

/** A choice's standing, drawn in its own aligned column: `Current`, `Configured`. */
export interface ChoiceStatus {
  readonly text: string
  /** Tone of the text; absent, it is dim. */
  readonly tone?: 'done' | 'waiting' | 'failed'
}

/** A provider-owned value with its display metadata. */
export interface Choice {
  readonly value: string
  readonly label: string
  /** Secondary text, dim beside the label. */
  readonly description?: string
  /** The value already in force; drawn as a `Current` status unless `status` says otherwise. */
  readonly current?: boolean
  readonly status?: ChoiceStatus
  /**
   * Kept on screen below the scrolled list, whatever the filter scrolls past:
   * an action such as starting a new session, which a long history must not
   * hide. It still has to match the filter to be shown.
   */
  readonly pinned?: boolean
  /** Session-only presentation role; the selected value remains the session id. */
  readonly role?: 'session-new' | 'session-current' | 'session-saved'
}

/** Application-owned choices and the initial cursor position. */
export interface ChoicePrompt {
  readonly title: string
  readonly choices: readonly Choice[]
  readonly initial: string
  readonly warning?: string
}

/** Panel border and padding, in columns. */
const FRAME = 4
/** Gap between columns. */
const GAP = 2
/** Widest share of the row the label column takes, so descriptions keep room. */
const LABEL_SHARE = 0.55

const TONES: Readonly<Record<NonNullable<ChoiceStatus['tone']>, PaletteColor>> = {
  done: PALETTE.done,
  waiting: PALETTE.waiting,
  failed: PALETTE.failed,
}

/**
 * Filter and choose a value without submitting text to the conversation.
 *
 * Typing filters by every word, anywhere in a choice, and the matched letters
 * are underlined. ↑↓ (or Ctrl-P/Ctrl-N) move one row, wrapping; PgUp/PgDn move
 * a page; Home/End jump to either end. The list scrolls only when the selection
 * would leave it, and an edge that hides choices says how many. Choices marked
 * `pinned` stay below the list however far it scrolls.
 *
 * @param props - choices, localized labels, row limit, and explicit acceptance callback.
 * @param props.limit - rows for the choices, including the rows that say how
 *   many are hidden; the title, filter, and key rows are outside it.
 * @returns a bounded keyboard picker; Escape remains owned by the application.
 */
export function Picker({ prompt, copy, limit, onSelect }: {
  readonly prompt: ChoicePrompt
  readonly copy: TuiCopy
  readonly limit: number
  readonly onSelect: (value: string) => void
}): React.ReactElement {
  const { columns } = useWindowSize()
  const [selected, setSelected] = useState(prompt.initial)
  const cursor = useRef(selected)
  const completed = useRef(false)
  const [query, setQuery] = useState('')
  const draft = useRef(query)
  const top = useRef(0)
  // The display order: the scrolled list, then the pinned choices under it.
  const ordered = (text: string): readonly Match<Choice>[] => {
    const matches = filterChoices(prompt.choices, text)
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
    onSelect(match.choice.value)
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
    else if (key.return) accept(draft.current)
    else if (key.backspace || key.delete) update(eraseLast(draft.current))
    else if (!key.tab) {
      for (const [index, part] of composerText(text).split('\n').entries()) {
        if (index > 0) { accept(draft.current); if (completed.current) return }
        update(draft.current + part)
      }
    }
  })

  // Pinned rows come out of the list's rows, but the list keeps at least one.
  const rows = Math.max(1, limit - pinned.length)
  const scroll = scrollTo(top.current, selectedIndex < listed ? selectedIndex : -1, listed, rows)
  top.current = scroll.top
  const shown = [
    ...matches.slice(scroll.top, scroll.top + scroll.count).map((match, index) => ({ match, index: scroll.top + index })),
    ...pinned.slice(0, Math.max(0, limit - scroll.count - (scroll.above > 0 ? 1 : 0) - (scroll.below > 0 ? 1 : 0)))
      .map((match, index) => ({ match, index: listed + index })),
  ]
  // Sized over every choice rather than the visible ones, so the columns hold
  // still while the list scrolls or narrows.
  const glyphs = prompt.choices.some(choice => choice.role !== undefined)
  const statusWidth = Math.max(0, ...prompt.choices.map(choice => stringWidth(statusOf(choice, copy)?.text ?? '')))
  const room = Math.max(8, columns - FRAME - MARKER_WIDTH - (glyphs ? MARKER_WIDTH : 0) - (statusWidth > 0 ? statusWidth + GAP : 0))
  const labelWidth = Math.min(Math.floor(room * LABEL_SHARE), Math.max(1, ...prompt.choices.map(choice => stringWidth(choice.label)))) + GAP
  const hasDescription = prompt.choices.some(choice => choice.description !== undefined && choice.description !== '')

  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting} wrap="truncate-end">{prompt.title}</Text>
    {prompt.warning !== undefined && <Text color={PALETTE.waiting}>{prompt.warning}</Text>}
    <Text wrap="truncate-end">
      <Text bold color={PALETTE.asking}>{`${MARKER.prompt} `}</Text>
      {query}▌{query === '' && <Text dimColor>{copy.pickerFilter}</Text>}
    </Text>
    {scroll.above > 0 && <Edge arrow="↑" count={scroll.above} copy={copy} />}
    {shown.map(({ match, index }) => <Row key={match.choice.value} match={match} active={index === selectedIndex} copy={copy}
      glyphs={glyphs} labelWidth={hasDescription || statusWidth > 0 ? labelWidth : undefined} statusWidth={statusWidth} />)}
    {scroll.below > 0 && <Edge arrow="↓" count={scroll.below} copy={copy} />}
    {matches.length === 0 && <Text dimColor>{`${' '.repeat(MARKER_WIDTH)}${copy.noChoices}`}</Text>}
    <Box flexDirection="row">
      <Box flexGrow={1} flexShrink={1}><Text dimColor wrap="truncate-end">{copy.pickerHelp}</Text></Box>
      {matches.length > 0 && <Box flexShrink={0} marginLeft={GAP}><Text dimColor>{`${selectedIndex + 1}/${matches.length}`}</Text></Box>}
    </Box>
  </Box>
}

/** Columns of the pointer rail and the session glyph rail. */
const MARKER_WIDTH = 2

/**
 * The status a choice shows, if any: its own, or `Current` for the value in force.
 * @param choice - the choice.
 * @param copy - localized labels.
 * @returns the status to draw.
 */
function statusOf(choice: Choice, copy: TuiCopy): ChoiceStatus | undefined {
  if (choice.status !== undefined) return choice.status
  return choice.current === true || choice.role === 'session-current' ? { text: copy.currentSelection, tone: 'done' } : undefined
}

/** The row an edge of a scrolled list spends saying how much it hides. */
function Edge({ arrow, count, copy }: { readonly arrow: string, readonly count: number, readonly copy: TuiCopy }): React.ReactElement {
  return <Text dimColor wrap="truncate-end">{`${' '.repeat(MARKER_WIDTH)}${arrow} ${count} ${copy.pickerMore}`}</Text>
}

/**
 * One choice: the pointer, the session glyph, the label with its matched
 * letters underlined, the status, and the description.
 */
function Row({ match, active, copy, glyphs, labelWidth, statusWidth }: {
  readonly match: Match<Choice>
  readonly active: boolean
  readonly copy: TuiCopy
  readonly glyphs: boolean
  /** Fixed label column; absent when nothing follows the label. */
  readonly labelWidth: number | undefined
  readonly statusWidth: number
}): React.ReactElement {
  const { choice, ranges } = match
  const status = statusOf(choice, copy)
  const role = choice.role
  const tone = active ? { color: PALETTE.asking } : {}
  return <Box flexDirection="row">
    <Box width={MARKER_WIDTH} flexShrink={0}>
      <Text bold color={PALETTE.asking}>{active ? MARKER.selected : MARKER.none}</Text>
    </Box>
    {glyphs && <Box width={MARKER_WIDTH} flexShrink={0}>
      {role !== undefined && <Text
        {...role === 'session-saved' ? { dimColor: true } : { color: role === 'session-current' ? PALETTE.done : PALETTE.asking }}>
        {role === 'session-new' ? '+' : role === 'session-current' ? '●' : '○'}
      </Text>}
    </Box>}
    <Box flexShrink={0} {...labelWidth === undefined ? { flexGrow: 1 } : { width: labelWidth }}>
      <Text bold={active} wrap="truncate-end" {...tone}>{spans(choice.label, ranges)}</Text>
    </Box>
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
