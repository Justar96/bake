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
 * exactly one reason. Everything it renders is pure over props. Terminal
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
import type { Clock } from '@dsh-tui/ui/activity.ts'

/**
 * The turn header's clock, supplied the way the product supplies it. There
 * is none without a terminal, and none when colour is off, so a captured
 * preview stays still.
 */
const clock: Clock | undefined = process.stdout.isTTY === true && (process.env['NO_COLOR'] ?? '') === ''
  ? { now: () => performance.now(), every: (ms, tick) => { const timer = setInterval(tick, ms); return () => { clearInterval(timer) } } }
  : undefined

/**
 * Read a recorded session and project it into transcript rows.
 *
 * The lookup finds no tool. There is no registry here, and resolving one
 * would mean booting the harness this loop exists to avoid. Every tool call
 * therefore renders at its raw arguments. That is the same fallback a profile
 * with an unknown tool gets.
 *
 * @param path - the fixture path, relative to this file's directory.
 * @param copy - the dictionary the replay is being read in.
 * @returns every row the recording produces, in log order.
 */
export function rowsOf(path: string, copy: TuiCopy): readonly Row[] {
  const file = new URL(path, import.meta.url)
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
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    live: [] as const,
    pending: [] as const,
    stopping: false,
    command: undefined,
    notice: copy.previewHelp,
    interaction: undefined,
    todos: undefined,
    model: 'harness/replay',
    cwd: process.cwd(),
    sessionId: 'session-harness',
    context: undefined,
    copy,
    // The shipped frame. `@dsh-tui/app` resolves this from the terminal, and
    // this file never imports it. A developer iterating on components sees
    // the frame the product draws wherever the environment allows it.
    frame: 'round' as const,
    quitting: false,
    ...clock === undefined ? {} : { clock },
    onCancel: () => {},
    onInterrupt: () => { process.exit(0) },
    onAnswer: () => {},
  }
}

/** Show recorded rows and local composer submissions without executing a task. */
function Preview(
  { rows, copy, stepMs }: { readonly rows: readonly Row[], readonly copy: TuiCopy, readonly stepMs?: number },
): React.ReactElement {
  const previewCopy = useMemo(() => ({
    ...copy, ready: copy.preview, working: copy.previewReplaying, prompt: copy.previewPrompt,
    steering: copy.previewPrompt, send: copy.previewSend,
  }), [copy])
  const [state, setState] = useState(() => {
    const shown = stepMs === undefined ? rows.length : Math.min(1, rows.length)
    return { shown, committed: appendTranscript(emptyTranscript, rows.slice(0, shown)) }
  })
  useEffect(() => {
    if (stepMs === undefined || state.shown >= rows.length) return
    const timer = setTimeout(() => {
      setState(current => ({
        shown: current.shown + 1,
        committed: appendTranscript(current.committed, rows.slice(current.shown, current.shown + 1)),
      }))
    }, stepMs)
    return () => { clearTimeout(timer) }
  }, [state.shown, rows, stepMs])
  return <App {...staticProps(previewCopy)} committed={state.committed}
    status={state.shown < rows.length ? 'running' : 'idle'}
    onSubmit={text => { setState(current => ({
      ...current,
      committed: appendTranscript(current.committed, [
        { kind: 'user', text }, { kind: 'notice', tone: 'info', text: copy.previewAccepted },
      ]),
    })) }}
  />
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

// The preview's interrupt callback exits Bun's hot watcher as well as the renderer.
const options = {
  exitOnCtrlC: false, interactive: process.stdin.isTTY === true && process.stdout.isTTY === true, incrementalRendering: true,
}
render(<Preview rows={rows} copy={copy} {...args.includes('--replay') ? { stepMs: 220 } : {}} />, options)
