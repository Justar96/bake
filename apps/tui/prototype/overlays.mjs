/**
 * Overlay geometry for completion popups and choice pickers.
 *
 * Both draw from unbounded sources — every registered command, every skill,
 * every file in the workspace — into the dynamic region, which L1 caps at
 * `rows - 1`. So the item limit is not a constant, it is a function of the
 * terminal height and of the chrome around the overlay. This script derives
 * that number, renders the result, and checks two defects that show up only
 * after the overlay is drawn.
 *
 *   1. the overlay fits at every terminal height down to 10 rows  (L1)
 *   2. its height does not change as it moves through loading, loaded, empty
 *      and error states  (L2)
 *
 *   node tui/prototype/overlays.mjs
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const WIDTH = 80

/** Rows the overlay cannot use. Status, composer, and one line of breathing room. */
const CHROME_ROWS = 3

/**
 * Largest item count an overlay may show at this terminal height.
 *
 * Derived, never hardcoded. A constant that fits an 80x24 window overflows a
 * split pane, and overflow is the one failure that clears the user's screen.
 *
 * @param rows - terminal height.
 * @param hasHeader - whether the overlay draws a title line.
 * @returns item limit, at least one.
 */
export const itemLimit = (rows, hasHeader) =>
  Math.max(1, rows - CHROME_ROWS - (hasHeader ? 1 : 0) - 1)

/**
 * One overlay row. A selection marker, a name, and a dim description.
 *
 * A path truncates from the start, because its tail is what distinguishes it —
 * truncating the end of `packages/app/src/module-7.ts` hides the only part the
 * user is reading. A name with a description keeps a fixed name column so the
 * descriptions align; a path without one takes the full width.
 */
const Item = ({ choice, selected, path }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: 2, flexShrink: 0 },
      h(Text, { color: selected ? 'cyan' : undefined }, selected ? '\u276f' : ' ')),
    path
      ? h(Box, { flexGrow: 1 },
        h(Text, {
          bold: selected, color: selected ? 'cyan' : undefined, wrap: 'truncate-start',
        }, choice.name))
      : h(React.Fragment, null,
        h(Box, { width: 24, flexShrink: 0 },
          h(Text, { bold: selected, color: selected ? 'cyan' : undefined, wrap: 'truncate-end' }, choice.name)),
        h(Box, { flexGrow: 1 },
          h(Text, { dimColor: true, wrap: 'truncate-end' }, choice.description ?? ''))))

/**
 * Anchored overlay above the composer.
 *
 * Height is held constant across states so the composer never moves while the
 * user is typing into it — the whole point of an inline completion.
 */
const Overlay = ({ title, state, items, limit, selected, width, hold, path }) => {
  const shown = state === 'loaded' ? items.slice(0, limit) : []
  const message = state === 'loading' ? 'searching\u2026'
    : state === 'empty' ? 'no matches'
      : state === 'error' ? 'catalog unavailable' : undefined
  const bodyRows = state === 'loaded' ? shown.length : 1
  // Hold the last loaded height through a transient re-query only. Reserving the
  // full limit would leave a 20-row hole under three matches.
  const padding = hold === undefined ? 0 : Math.max(0, hold - bodyRows)
  const overflow = state === 'loaded' && items.length > limit

  return h(Box, { flexDirection: 'column', width },
    title === undefined ? null : h(Text, { dimColor: true }, title),
    message === undefined
      ? shown.map((choice, index) =>
        h(Item, { key: index, choice, selected: index === selected, path }))
      : h(Box, { flexDirection: 'row' },
        h(Box, { width: 2, flexShrink: 0 }, h(Text, { dimColor: true }, ' ')),
        h(Text, { dimColor: true, color: state === 'error' ? 'red' : undefined }, message)),
    Array.from({ length: Math.max(0, padding) }, (_, index) => h(Text, { key: `pad${index}` }, ' ')),
    overflow ? h(Text, { dimColor: true }, `  +${items.length - limit} more \u2014 keep typing to narrow`) : null)
}

