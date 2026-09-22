/** Fixed-size Ink test streams keep snapshots independent of the host terminal. */
import React from 'react'
import { useStdout } from 'ink'
import { render as renderInk } from 'ink-testing-library'

export { cleanup } from 'ink-testing-library'

function SizedTerminal({ children }: { readonly children: React.ReactNode }): React.ReactElement {
  const { stdout } = useStdout()
  Object.defineProperties(stdout, {
    columns: { value: 100, configurable: true },
    rows: { value: 30, configurable: true },
  })
  return <>{children}</>
}

/** Render against the 100 by 30 viewport used by the component snapshots. */
export function render(tree: React.ReactElement): ReturnType<typeof renderInk> {
  const view = renderInk(<SizedTerminal>{tree}</SizedTerminal>)
  return { ...view, rerender: next => view.rerender(<SizedTerminal>{next}</SizedTerminal>) }
}
