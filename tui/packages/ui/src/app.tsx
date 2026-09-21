/** Terminal view over committed history, live presentation, and harness-owned state. */
import React from 'react'
import { Box, Static, Text, useInput, usePaste } from 'ink'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { AgentStatus } from '@deepseek-ai/dsh-agent'
import type { Row } from './rows.ts'
import type { TuiCopy } from './copy.ts'
import { useComposer } from './composer.ts'
import { InteractionView, type Interaction, type InteractionAnswer } from './interaction.tsx'

/** Display-only projection of one pending inbox message. */
export interface PendingInput {
  readonly id: string
  readonly target: 'next-step' | 'next-turn'
  readonly text: string
}

/** Application state and actions supplied by the terminal owner. */
export interface AppProps {
  readonly committed: readonly Row[]
  readonly live: readonly Row[]
  readonly pending: readonly PendingInput[]
  readonly status: AgentStatus
  readonly stopping: boolean
  readonly command: string | undefined
  readonly notice: string | undefined
  readonly interaction: Interaction | undefined
  readonly model: string
  readonly cwd: string
  readonly sessionId: string
  readonly copy: TuiCopy
  readonly onSubmit: (text: string) => void
  readonly onCancel: () => void
  readonly onInterrupt: () => void
  readonly onAnswer: (id: number, answer: InteractionAnswer) => void
}

/**
 * Render one complete committed or transient row without silently truncating results.
 * @param props - the row to render.
 * @returns the row element.
 */
export function RowView({ row }: { readonly row: Row }): React.ReactElement {
  switch (row.kind) {
    case 'tool-call': return <Text color="yellow">{'⚙ '}{row.tool}({row.input})</Text>
    case 'tool-result': return <Text color={row.ok ? 'gray' : 'red'}>{'← '}{row.text}</Text>
    case 'notice': return <Text color={row.tone === 'error' ? 'red' : row.tone === 'warn' ? 'yellow' : 'cyan'}>{row.tone === 'info' ? '· ' : '! '}{row.text}</Text>
    case 'user': return <Text color="cyan">{'> '}{row.text}</Text>
    case 'reasoning': return <Text dimColor>{'· '}{row.text}</Text>
    case 'assistant': return <Text>{'  '}{row.text}</Text>
    default: return assertNever(row)
  }
}

/**
 * Render the terminal session. Ink owns key decoding and bracketed-paste mode.
 * @param props - harness state, localized copy, and application callbacks.
 * @returns transcript, active interaction, status, and composer.
 */
export function App(props: AppProps): React.ReactElement {
  const composer = useComposer(props.onSubmit)
  const { copy, interaction } = props
  usePaste(composer.paste, { isActive: interaction === undefined })
  useInput((text, key) => {
    if (key.ctrl && text === 'c') { props.onInterrupt(); return }
    if (key.escape) { props.onCancel(); return }
    if (interaction !== undefined || key.ctrl || key.meta) return
    if (key.backspace || key.delete) composer.erase()
    else composer.type(key.return ? '\n' : text)
  })
  const status = props.stopping ? copy.stopping : props.status === 'running' ? copy.working : copy.ready
  return <Box flexDirection="column">
    <Static items={props.committed.map((row, key) => ({ row, key }))}>
      {item => <RowView key={item.key} row={item.row} />}
    </Static>
    {props.live.map((row, index) => <RowView key={index} row={row} />)}
    {props.pending.length > 0 && <Box flexDirection="column">
      <Text color="yellow">{copy.pending}</Text>
      {props.pending.map(message => <Text key={message.id} dimColor>{message.target === 'next-step' ? copy.nextStep : copy.nextTurn}: {message.text}</Text>)}
    </Box>}
    {interaction !== undefined && <InteractionView key={interaction.id} interaction={interaction} copy={copy} onAnswer={props.onAnswer} />}
    <Text dimColor>{props.status === 'running' ? '● ' : '○ '}{status}{'  '}{props.model}{'  '}{props.cwd}</Text>
    <Text dimColor>{copy.session}: {props.sessionId}</Text>
    {props.command !== undefined && <Text>{copy.command}: {props.command}</Text>}
    {props.notice !== undefined && <Text color="yellow">{props.notice}</Text>}
    <Text dimColor>{copy.help}{props.status === 'running' ? ` · ${copy.steering}` : ''}</Text>
    {interaction === undefined && <Text>{'> '}{composer.text}▌</Text>}
  </Box>
}
