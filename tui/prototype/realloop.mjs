/**
 * Validate L1 and L2 against Ink's real render loop.
 *
 * `frames.mjs` and `stability.mjs` measure what Yoga lays out. This script
 * measures what Ink actually writes: it drives a live `render()` through a fake
 * TTY, captures every byte, and checks the claims that matter on screen.
 *
 *   1. a committed transcript row is written exactly once, however many times
 *      the live region updates  (the O(1) append guarantee)
 *   2. no full-screen clear during an ordinary turn  (L1)
 *   3. frame height never changes while a turn runs  (L2)
 *
 *   node tui/prototype/realloop.mjs
 */
import { EventEmitter } from 'node:events'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const ink = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default
const { Box, Static, Text, render } = ink

const h = React.createElement
const COLUMNS = 80
const ROWS = 24
const LIVE_BUDGET = 6
const sleep = ms => new Promise(resolve => { setTimeout(resolve, ms) })

/** A terminal Ink will treat as real, recording everything written to it. */
function fakeTty() {
  const stdout = new EventEmitter()
  stdout.chunks = []
  stdout.isTTY = true
  stdout.columns = COLUMNS
  stdout.rows = ROWS
  stdout.write = chunk => { stdout.chunks.push(String(chunk)); return true }
  stdout.getWindowSize = () => [COLUMNS, ROWS]

  const stdin = new EventEmitter()
  stdin.isTTY = true
  stdin.setRawMode = () => stdin
  stdin.setEncoding = () => stdin
  stdin.resume = () => stdin
  stdin.pause = () => stdin
  stdin.ref = () => stdin
  stdin.unref = () => stdin
  stdin.read = () => null
  return { stdout, stdin }
}

/** External store so the turn can be driven from outside React. */
function createStore(initial) {
  let state = initial
  const listeners = new Set()
  return {
    set(next) { state = next; for (const listener of listeners) listener() },
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) },
    snapshot: () => state,
  }
}

const TOKENS = [
  'Reading the controller.',
  'Two registrations go through ctx.effect.',
  'Both unwind when the disposer runs.',
  'Checking for late registrations.',
  'None found.',
]
const COMMITTED = ['COMMITTED-ROW-ALPHA', 'COMMITTED-ROW-BETA']
/** A transcript of `count` rows, the first two being the markers we count. */
const transcriptOf = count => [
  ...COMMITTED,
  ...Array.from({ length: Math.max(0, count - COMMITTED.length) }, (_, index) => `history row ${index}`),
]

const App = ({ store, reserve }) => {
  const state = React.useSyncExternalStore(store.subscribe, store.snapshot, store.snapshot)
  const shown = state.overflow === true ? state.live : state.live.slice(-LIVE_BUDGET)
  const padding = reserve && state.running ? LIVE_BUDGET - shown.length : 0
  return h(Box, { flexDirection: 'column' },
    h(Static, { items: state.committed }, (row, index) => h(Text, { key: index }, row)),
    h(Box, { flexDirection: 'column' },
      shown.map((line, index) => h(Text, { key: index }, line)),
      Array.from({ length: Math.max(0, padding) }, (_, index) => h(Text, { key: `pad${index}` }, ' ')),
      h(Text, { color: 'yellow' }, state.running ? '\u25cf working' : '\u25b8 ready'),
      h(Text, { dimColor: true }, '\u203a esc to interrupt')))
}

/**
 * Drive one streaming turn and return the captured byte stream.
 * @param overflow - pad the live region past the viewport to exercise §1.
 */
