/** Verify that each Bake terminal consumer shares Ink's React instance. */
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'

for (const name of ['app', 'ui', 'harness']) {
  const consumer = createRequire(new URL(`../packages/${name}/package.json`, import.meta.url))
  const ink = createRequire(consumer.resolve('ink'))
  assert.ok(consumer('react') === ink('react'), `${name} and Ink must share React`)
}

console.log('React peers: all TUI consumers share their renderer instance')
