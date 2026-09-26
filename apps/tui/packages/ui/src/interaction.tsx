/** Terminal presentation for one scoped human-interaction request. */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste } from 'ink'
import stringWidth from 'string-width'
import type { AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionItem } from '@deepseek-ai/dsh-user-questions'
import type { TuiCopy } from './copy.ts'
import { useComposer } from './composer.ts'
import { MARKER } from './layout.ts'
import { PALETTE } from './palette.ts'
import { Picker, type ChoicePrompt } from './picker.tsx'

/** One pending interaction; the application owns settlement and cancellation. */
export type Interaction =
  | { readonly id: number; readonly kind: 'approval'; readonly tool: string; readonly reason: string; readonly callId?: string }
  | { readonly id: number; readonly kind: 'questions'; readonly questions: readonly AskUserQuestionItem[] }
  | { readonly id: number; readonly kind: 'login'; readonly message: string; readonly secret: boolean }
  | ({ readonly id: number; readonly kind: 'select' } & ChoicePrompt)

/** A human decision submitted for the displayed request. */
export type InteractionAnswer = string | AskUserQuestionAnswer

/**
 * Show the complete question or plan and collect an explicit response.
 * @param props - pending interaction, localized labels, and settlement callback.
 * @returns the interaction panel.
 */
export function InteractionView({ interaction, copy, onAnswer, limit }: {
  readonly interaction: Interaction
  readonly limit: number
  readonly copy: TuiCopy
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  return interaction.kind === 'select'
    ? <Picker prompt={interaction} copy={copy} limit={limit} onSelect={value => onAnswer(interaction.id, value)} />
    : interaction.kind === 'questions'
      ? <QuestionsView interaction={interaction} copy={copy} limit={limit} onAnswer={onAnswer} />
      : <RequestView interaction={interaction} copy={copy} onAnswer={onAnswer} />
}

function RequestView({ interaction, copy, onAnswer }: {
  readonly interaction: Extract<Interaction, { kind: 'approval' | 'login' }>
  readonly copy: TuiCopy
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  const completed = useRef(false)
  const composer = useComposer(input => {
    if (completed.current) return
    if (interaction.kind === 'login') { completed.current = true; onAnswer(interaction.id, input) }
  })
  usePaste(text => {
    if (interaction.kind !== 'approval') composer.paste(text)
  })
  useInput((text, key) => {
    if (key.meta || key.escape) return
    if (interaction.kind === 'approval') {
      if (key.ctrl) return
      if (text.trim().toLowerCase() === 'y') onAnswer(interaction.id, 'allowed-once')
      else if (text.trim().toLowerCase() === 'n') onAnswer(interaction.id, 'rejected')
      return
    }
    if (composer.editKey(text, key) || key.ctrl) return
    if (key.shift && key.return) composer.paste('\n')
    else composer.type(key.return ? '\n' : text)
  })
  // Every panel is ordered top to bottom. What is asked, the answer, then the keys.
  if (interaction.kind === 'approval') return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting} wrap="truncate-end">
      {copy.approval}: {interaction.tool}{interaction.callId === undefined ? '' : <Text bold={false} dimColor>{` ${interaction.callId}`}</Text>}
    </Text>
    <Text>{interaction.reason}</Text>
    <Text dimColor>{copy.approve}</Text>
  </Box>
  const mask = (text: string): string => interaction.secret ? '*'.repeat(text.length) : text
  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting}>{interaction.message}</Text>
    <Text><Text bold color={PALETTE.asking}>{`${MARKER.prompt} `}</Text>{mask(composer.before)}▌{mask(composer.after)}</Text>
    <Text dimColor>{copy.loginHelp}</Text>
  </Box>
}

/** Collect each question in order and return one structured answer for the request. */
function QuestionsView({ interaction, copy, limit, onAnswer }: {
  readonly interaction: Extract<Interaction, { kind: 'questions' }>
  readonly copy: TuiCopy
  readonly limit: number
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
    count={interaction.questions.length} copy={copy} limit={limit} onSubmit={accept} />
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
function QuestionPage({ question, number, count, copy, limit, onSubmit }: {
  readonly question: AskUserQuestionItem
  readonly number: number
  readonly count: number
  readonly copy: TuiCopy
  readonly limit: number
  readonly onSubmit: (answer: AskUserQuestionAnswerItem) => void
}): React.ReactElement {
  const options = question.options ?? []
  const other = options.length
  const multi = question.multiSelect === true
  const initial = question.intent?.kind === 'plan-review'
    ? options.findIndex(option => option.label !== question.intent?.approve) : 0
  const [focused, setFocused] = useState(initial < 0 ? other : initial)
  const cursor = useRef(focused)
  const [selected, setSelected] = useState<readonly string[]>([])
  const checked = useRef(selected)
  // Set by an Enter that had nothing to submit; any other key clears it.
  const [nudged, setNudged] = useState(false)
  const composer = useComposer(() => false)
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
    if (key.meta || key.escape) return
    if (key.return && !key.shift) { submit(); return }
    setNudged(false)
    if (key.upArrow || key.downArrow || key.tab) {
      focus((cursor.current + (key.upArrow || (key.tab && key.shift) ? -1 : 1) + other + 1) % (other + 1))
      return
    }
    if (key.shift && key.return) { focus(other); composer.paste('\n'); return }
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
  const optionLimit = Math.max(1, limit - 1)
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
  return <Box flexDirection="column" borderStyle="round" borderColor={PALETTE.waiting} paddingX={1}>
    <Box flexDirection="row">
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
    </Box>
    <Text bold>{question.question}</Text>
    {question.detail !== undefined && <Text>{question.detail}</Text>}
    <Text> </Text>
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
    <Text wrap="truncate-end">
      {pointer(other)}{box(custom.trim() !== '')}
      <Text dimColor={focused !== other}>{numbered(other)}</Text>
      <Text bold={focused === other} {...focused === other ? { color: PALETTE.asking } : {}}>{`${copy.customAnswer}: `}</Text>
      {focused === other
        ? <>{composer.before}▌{composer.after}{custom === '' ? <Text dimColor>{copy.customAnswerHint}</Text> : null}</>
        : custom === '' ? <Text dimColor>{copy.customAnswerHint}</Text> : custom}
    </Text>
    <Box flexDirection="row" marginTop={1}>
      <Box flexGrow={1} flexShrink={1}>
        {nudged
          ? <Text color={PALETTE.waiting} wrap="truncate-end">{multi ? copy.questionChooseOne : copy.questionTypeFirst}</Text>
          : <Text dimColor wrap="truncate-end">{multi ? copy.multiPickerHelp : copy.questionPickerHelp}</Text>}
      </Box>
      <Box flexShrink={0} marginLeft={2}><Text dimColor>{status}</Text></Box>
    </Box>
  </Box>
}
