/** Recorded model/tool history rendered through the terminal's public components. */
import { readFile } from 'node:fs/promises'
import React from 'react'
import { renderToString } from 'ink'
import { expect, it } from 'vitest'
import { parseSessionLog } from '@deepseek-ai/dsh-llm-replay'
import { App, appendTranscript, emptyTranscript, formatRow, project, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'

it('renders the shared bash recording in durable event order', async () => {
  const fixture = new URL('../../../../snapshots/session/bash-tool-turn/session.v3.jsonl', import.meta.url)
  const events = parseSessionLog(await readFile(fixture, 'utf8'))
  const committed = events.reduce((history, event) => appendTranscript(history, project(event)), emptyTranscript)
  const transcript = transcriptRows(committed).map(formatRow).join('\n') + '\n'
  await expect(transcript).toMatchFileSnapshot('../../../tests/fixtures/bash-tool-turn/transcript.expected.txt')
  const screen = renderToString(React.createElement(App, {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8,
    committed, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, context: undefined,
    model: 'deepseek-official/deepseek-v4-flash', cwd: '/workspace', sessionId: 'recorded-session', copy: dictionaries.en,
    onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }), { columns: 120 })
  await expect(screen + '\n').toMatchFileSnapshot('../../../tests/fixtures/bash-tool-turn/screen.expected.txt')
})
