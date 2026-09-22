/**
 * Component development loop. Replays a recorded session through the real
 * projection and the real shell, with no harness runtime, no agent, no network,
 * and no API key.
 *
 * ```sh
 * bun --hot tui/packages/harness/dev.tsx              # iterate on components
 * bun tui/packages/harness/dev.tsx --replay           # watch it arrive in order
 * bun tui/packages/harness/dev.tsx --locale zh        # check a dictionary
 * bun tui/packages/harness/dev.tsx fixtures/other.jsonl
 * ```
 *
 * This runs under Bun while the product runs under Node. That is safe for
 * exactly one reason: everything it renders is pure over props. Terminal
 * ownership, the agent, and every service live in `@dsh-tui/app`, which this
 * file never imports.
 *
 * @module tui-harness
 */

import { readFileSync } from 'node:fs'
import React, { useEffect, useMemo, useState } from 'react'
import { render } from 'ink'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { App, appendTranscript, emptyTranscript, project, projector } from '@dsh-tui/ui'
import type { Row } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/**
 * Read a recorded session and project it into transcript rows.
 *
 * The lookup finds no tool: there is no registry here, and resolving one would
 * mean booting the harness this loop exists to avoid. Every tool call
 * therefore renders at its raw arguments, which is the fallback a profile with
 * an unknown tool gets too.
 *
 * @param path - the fixture path, relative to this file's directory.
 * @param copy - the dictionary the replay is being read in.
 * @returns every row the recording produces, in log order.
 */
export function rowsOf(path: string, copy: TuiCopy): readonly Row[] {
  const file = new URL(path, import.meta.url).pathname
  const seam = projector(copy, () => undefined)
  const rows: Row[] = []
  for (const line of readFileSync(file, 'utf8').split('\n')) {
    if (line.trim() === '') continue
    rows.push(...project(JSON.parse(line) as SessionEvent, seam))
  }
  return rows
}

/** Everything the shell needs that a replay has no live source for. */
function staticProps(copy: TuiCopy) {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8,
    live: [] as const,
    pending: [] as const,
    stopping: false,
    command: undefined,
    notice: undefined,
    interaction: undefined,
    todos: undefined,
    model: 'harness/replay',
    cwd: process.cwd(),
    sessionId: 'session-harness',
    context: undefined,
    copy,
    onSubmit: () => {},
    onCancel: () => {},
    onInterrupt: () => { process.exit(0) },
    onAnswer: () => {},
  }
}

/** Replay rows one at a time so the transcript can be watched as it arrives. */
function Replay(
  { rows, copy, stepMs }: { readonly rows: readonly Row[], readonly copy: TuiCopy, readonly stepMs: number },
): React.ReactElement {
  const versions = useMemo(() => {
    let transcript = emptyTranscript
    return rows.map(row => (transcript = appendTranscript(transcript, [row])))
  }, [rows])
  const [shown, setShown] = useState(1)
  useEffect(() => {
    if (shown >= rows.length) return
    const timer = setTimeout(() => { setShown(count => count + 1) }, stepMs)
    return () => { clearTimeout(timer) }
  }, [shown, rows.length, stepMs])
  const running = shown < rows.length
  return <App {...staticProps(copy)} committed={versions[shown - 1] ?? emptyTranscript} status={running ? 'running' : 'idle'} />
}

const args = process.argv.slice(2)
const localeArg = args[args.indexOf('--locale') + 1]
const locale = args.includes('--locale') && localeArg !== undefined && localeArg in dictionaries
  ? localeArg as keyof typeof dictionaries
  : 'en'
const copy = dictionaries[locale]
const fixture = args.find((arg, index) => !arg.startsWith('--') && args[index - 1] !== '--locale')
  ?? 'fixtures/session.jsonl'
const rows = rowsOf(fixture, copy)

if (args.includes('--replay')) render(<Replay rows={rows} copy={copy} stepMs={220} />)
else render(<App {...staticProps(copy)} committed={appendTranscript(emptyTranscript, rows)} status="idle" />)
