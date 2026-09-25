/** Terminal presentation for one scoped human-interaction request. */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste } from 'ink'
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

/** Keep option focus separate from the Other draft, so browsing cannot erase it. */
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
  const initial = question.intent?.kind === 'plan-review'
    ? options.findIndex(option => option.label !== question.intent?.approve) : 0
  const [focused, setFocused] = useState(initial < 0 ? other : initial)
  const cursor = useRef(focused)
  const [selected, setSelected] = useState<readonly string[]>([])
  const checked = useRef(selected)
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
    if (question.multiSelect === true) {
      if (checked.current.length === 0 && custom === '') return
      onSubmit({ id: question.id, selected: [...checked.current], ...(custom === '' ? {} : { custom }) })
    } else if (cursor.current < other) {
      onSubmit({ id: question.id, selected: [options[cursor.current]!.label] })
    } else if (custom !== '') onSubmit({ id: question.id, selected: [], custom })
  }
  usePaste(text => { focus(other); composer.paste(text) })
  useInput((text, key) => {
    if (key.meta || key.escape) return
    if (key.upArrow || key.downArrow || key.tab) {
      focus((cursor.current + (key.upArrow ? -1 : 1) + other + 1) % (other + 1))
      return
    }
    if (key.return && !key.shift) { submit(); return }
    if (key.shift && key.return) { focus(other); composer.paste('\n'); return }
    if (question.multiSelect === true && cursor.current < other && text === ' ') { toggle(cursor.current); return }
    if (cursor.current < other && /^[1-9]$/.test(text) && Number(text) <= other) {
      focus(Number(text) - 1)
      return
    }
    if (key.ctrl && cursor.current !== other) return
    focus(other)
    if (composer.editKey(text, key) || key.ctrl) return
    composer.paste(text)
  })
  const optionLimit = Math.max(1, limit - 1)
  const start = Math.max(0, Math.min(focused, other - 1) - optionLimit + 1)
  const shown = options.slice(start, start + optionLimit)
  const custom = composer.before + composer.after
  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text bold color={PALETTE.waiting}>{copy.questions}{count > 1 ? <Text bold={false} dimColor>{` ${number}/${count}`}</Text> : ''}</Text>
    {question.header !== undefined && <Text>{question.header}</Text>}
    <Text>{question.question}</Text>
    {question.detail !== undefined && <Text>{question.detail}</Text>}
    {shown.map((option, index) => {
      const absolute = start + index
      return <Text key={absolute} wrap="truncate-end" {...absolute === focused ? { color: PALETTE.asking } : {}}>
        {absolute === focused ? MARKER.selected : ' '} {question.multiSelect === true ? `[${selected.includes(option.label) ? 'x' : ' '}] ` : ''}{absolute + 1}. {option.label}{option.description ? <Text dimColor={absolute !== focused}>{`  ${option.description}`}</Text> : ''}
      </Text>
    })}
    <Text {...focused === other ? { color: PALETTE.asking } : {}}>
      {focused === other ? MARKER.selected : ' '} {copy.customAnswer}: {focused === other
        ? <>{composer.before}▌{composer.after}</> : custom}
    </Text>
    <Box flexDirection="row">
      <Box flexGrow={1} flexShrink={1}>
        <Text dimColor wrap="truncate-end">{question.multiSelect === true ? copy.multiPickerHelp : copy.questionPickerHelp}</Text>
      </Box>
      <Box flexShrink={0} marginLeft={2}><Text dimColor>{`${focused + 1}/${other + 1}`}</Text></Box>
    </Box>
  </Box>
}
