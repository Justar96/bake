/** Recorded model/tool history rendered through the terminal's public components. */
import { readFile } from 'node:fs/promises'
import React from 'react'
import { afterEach, expect, it } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { parseSessionLog } from '@deepseek-ai/dsh-llm-replay'
import { Actions, App, appendTranscript, emptyTranscript, formatRow, project, projector, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'

afterEach(cleanup)

it('renders the shared bash recording in durable event order', async () => {
  const fixture = new URL('../../../../snapshots/session/bash-tool-turn/session.v3.jsonl', import.meta.url)
  const events = parseSessionLog(await readFile(fixture, 'utf8'))
  // No registry outside the application, so every tool keeps its raw arguments:
  // the fixture asserts durable event order, not a tool's own card.
  const seam = projector(dictionaries.en, () => undefined)
  // Folded as the controller folds them, so each action prints as one block.
  const actions = new Actions()
  const committed = events.reduce((history, event) => appendTranscript(history,
    actions.fold(project(event, seam), event.type === 'assistant/message' || event.type === 'turn/end')), emptyTranscript)
  const transcript = transcriptRows(committed).map(formatRow).join('\n') + '\n'
  await expect(transcript).toMatchFileSnapshot('../../../tests/fixtures/bash-tool-turn/transcript.expected.txt')
  const screen = render(React.createElement(App, {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, context: undefined, todos: undefined,
    model: 'deepseek-official/deepseek-v4-flash', cwd: '/workspace', sessionId: 'recorded-session', copy: dictionaries.en, frame: 'round', quitting: false,
    onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }))
  await expect(screen.lastFrame() + '\n').toMatchFileSnapshot('../../../tests/fixtures/bash-tool-turn/screen.expected.txt')
})