async function runTurn(reserve, overflow = false, transcriptRows = COMMITTED.length) {
  const { stdout, stdin } = fakeTty()
  const filler = overflow
    ? Array.from({ length: ROWS + 6 }, (_, index) => `overflow line ${index}`)
    : []
  const store = createStore({ committed: transcriptOf(transcriptRows), live: filler, running: true })
  if (overflow) store.set({ ...store.snapshot(), overflow: true })
  const instance = render(h(App, { store, reserve }), {
    stdout, stdin, exitOnCtrlC: false, patchConsole: false,
  })
  await sleep(20)
  const boundaries = [stdout.chunks.length]
  for (const token of TOKENS) {
    store.set({ ...store.snapshot(), live: [...store.snapshot().live, token] })
    await sleep(20)
    boundaries.push(stdout.chunks.length)
  }
  store.set({ ...store.snapshot(), running: false })
  await sleep(20)
  instance.unmount()
  await sleep(10)
  return { stream: stdout.chunks.join(''), chunks: stdout.chunks, boundaries }
}

const CLEAR_SCREEN = '\u001B[2J'
const countOf = (text, needle) => text.split(needle).length - 1

/** Frame height Ink erased before redrawing, read off its cursor-up moves. */
function frameHeights(chunks) {
  const heights = []
  for (const chunk of chunks) {
    const ups = countOf(chunk, '\u001B[1A')
    if (ups > 0) heights.push(ups + 1)
  }
  return heights
}

function report(name, result) {
  const committedWrites = COMMITTED.map(row => countOf(result.stream, row))
  const clears = countOf(result.stream, CLEAR_SCREEN)
  const heights = frameHeights(result.chunks)
  const distinct = [...new Set(heights)]
  console.log(`\n${name}`)
  console.log(`  bytes written      : ${result.stream.length}`)
  console.log(`  committed row draws: ${committedWrites.join(', ')} (want 1, 1)`)
  console.log(`  full-screen clears : ${clears} (want 0)`)
  console.log(`  erased heights     : ${heights.join(' ') || '(none)'}`)
  console.log(`  distinct heights   : ${distinct.length} \u2192 ${distinct.length <= 1 ? 'stable' : 'JUMPS'}`)
  return { committedWrites, clears, distinct }
}

const growing = report('growing live region', await runTurn(false))
const reserved = report('reserved live region', await runTurn(true))

// §1 says a dynamic region taller than the viewport makes Ink clear the screen
// and replay the whole transcript every frame. Demonstrated rather than
// asserted from source, so the cost of violating L1 stays visible.
const overflowed = report(`overflowing live region (${ROWS + 6} lines in ${ROWS} rows)`, await runTurn(false, true))
console.log(overflowed.clears > 0 && overflowed.committedWrites.some(count => count > 1)
  ? '  → L1 violation reproduced: screen cleared and transcript replayed per frame'
  : '  → no violation observed; §1 needs revisiting')

const failures = []
for (const [name, result] of [['growing', growing], ['reserved', reserved]]) {
  if (result.committedWrites.some(count => count !== 1)) failures.push(`${name}: transcript row redrawn`)
  if (result.clears !== 0) failures.push(`${name}: full-screen clear during a turn`)
}
if (reserved.distinct.length > 1) failures.push('reserved: frame height changed while running')
if (!(overflowed.clears > 0)) failures.push('overflow case did not reproduce the documented §1 behavior')

// Cost of violating L1 is a function of transcript length, not of the turn.
console.log('\nbytes written for the same turn, by transcript length:')
console.log('  transcript   compliant   overflowing   ratio')
for (const rows of [2, 50, 200]) {
  const ok = (await runTurn(true, false, rows)).stream.length
  const bad = (await runTurn(false, true, rows)).stream.length
  console.log(`  ${String(rows).padStart(9)}   ${String(ok).padStart(9)}   ${String(bad).padStart(11)}`
    + `   ${(bad / ok).toFixed(1)}\u00d7`)
}

console.log(failures.length === 0
  ? '\nReal render loop agrees with the prototype: transcript written once, no clears, reserved mode stable.'
  : `\n${failures.length} failure(s):\n  ${failures.join('\n  ')}`)
process.exit(failures.length === 0 ? 0 : 1)
