/** Recorded model/tool history rendered through the terminal's public components. */
import { readFile } from 'node:fs/promises'
import { expect, it } from 'vitest'
import { parseSessionLog } from '@deepseek-ai/dsh-llm-replay'
import { formatRow, project } from '@dsh-tui/ui'

it('renders the shared bash recording in durable event order', async () => {
  const fixture = new URL('../../../../snapshots/session/bash-tool-turn/session.v3.jsonl', import.meta.url)
  const events = parseSessionLog(await readFile(fixture, 'utf8'))
  const transcript = events.flatMap(project).map(formatRow).join('\n') + '\n'
  await expect(transcript).toMatchFileSnapshot('../../../tests/fixtures/bash-tool-turn/transcript.expected.txt')
})
