/** Fixed-size Ink test streams keep snapshots independent of the host terminal. */
import React from 'react'
import { useStdout } from 'ink'
import { render as renderInk } from 'ink-testing-library'
import { markCaret } from './caret.ts'

export { cleanup } from 'ink-testing-library'

function SizedTerminal({ children, columns = 100, rows = 30 }: {
  readonly children: React.ReactNode
  readonly columns?: number
  readonly rows?: number
}): React.ReactElement {
  const { stdout } = useStdout()
  Object.defineProperties(stdout, {
    columns: { value: columns, configurable: true },
    rows: { value: rows, configurable: true },
  })
  return <>{children}</>
}

function renderSized(tree: React.ReactElement, columns: number, rows: number): ReturnType<typeof renderInk> {
  const view = renderInk(<SizedTerminal columns={columns} rows={rows}>{tree}</SizedTerminal>)
  return {
    ...view,
    rerender: next => view.rerender(<SizedTerminal columns={columns} rows={rows}>{next}</SizedTerminal>),
    // The caret is a reverse-video cell; the frame marks it with `▌` (see `caret.ts`).
    lastFrame: () => { const frame = view.lastFrame(); return frame === undefined ? undefined : markCaret(frame) },
  }
}

/** Render against the 100 by 30 viewport used by the component snapshots. */
export function render(tree: React.ReactElement): ReturnType<typeof renderInk> {
  return renderSized(tree, 100, 30)
}

/** Render against a deliberately narrow or short terminal. */
export function renderAt(tree: React.ReactElement, columns: number, rows: number): ReturnType<typeof renderInk> {
  return renderSized(tree, columns, rows)
}
