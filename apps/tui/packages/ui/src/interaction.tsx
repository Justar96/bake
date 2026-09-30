/** Terminal presentation for one scoped human-interaction request. */
import React, { useMemo, useRef, useState } from 'react'
import { Box, Text, useInput, usePaste } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import type { AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionItem } from '@deepseek-ai/dsh-user-questions'
import type { TuiCopy } from './copy.ts'
import { isNewline, useComposer } from './composer.ts'
import { cursorWindow } from './editor.ts'
import { MARKER } from './layout.ts'
import { PALETTE } from './palette.ts'
import { Picker, type ChoicePrompt } from './picker.tsx'

/**
 * One field of a sign-in: what it asks, where the flow stands, and why the
 * last answer was refused. Only `message` and `secret` are required, so a
 * flow that knows nothing more still gets a usable panel.
 */
export interface LoginField {
  /** What is being signed in to, drawn as the panel heading. */
  readonly title?: string
  /** What this field asks for. */
  readonly message: string
  /** Masks the answer as it is typed. */
  readonly secret: boolean
  /** This field's place in a flow of several. */
  readonly step?: { readonly index: number; readonly count: number }
  /**
   * The value an empty answer stands for, shown dim in the empty field.
   * Enter on an empty field submits it empty, and the flow applies this.
   */
  readonly fallback?: string
  /** An example shown dim in the empty field; an empty answer is refused. */
  readonly placeholder?: string
  /** One dim line of guidance under the field. */
  readonly hint?: string
  /** Why the previous answer was refused, shown until the next one is typed. */
  readonly error?: string
  /** Text the field opens with, such as the address that just failed. */
  readonly initial?: string
}

/** One pending interaction; the application owns settlement and cancellation. */
export type Interaction =
  | { readonly id: number; readonly kind: 'approval'; readonly tool: string; readonly reason: string; readonly callId?: string }
  | { readonly id: number; readonly kind: 'questions'; readonly questions: readonly AskUserQuestionItem[] }
  | ({ readonly id: number; readonly kind: 'login' } & LoginField)
  | ({ readonly id: number; readonly kind: 'select' } & ChoicePrompt)

/** A choice answered with the level the picker's levels row showed for it. */
export interface ChoiceAnswer {
  readonly value: string
  readonly level: string
}

/** A human decision submitted for the displayed request. */
export type InteractionAnswer = string | AskUserQuestionAnswer | ChoiceAnswer

/**
 * Show the complete question or plan and collect an explicit response.
 * @param props - pending interaction, localized labels, and settlement callback.
 * @returns the interaction panel.
 */
