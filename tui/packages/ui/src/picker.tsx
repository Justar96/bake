/** Filterable terminal choices; the application owns acceptance and cancellation. */
import React, { useRef, useState } from 'react'
import { Box, Text, useInput, usePaste } from 'ink'
import type { TuiCopy } from './copy.ts'
import { composerText, eraseLast } from './editor.ts'

/** A provider-owned value with its display metadata. */
export interface Choice {
  readonly value: string
  readonly label: string
  readonly description?: string
  readonly current?: boolean
}

/** Application-owned choices and the initial cursor position. */
export interface ChoicePrompt {
  readonly title: string
  readonly choices: readonly Choice[]
  readonly initial: string
  readonly warning?: string
}

/**
 * Filter and choose a value without submitting text to the conversation.
 * @param props - choices, localized labels, row limit, and explicit acceptance callback.
 * @returns a bounded keyboard picker; Escape remains owned by the application.
 */
export function Picker({ prompt, copy, limit, onSelect }: {
  readonly prompt: ChoicePrompt
  readonly copy: TuiCopy
  readonly limit: number
  readonly onSelect: (value: string) => void
}): React.ReactElement {
  const [selected, setSelected] = useState(prompt.initial)
  const cursor = useRef(selected)
  const completed = useRef(false)
  const matches = (query: string) => prompt.choices.filter(choice =>
    `${choice.label} ${choice.value}`.toLowerCase().includes(query.trim().toLowerCase()))
  const indexOf = (choices: readonly Choice[]) => Math.max(0, choices.findIndex(choice => choice.value === cursor.current))
  const accept = (query: string): void => {
    const choices = matches(query)
    const choice = choices[indexOf(choices)]
    if (completed.current || choice === undefined) return
    completed.current = true
    onSelect(choice.value)
  }
  const [query, setQuery] = useState('')
  const draft = useRef(query)
  const update = (value: string): void => { draft.current = value; setQuery(value) }
  const choices = matches(query)
  const selectedIndex = indexOf(choices)
  const start = Math.max(0, selectedIndex - limit + 1)
  usePaste(text => update(draft.current + composerText(text)))
  useInput((text, key) => {
    if (key.ctrl || key.meta || key.escape || completed.current) return
    if (key.upArrow || key.downArrow) {
      const current = matches(draft.current)
      if (current.length === 0) return
      cursor.current = current[(indexOf(current) + (key.downArrow ? 1 : -1) + current.length) % current.length]!.value
      setSelected(cursor.current)
    } else if (key.return) accept(draft.current)
    else if (key.backspace || key.delete) update(eraseLast(draft.current))
    else if (!key.tab) {
      for (const [index, part] of composerText(text).split('\n').entries()) {
        if (index > 0) { accept(draft.current); if (completed.current) return }
        update(draft.current + part)
      }
    }
  })
  return <Box flexDirection="column" borderStyle="round" paddingX={1}>
    <Text color="yellow">{prompt.title}</Text>
    {prompt.warning !== undefined && <Text color="yellow">{prompt.warning}</Text>}
    {choices.slice(start, start + limit).map((choice, index) => <Text key={choice.value} wrap="truncate-end" {...start + index === selectedIndex ? { color: 'cyan' as const } : {}}>
      {start + index === selectedIndex ? '› ' : '  '}{choice.label}{choice.current ? ` · ${copy.currentSelection}` : ''}{choice.description === undefined ? '' : ` · ${choice.description}`}
    </Text>)}
    {choices.length === 0 && <Text dimColor>{copy.noChoices}</Text>}
    <Text dimColor>{copy.pickerHelp}{choices.length === 0 ? '' : ` · ${selectedIndex + 1}/${choices.length}`}</Text>
    <Text>{'> '}{query}▌</Text>
  </Box>
}
