/** Terminal presentation for one scoped human-interaction request. */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste } from 'ink'
import type { AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionItem } from '@deepseek-ai/dsh-user-questions'
import type { TuiCopy } from './copy.ts'
import { useComposer } from './composer.ts'

/** One pending interaction; the application owns settlement and cancellation. */
export type Interaction =
  | { readonly id: number; readonly kind: 'approval'; readonly tool: string; readonly reason: string; readonly callId?: string }
  | { readonly id: number; readonly kind: 'questions'; readonly questions: readonly AskUserQuestionItem[] }
  | { readonly id: number; readonly kind: 'login'; readonly message: string; readonly secret: boolean }

/** A human decision submitted for the displayed request. */
export type InteractionAnswer = string | AskUserQuestionAnswer

/**
 * Encode numbered choices or a written answer using the question service's fields.
 * @param question - the displayed question.
 * @param text - the user's non-empty response.
 * @returns exact option labels, or a custom answer.
 */
export function questionAnswer(question: AskUserQuestionItem, text: string): AskUserQuestionAnswerItem {
  const indexes = text.split(',').map(part => Number(part.trim()) - 1)
  const options = question.options ?? []
  if (/^\d+(\s*,\s*\d+)*$/.test(text) && (question.multiSelect === true || indexes.length === 1)
    && indexes.every(index => Number.isInteger(index) && options[index] !== undefined)) {
    return { id: question.id, selected: [...new Set(indexes)].map(index => options[index]!.label) }
  }
  return { id: question.id, selected: [], custom: text }
}

/**
 * Show the complete question or plan and collect an explicit response.
 * @param props - pending interaction, localized labels, and settlement callback.
 * @returns the interaction panel.
 */
export function InteractionView({ interaction, copy, onAnswer }: {
  readonly interaction: Interaction
  readonly copy: TuiCopy
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}): React.ReactElement {
  const [answers, setAnswers] = useState<AskUserQuestionAnswerItem[]>([])
  const answered = useRef<AskUserQuestionAnswerItem[]>([])
  const completed = useRef(false)
  const question = interaction.kind === 'questions' ? interaction.questions[answers.length] : undefined
  const composer = useComposer(input => {
    if (completed.current) return
    if (interaction.kind === 'login') { completed.current = true; onAnswer(interaction.id, input); return }
    if (interaction.kind !== 'questions') return
    const active = interaction.questions[answered.current.length]
    if (active === undefined) return
    const next = [...answered.current, questionAnswer(active, input.trim())]
    answered.current = next
    if (next.length === interaction.questions.length) { completed.current = true; onAnswer(interaction.id, { answers: next }) }
    else setAnswers(next)
  })
  usePaste(text => {
    if (interaction.kind !== 'approval') composer.paste(text)
  })
  useInput((text, key) => {
    if (key.ctrl || key.meta || key.escape) return
    if (interaction.kind === 'approval') {
      if (text.trim().toLowerCase() === 'y') onAnswer(interaction.id, 'allowed-once')
      else if (text.trim().toLowerCase() === 'n') onAnswer(interaction.id, 'rejected')
      return
    }
    if (key.backspace || key.delete) composer.erase()
    else composer.type(key.return ? '\n' : text)
  })
  if (interaction.kind === 'approval') return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text color="yellow">{copy.approval}: {interaction.tool} {interaction.callId}</Text>
    <Text>{interaction.reason}</Text><Text>{copy.approve}</Text>
  </Box>
  if (interaction.kind === 'login') return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text color="yellow">{interaction.message}</Text><Text dimColor>{copy.cancelHelp}</Text>
    <Text>{'? '}{interaction.secret ? '*'.repeat(composer.text.length) : composer.text}▌</Text>
  </Box>
  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text color="yellow">{copy.questions} ({answers.length + 1}/{interaction.questions.length})</Text>
    <Text>{question?.header}</Text><Text>{question?.question}</Text>
    {question?.detail !== undefined && <Text>{question.detail}</Text>}
    {question?.options?.map((option, index) => <Text key={option.label}>{index + 1}. {option.label}{option.description ? ` — ${option.description}` : ''}</Text>)}
    <Text dimColor>{question?.multiSelect ? copy.multiHelp : copy.questionHelp}</Text>
    <Text>{'> '}{composer.text}▌</Text>
  </Box>
}