export function InteractionView({ interaction, copy, onAnswer, limit, height, columns }: {
  readonly interaction: Interaction
  /** Maximum selectable rows, including Other. */
  readonly limit: number
  /** Total rows available to the interaction. */
  readonly height: number
  readonly columns: number
  readonly copy: TuiCopy
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  return interaction.kind === 'select'
    ? <Picker prompt={interaction} copy={copy} limit={limit}
      onSelect={(value, level) => onAnswer(interaction.id, level === undefined ? value : { value, level })} />
    : interaction.kind === 'questions'
      ? <QuestionsView interaction={interaction} copy={copy} limit={limit} height={height} columns={columns} onAnswer={onAnswer} />
      : interaction.kind === 'login'
        ? <LoginView interaction={interaction} copy={copy} columns={columns} onAnswer={onAnswer} />
        : <ApprovalView interaction={interaction} copy={copy} onAnswer={onAnswer} />
}

function ApprovalView({ interaction, copy, onAnswer }: {
  readonly interaction: Extract<Interaction, { kind: 'approval' }>
  readonly copy: TuiCopy
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  // A paste is taken and dropped: pasted text that happens to read `y` is not a decision.
  usePaste(() => {})
  useInput((text, key) => {
    if (key.meta || key.escape || key.ctrl) return
    if (text.trim().toLowerCase() === 'y') onAnswer(interaction.id, 'allowed-once')
    else if (text.trim().toLowerCase() === 'n') onAnswer(interaction.id, 'rejected')
  })
  // Every panel is ordered top to bottom. What is asked, the answer, then the keys.
  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting} wrap="truncate-end">
      {copy.approval}: {interaction.tool}{interaction.callId === undefined ? '' : <Text bold={false} dimColor>{` ${interaction.callId}`}</Text>}
    </Text>
    <Text>{interaction.reason}</Text>
    <Text dimColor>{copy.approve}</Text>
  </Box>
}

/** One masked character. A bullet, not an asterisk, so a key does not read as a wildcard. */
const MASK = '\u2022'
const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
/** Leads a refused answer's reason, as it leads a failed action. */
const MARKER_FAILED = '\u2717'

/**
 * One sign-in field: the heading and step, what is asked, the answer, then
 * guidance, the refusal of the last answer, and the keys.
 *
 * Enter on an empty field submits only when the field has a `fallback` to
 * stand for; otherwise it says what is missing instead of doing nothing. The
 * refusal stays until the answer changes, so it describes what is on screen.
 */
function LoginView({ interaction, copy, columns, onAnswer }: {
  readonly interaction: Extract<Interaction, { kind: 'login' }>
  readonly copy: TuiCopy
  readonly columns: number
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  const completed = useRef(false)
  const [nudged, setNudged] = useState(false)
  const [edited, setEdited] = useState(false)
  const composer = useComposer(input => {
    if (completed.current) return
    completed.current = true
    onAnswer(interaction.id, input)
  }, undefined, interaction.fallback !== undefined, interaction.initial ?? '')
  const change = (): void => { setNudged(false); setEdited(true) }
  usePaste(text => { change(); composer.paste(text) })
  useInput((text, key) => {
    const newline = isNewline(text, key)
    if ((key.meta && !newline) || key.escape) return
    // Before the Ctrl guard, which a CSI-u Ctrl-J would otherwise stop at.
    if (newline) { change(); composer.paste(text.startsWith('\n') ? text : '\n'); return }
    if (key.return && composer.value.trim() === '' && interaction.fallback === undefined) { setNudged(true); return }
    if (composer.editKey(text, key)) { if (key.backspace || key.delete) change(); return }
    if (key.ctrl) return
    if (!key.return) change()
    composer.type(key.return ? '\n' : text)
  })
  const mask = (text: string): string => interaction.secret ? MASK.repeat(Array.from(graphemes.segment(text)).length) : text
  const empty = composer.text === ''
  const shadow = interaction.fallback ?? interaction.placeholder
  const error = nudged ? copy.loginEmpty : edited ? undefined : interaction.error
  const heading = interaction.title ?? interaction.message
  const field = cursorWindow(mask(composer.before), mask(composer.after), Math.max(1, columns - 6))
  return <Box width={Math.max(1, columns)} flexDirection="column" borderStyle="round" paddingX={1}>
    <Box flexDirection="row">
      <Box flexGrow={1} flexShrink={1}><Text bold color={PALETTE.waiting} wrap="truncate-end">{heading}</Text></Box>
      {interaction.step !== undefined && interaction.step.count > 1 && <Box flexShrink={0} marginLeft={2}>
        <Text>
          {Array.from({ length: interaction.step.count }, (_, index) => <Text key={index}
            {...index < interaction.step!.index - 1 ? { color: PALETTE.done }
              : index === interaction.step!.index - 1 ? { color: PALETTE.waiting, bold: true } : { dimColor: true }}>
            {index < interaction.step!.index ? MARKER.action : MARKER.waiting}</Text>)}
          <Text dimColor>{`  ${interaction.step.index}/${interaction.step.count}`}</Text>
        </Text>
      </Box>}
    </Box>
    {interaction.title !== undefined && <Text bold wrap="truncate-end">{interaction.message}</Text>}
    <Text wrap="truncate-end">
      <Text bold color={PALETTE.asking}>{`${MARKER.prompt} `}</Text>
      {field.before}▌{field.after}
      {empty && shadow !== undefined ? <Text dimColor>{shadow}</Text> : null}
    </Text>
    {interaction.hint !== undefined && <Text dimColor wrap="truncate-end">{`  ${interaction.hint}`}</Text>}
    {error !== undefined && <Text color={PALETTE.failed}>{`${MARKER_FAILED} ${error}`}</Text>}
    <Text dimColor wrap="truncate-end">{interaction.error !== undefined && !edited ? copy.loginRetryHelp : copy.loginHelp}</Text>
  </Box>
}

/** Collect each question in order and return one structured answer for the request. */
function QuestionsView({ interaction, copy, limit, height, columns, onAnswer }: {
  readonly interaction: Extract<Interaction, { kind: 'questions' }>
  readonly copy: TuiCopy
  readonly limit: number
  readonly height: number
  readonly columns: number
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement | null {
  const [answers, setAnswers] = useState<AskUserQuestionAnswerItem[]>([])
  const answered = useRef<AskUserQuestionAnswerItem[]>([])
  const completed = useRef(false)
  const question = interaction.questions[answers.length]
  if (question === undefined) return null
  const accept = (answer: AskUserQuestionAnswerItem): void => {
    if (completed.current || answered.current.length !== answers.length || answer.id !== question.id) return
    const next = [...answered.current, answer]
    answered.current = next
    if (next.length === interaction.questions.length) {
      completed.current = true
      onAnswer(interaction.id, { answers: next })
    } else setAnswers(next)
  }
  return <QuestionPage key={`${answers.length}:${question.id}`} question={question} number={answers.length + 1}
    count={interaction.questions.length} copy={copy} limit={limit} height={height} columns={columns} onSubmit={accept} />
}

/** A multi-select box's two states. The tick is a text glyph one cell wide, never an emoji. */
const BOX = { on: '[✓]', off: '[ ]' } as const

/** Widest a label may pad to so descriptions line up; a longer one pushes its own description. */
const LABEL_COLUMN = 28

/**
 * One question, its options, and an Other answer, in one box.
 *
 * Option focus is kept separate from the Other draft, so browsing cannot
 * erase it. The head names the step and the question's short header; each
 * option is numbered, with its description in a dim column beside it. A
 * multi-select question gives every option a box, and ticks the Other row
 * once it has text, since that text is submitted with the ticked options.
 * The Other row is always the last, stays in view while the options above
 * it scroll, and is numbered after them, so a digit reaches it too. Typing on an option moves to the Other row and types
 * there. An Enter with nothing to submit says so in the footer instead of
 * doing nothing.
 */
function QuestionPage({ question, number, count, copy, limit, height, columns, onSubmit }: {
  readonly question: AskUserQuestionItem
  readonly number: number
  readonly count: number
  readonly copy: TuiCopy
  readonly limit: number
  readonly height: number
  readonly columns: number
  readonly onSubmit: (answer: AskUserQuestionAnswerItem) => void
}): React.ReactElement {
  const options = question.options ?? []
  const other = options.length
  const multi = question.multiSelect === true
  const [focused, setFocused] = useState(0)
  const cursor = useRef(focused)
  const [selected, setSelected] = useState<readonly string[]>([])
  const checked = useRef(selected)
  // Set by an Enter that had nothing to submit; any other key clears it.
  const [nudged, setNudged] = useState(false)
  const [detailStart, setDetailStart] = useState(0)
  const composer = useComposer(() => false)
  const framed = height >= 7 && columns >= 4
  const contentRows = Math.max(1, height - (framed ? 2 : 0))
  const headingRows = contentRows >= 4 ? 2 : contentRows >= 3 ? 1 : 0
  const footerRows = Number(contentRows >= 5)
  const choiceRows = contentRows - headingRows - footerRows
  const showOther = other === 0 || choiceRows > 1 || focused === other
  const optionLimit = Math.max(0, Math.min(other, Math.max(0, limit - 1), choiceRows - Number(showOther)))
  const spare = Math.max(0, choiceRows - optionLimit - Number(showOther))
  const gaps = spare >= 2 && footerRows > 0 && question.detail !== undefined && question.detail !== '' ? 2 : 0
  const detailCapacity = Math.max(0, spare - gaps)
  const detailLines = useMemo(() => question.detail === undefined ? []
    : wrapAnsi(question.detail, Math.max(1, columns - (framed ? 4 : 0)), { hard: true, trim: false }).split('\n'),
  [question.detail, columns, framed])
  const detailOverflow = detailLines.length > detailCapacity
  const detailPage = Math.max(1, detailCapacity - Number(detailOverflow && detailCapacity > 1))
  const detailLast = Math.max(0, detailLines.length - detailPage)
  const detailOffset = Math.min(detailStart, detailLast)
  const scrollDetail = (delta: number): void => {
    setDetailStart(current => Math.max(0, Math.min(detailLast, Math.min(current, detailLast) + delta)))
  }
  const focus = (index: number): void => { cursor.current = index; setFocused(index) }
  const toggle = (index: number): void => {
    const label = options[index]?.label
    if (label === undefined) return
    checked.current = checked.current.includes(label)
      ? checked.current.filter(item => item !== label) : [...checked.current, label]
    setSelected(checked.current)
  }
  const submit = (): void => {
    const custom = composer.value.trim()
    if (multi) {
      if (checked.current.length === 0 && custom === '') { setNudged(true); return }
      onSubmit({ id: question.id, selected: [...checked.current], ...(custom === '' ? {} : { custom }) })
    } else if (cursor.current < other) {
      onSubmit({ id: question.id, selected: [options[cursor.current]!.label] })
    } else if (custom !== '') onSubmit({ id: question.id, selected: [], custom })
    else setNudged(true)
  }
  usePaste(text => { setNudged(false); focus(other); composer.paste(text) })
  useInput((text, key) => {
    const newline = isNewline(text, key)
    if ((key.meta && !newline) || key.escape) return
    if (key.return && !newline) { submit(); return }
    if (key.pageUp || (key.shift && key.upArrow)) { scrollDetail(-detailPage); return }
    if (key.pageDown || (key.shift && key.downArrow)) { scrollDetail(detailPage); return }
    if (key.ctrl && key.home) { setDetailStart(0); return }
    if (key.ctrl && key.end) { setDetailStart(detailLines.length); return }
    setNudged(false)
    if (key.upArrow || key.downArrow || key.tab) {
      focus((cursor.current + (key.upArrow || (key.tab && key.shift) ? -1 : 1) + other + 1) % (other + 1))
      return
    }
    if (newline) { focus(other); composer.paste(text.startsWith('\n') ? text : '\n'); return }
    if (multi && cursor.current < other && text === ' ') { toggle(cursor.current); return }
    if (cursor.current < other && /^[1-9]$/.test(text) && Number(text) <= other + 1) {
      focus(Number(text) - 1)
      return
    }
    if (key.ctrl && cursor.current !== other) return
    focus(other)
    if (composer.editKey(text, key) || key.ctrl) return
    composer.paste(text)
  })
  // Options scroll; the Other row stays under them, so its draft never leaves view.
  const rows = other + 1
  const start = Math.max(0, Math.min(focused, other - 1) - optionLimit + 1)
  const end = Math.min(other, start + optionLimit)
  const custom = composer.before + composer.after
  const digits = String(rows).length
  const labelWidth = Math.min(LABEL_COLUMN, Math.max(0, ...options.map(option => option.description ? stringWidth(option.label) : 0)))
  const box = (on: boolean): React.ReactElement | null => multi
    ? <Text {...on ? { color: PALETTE.done, bold: true } : { dimColor: true }}>{`${on ? BOX.on : BOX.off} `}</Text>
    : null
  const pointer = (index: number): React.ReactElement =>
    <Text bold color={PALETTE.asking}>{index === focused ? `${MARKER.selected} ` : '  '}</Text>
  const numbered = (index: number): string => `${String(index + 1).padStart(digits)}. `
  const status = multi
    ? `${selected.length + (custom.trim() === '' ? 0 : 1)} ${copy.questionSelected}`
    : `${focused + 1}/${rows}`
  // A neutral frame, like the approval and input panels. The yellow title says
  // it is waiting on the user; a yellow frame around it said so twice.
  return <Box width={Math.max(1, columns)} maxHeight={Math.max(1, height)} flexDirection="column" flexShrink={0}
    {...framed ? { borderStyle: 'round', paddingX: 1 } : {}} overflowY="hidden">
    {headingRows >= 2 && <Box flexDirection="row">
      <Box flexGrow={1} flexShrink={1}>
        <Text wrap="truncate-end">
          <Text bold color={PALETTE.waiting}>{copy.questions}</Text>
          {question.header === undefined ? null : <Text dimColor>{`  ${question.header}`}</Text>}
        </Text>
      </Box>
      {count > 1 && <Box flexShrink={0} marginLeft={2}>
        <Text>
          {Array.from({ length: count }, (_, index) => <Text key={index} {...index < number - 1 ? { color: PALETTE.done }
            : index === number - 1 ? { color: PALETTE.waiting, bold: true } : { dimColor: true }}>
            {index < number ? MARKER.action : MARKER.waiting}</Text>)}
          <Text dimColor>{`  ${number}/${count}`}</Text>
        </Text>
      </Box>}
    </Box>}
    {headingRows >= 1 && <Text bold wrap="truncate-end">{question.question}</Text>}
    {detailCapacity > 0 && detailLines.length > 0 && <Box flexDirection="column" flexShrink={0}>
      {detailLines.slice(detailOffset, detailOffset + detailPage).map((line, index) =>
        <Text key={detailOffset + index} wrap="truncate-end">{line}</Text>)}
      {detailOverflow && detailCapacity > 1 && <Text dimColor wrap="truncate-end">
        {`${copy.questionDetailHelp} · ${detailOffset + 1}-${Math.min(detailLines.length, detailOffset + detailPage)}/${detailLines.length}`}
      </Text>}
    </Box>}
    {gaps > 0 && <Text> </Text>}
    {options.slice(start, end).map((option, index) => {
      const absolute = start + index
      const here = absolute === focused
      return <Text key={absolute} wrap="truncate-end">
        {pointer(absolute)}{box(selected.includes(option.label))}
        <Text dimColor={!here}>{numbered(absolute)}</Text>
        <Text bold={here} {...here ? { color: PALETTE.asking } : {}}>{option.label}</Text>
        {option.description ? <Text dimColor>{`${' '.repeat(Math.max(0, labelWidth - stringWidth(option.label)))}  ${option.description}`}</Text> : ''}
      </Text>
    })}
    {showOther && <Text wrap="truncate-end">
      {pointer(other)}{box(custom.trim() !== '')}
      <Text dimColor={focused !== other}>{numbered(other)}</Text>
      <Text bold={focused === other} {...focused === other ? { color: PALETTE.asking } : {}}>{`${copy.customAnswer}: `}</Text>
      {focused === other
        ? <>{composer.before}▌{composer.after}{custom === '' ? <Text dimColor>{copy.customAnswerHint}</Text> : null}</>
        : custom === '' ? <Text dimColor>{copy.customAnswerHint}</Text> : custom}
    </Text>}
    {footerRows > 0 && <Box flexDirection="row" marginTop={gaps > 0 ? 1 : 0}>
      <Box flexGrow={1} flexShrink={1}>
        {nudged
          ? <Text color={PALETTE.waiting} wrap="truncate-end">{multi ? copy.questionChooseOne : copy.questionTypeFirst}</Text>
          : <Text dimColor wrap="truncate-end">{multi ? copy.multiPickerHelp : copy.questionPickerHelp}</Text>}
      </Box>
      <Box flexShrink={0} marginLeft={2}><Text dimColor>{status}</Text></Box>
    </Box>}
  </Box>
}