const COMMANDS = [
  { name: '/help', description: 'List available commands' },
  { name: '/login', description: 'Sign in to a provider' },
  { name: '/model', description: 'List or select the model' },
  { name: '/compact', description: 'Compact the conversation' },
  { name: '/goal', description: 'Set the session goal' },
  { name: '/plan', description: 'Record a plan' },
  { name: '/todo', description: 'Manage the todo list' },
  { name: '/skills', description: 'List loaded skills' },
]
const FILES = Array.from({ length: 40 }, (_, index) => ({
  name: `packages/app/src/module-${index}.ts`, description: 'workspace file',
}))

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')
const heightOf = text => (text === '' ? 0 : strip(text).split('\n').length)

const failures = []

console.log('item limit by terminal height (chrome reserves'
  + ` ${CHROME_ROWS} rows, header one more):`)
console.log('  rows   no header   with header')
for (const rows of [10, 16, 24, 40, 60]) {
  console.log(`  ${String(rows).padStart(4)}   ${String(itemLimit(rows, false)).padStart(9)}`
    + `   ${String(itemLimit(rows, true)).padStart(11)}`)
}

// L1. The overlay must fit at every height, including the smallest supported.
for (const rows of [10, 16, 24, 40]) {
  const limit = itemLimit(rows, true)
  const rendered = renderToString(
    h(Overlay, {
      title: 'files matching "mod"', state: 'loaded', items: FILES,
      limit, selected: 0, width: WIDTH, reserve: true,
    }), { columns: WIDTH })
  const height = heightOf(rendered)
  const budget = rows - CHROME_ROWS
  if (height > budget) failures.push(`overlay of ${height} rows exceeds ${budget} at ${rows}-row terminal`)
}
console.log(`\nL1: overlay fits within its budget at 10, 16, 24 and 40 rows \u2014 `
  + `${failures.length === 0 ? 'ok' : 'VIOLATED'}`)

// L2. Height must not move as the overlay changes state under the user.
const limit = itemLimit(24, true)
const states = ['loading', 'loaded', 'empty', 'error']
const measure = hold => states.map(state => heightOf(renderToString(
  h(Overlay, {
    title: 'commands', state, items: COMMANDS, limit, selected: 0, width: WIDTH, hold,
  }), { columns: WIDTH })))
const loose = measure(undefined)
const held = measure(COMMANDS.length)
if (new Set(held).size > 1) failures.push('held overlay changes height across states')

console.log('\nheight across loading \u2192 loaded \u2192 empty \u2192 error:')
console.log(`  unheld : ${loose.join(' ')}  \u2192 ${new Set(loose).size > 1 ? 'moves' : 'stable'}`)
console.log(`  held   : ${held.join(' ')}  \u2192 ${new Set(held).size > 1 ? 'moves' : 'stable'}`)
console.log('  (held to the last loaded row count, not to the limit)')

/**
 * Draw a scene with the overlay below the composer.
 *
 * Anchoring it under the input is what keeps the caret still. Results change on
 * every keystroke, so an overlay above the composer would push the line the
 * user is typing into up and down as they type.
 */
const show = (label, props) => {
  const rendered = renderToString(h(Overlay, { width: WIDTH, ...props }), { columns: WIDTH })
  console.log(`\n\u250c\u2500 ${label} ${'\u2500'.repeat(Math.max(0, WIDTH - label.length - 4))}`)
  console.log('\u2502 \u25b8 ready  \u00b7  deepseek/chat  \u00b7  ctx 12%')
  console.log(`\u2502 \u203a ${props.draft ?? '/mo'}\u2588`)
  console.log(strip(rendered).replace(/^/gm, '\u2502 '))
}

show('command completion \u2014 typed "/mo"', {
  title: undefined, state: 'loaded', items: COMMANDS.slice(0, 3), limit, selected: 0,
})
show('file reference \u2014 40 matches, paths truncate from the start', {
  title: 'files matching "mod"', state: 'loaded', items: FILES, limit: 5, selected: 1,
  path: true, draft: 'read @mod',
})
show('catalog unavailable', {
  title: 'commands', state: 'error', items: [], limit: 5, selected: 0,
})

console.log(failures.length === 0
  ? '\nOverlays honor L1 at every supported height and L2 across every state.'
  : `\n${failures.length} failure(s):\n  ${failures.join('\n  ')}`)
process.exit(failures.length === 0 ? 0 : 1)
