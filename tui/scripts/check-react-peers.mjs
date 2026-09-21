/**
 * Verify renderer peer identity for the fork-local Ink consumers.
 *
 * React's hook dispatcher is module-global, so a package that resolves a
 * different React instance than Ink does renders against an empty dispatcher
 * and fails at the first hook. pnpm's peer resolution can split instances
 * silently when a range changes, which is why this is checked rather than
 * assumed.
 *
 * Upstream's own React graph is deliberately out of scope. The repository
 * resolves `@testing-library/react` against React 18 while `react-dom` gets 19,
 * and that predates this fork: `react@18.3.1` appears in `pnpm-lock.yaml` the
 * same number of times before and after `tui/` existed. Asserting it here would
 * fail this gate for a condition the fork does not own and cannot fix inside
 * `tui/`.
 */
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'

for (const name of ['app', 'ui', 'harness']) {
  const consumer = createRequire(new URL(`../packages/${name}/package.json`, import.meta.url))
  const ink = createRequire(consumer.resolve('ink'))
  assert.ok(consumer('react') === ink('react'), `${name} and Ink must share React`)
}
console.log('React peers: every TUI renderer shares one React instance with Ink')
