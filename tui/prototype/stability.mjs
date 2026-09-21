/**
 * Measure vertical stability of the dynamic region across a streaming turn.
 *
 * Ink erases the previous dynamic block and rewrites it each frame. When the
 * new block is taller than the old one the terminal scrolls to make room, and
 * everything on screen appears to jump; when it is shorter, lines are erased
 * from under the user's eyes. Either way the status line and composer move.
 *
 * Calm rendering therefore is not about draw speed, it is about the dynamic
 * region keeping a constant height while a turn runs. This script renders the
 * same streaming sequence twice and counts the movement.
 *
 *   node tui/prototype/stability.mjs
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const WIDTH = 80
const LIVE_BUDGET = 6

/** Tokens arriving over a turn, then a tool call, then more text. */
const STREAM = [
  ['assistant', 'Reading the controller.'],
  ['assistant', 'Two registrations go through ctx.effect, so the disposer unwinds both.'],
  ['tool', 'bash \u00b7 rg -n "commands.register"'],
  ['result', 'packages/app/src/controller.ts:45'],
  ['result', 'packages/app/src/controller.ts:52'],
  ['assistant', 'That accounts for /login and /help.'],
  ['assistant', 'Checking whether anything else registers late.'],
]

const Row = ({ kind, text }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: 2, flexShrink: 0 }, h(Text, { dimColor: kind !== 'assistant' }, kind === 'tool' ? '\u2699' : ' ')),
    h(Box, { flexGrow: 1 }, h(Text, { wrap: 'wrap' }, text)))

/**
 * One frame of the dynamic region.
 * @param reserve - hold the live area at its full budget instead of letting it grow.
 */
const Frame = ({ rows, reserve }) => {
  const shown = rows.slice(-LIVE_BUDGET)
  const padding = reserve ? LIVE_BUDGET - shown.length : 0
  return h(Box, { flexDirection: 'column', width: WIDTH },
    shown.map(([kind, text], index) => h(Row, { key: index, kind, text })),
    Array.from({ length: Math.max(0, padding) }, (_, index) => h(Text, { key: `pad${index}` }, ' ')),
    h(Text, { color: 'yellow' }, ' \u25cf working  \u00b7  deepseek/chat  \u00b7  ctx 12%  \u00b7  turn 4'),
    h(Box, { flexDirection: 'row' },
      h(Text, { color: 'cyan' }, '\u203a '), h(Text, { dimColor: true }, 'esc to interrupt')))
}

const heightOf = text => text.replace(/\u001B\[[0-9;]*m/g, '').split('\n').length

/** Render the turn frame by frame and report how much the frame height moves. */
function measure(reserve) {
  const heights = []
  for (let count = 1; count <= STREAM.length; count += 1) {
    const rows = STREAM.slice(0, count)
    heights.push(heightOf(renderToString(h(Frame, { rows, reserve }), { columns: WIDTH })))
  }
  let shifts = 0
  let travel = 0
  for (let index = 1; index < heights.length; index += 1) {
    const delta = heights[index] - heights[index - 1]
    if (delta !== 0) shifts += 1
    travel += Math.abs(delta)
  }
  return { heights, shifts, travel }
}

const growing = measure(false)
const reserved = measure(true)

const report = (name, result) => {
  console.log(`\n${name}`)
  console.log(`  frame heights : ${result.heights.join(' ')}`)
  console.log(`  height changes: ${result.shifts} of ${result.heights.length - 1} frames`)
  console.log(`  total travel  : ${result.travel} rows of screen movement`)
}

report('growing live region (status and composer move on every token)', growing)
report('reserved live region (status and composer pinned)', reserved)

console.log(`\nReserving the live area removes ${growing.travel - reserved.travel} rows of movement`
  + ` across ${STREAM.length} frames.`)
console.log(reserved.travel === 0
  ? 'Reserved mode is perfectly stable: zero movement while the turn runs.'
  : `Reserved mode still moves ${reserved.travel} rows \u2014 investigate.`)
process.exit(reserved.travel === 0 ? 0 : 1)
