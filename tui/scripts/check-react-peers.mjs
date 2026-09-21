/** Verify renderer peer identity across upstream DOM and fork-local Ink consumers. */
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'

const root = createRequire(new URL('../../package.json', import.meta.url))
const domTests = createRequire(root.resolve('@testing-library/react/package.json'))
const dom = createRequire(domTests.resolve('react-dom/package.json'))
assert.ok(domTests('react') === dom('react'), 'upstream DOM tests and React DOM must share React')
domTests('@testing-library/react')

for (const name of ['app', 'ui', 'harness']) {
  const consumer = createRequire(new URL(`../packages/${name}/package.json`, import.meta.url))
  const ink = createRequire(consumer.resolve('ink'))
  assert.ok(consumer('react') === ink('react'), `${name} and Ink must share React`)
}
console.log('React peers: upstream DOM and TUI renderers resolve matching instances')
