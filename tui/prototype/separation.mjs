/**
 * Separating the chat area, the status line, and the composer.
 *
 * A terminal has few ways to say "this is a different kind of thing", and each
 * costs something. A blank line and a rule each cost a row, which L1 charges
 * against the same budget the live region needs \u2014 on a 10-row window one
 * separator row is a tenth of the screen. Color costs nothing but disappears
 * under NO_COLOR. Alignment and indentation cost nothing and survive both.
 *
 * This renders the candidates side by side with their row cost so the choice is
 * made on evidence rather than on taste alone.
 *
 *   node tui/prototype/separation.mjs
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const WIDTH = 72

const CHAT = [
  ['\u203a', 'Find where the session controller registers commands'],
  [' ', 'Two registrations go through ctx.effect, so the disposer unwinds both.'],
  ['\u2699', 'bash \u00b7 rg -n "commands.register"'],
  [' ', 'packages/app/src/controller.ts:45'],
]

const ChatRow = ({ glyph, text, dim }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: 2, flexShrink: 0 }, h(Text, { dimColor: glyph === '\u2699' }, glyph)),
    h(Box, { flexGrow: 1 }, h(Text, { dimColor: dim, wrap: 'wrap' }, text)))

const Chat = () => h(Box, { flexDirection: 'column' },
  CHAT.map(([glyph, text], index) => h(ChatRow, { key: index, glyph, text, dim: glyph === ' ' && index === 3 })))

/** Status as a sentence: reads like one more line of conversation. */
const StatusSentence = () =>
  h(Text, { dimColor: true }, ' \u25b8 ready  \u00b7  deepseek/chat  \u00b7  ctx 12%  \u00b7  turn 3')

/**
 * Status as a justified bar: a left cluster and a right cluster pushed apart.
 *
 * Nothing in a transcript is right-aligned, so the eye reads a filled row as
 * chrome before it reads a single word of it. Costs no rows and no color.
 */
const StatusBar = ({ width }) => {
  const left = '\u25b8 ready  \u00b7  deepseek/chat'
  const right = 'ctx 12%  \u00b7  turn 3  \u00b7  0f3a9c'
  const gap = Math.max(1, width - left.length - right.length)
  return h(Text, null,
    h(Text, { color: 'green' }, left),
    h(Text, null, ' '.repeat(gap)),
    h(Text, { dimColor: true }, right))
}

const Composer = ({ glyph, color }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: 2, flexShrink: 0 }, h(Text, { color, bold: true }, glyph)),
    h(Box, { flexGrow: 1 }, h(Text, { dimColor: true }, 'Ask anything, / for commands')))

const Rule = ({ width }) => h(Text, { dimColor: true }, '\u2500'.repeat(width))

const VARIANTS = {
  'A \u2014 nothing (status reads as one more chat row)': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Chat), h(StatusSentence), h(Composer, { glyph: '\u203a', color: 'cyan' })),

  'B \u2014 blank line': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Chat), h(Text, null, ' '), h(StatusSentence), h(Composer, { glyph: '\u203a', color: 'cyan' })),

  'C \u2014 full rule': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Chat), h(Rule, { width }), h(StatusSentence), h(Composer, { glyph: '\u203a', color: 'cyan' })),

  'D \u2014 justified status bar, no extra rows': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Chat), h(StatusBar, { width }), h(Composer, { glyph: '\u276f', color: 'cyan' })),

  // Rows inside the transcript are charged to scrollback, which is unbounded,
  // not to the dynamic region L1 caps. Spacing is cheap here and costly below.
  'F \u2014 D, plus a blank line opening each turn in the chat area': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Box, { flexDirection: 'column' },
        h(ChatRow, { glyph: '\u203a', text: 'Find where the session controller registers commands' }),
        h(ChatRow, { glyph: ' ', text: 'Two registrations go through ctx.effect, so the disposer unwinds both.' }),
        h(ChatRow, { glyph: '\u2699', text: 'bash \u00b7 rg -n "commands.register"' }),
        h(ChatRow, { glyph: ' ', text: 'packages/app/src/controller.ts:45', dim: true }),
        h(Text, null, ' '),
        h(ChatRow, { glyph: '\u203a', text: 'Now check the picker' }),
        h(ChatRow, { glyph: ' ', text: 'Reading packages/ui/src/picker.tsx.' })),
      h(StatusBar, { width }), h(Composer, { glyph: '\u276f', color: 'cyan' })),

  'E \u2014 bar plus a composer that owns the bottom edge': ({ width }) =>
    h(Box, { flexDirection: 'column', width },
      h(Chat), h(StatusBar, { width }),
      h(Box, { flexDirection: 'row' },
        h(Box, { width: 2, flexShrink: 0 }, h(Text, { color: 'cyan', bold: true }, '\u276f')),
        h(Box, { flexGrow: 1 }, h(Text, { dimColor: true }, 'Ask anything, / for commands')),
        h(Box, { flexShrink: 0 }, h(Text, { dimColor: true }, '\u21b5 send')))),
}

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')
const heightOf = text => strip(text).split('\n').length
const baseline = heightOf(renderToString(h(Chat), { columns: WIDTH }))

for (const [name, Variant] of Object.entries(VARIANTS)) {
  const rendered = renderToString(h(Variant, { width: WIDTH }), { columns: WIDTH })
  const rows = heightOf(rendered)
  console.log(`\n\u250c\u2500 ${name} ${'\u2500'.repeat(Math.max(0, WIDTH - name.length - 4))}`)
  console.log(strip(rendered).replace(/^/gm, '\u2502 '))
  console.log(`\u2514\u2500 chrome cost: ${rows - baseline} rows beyond the chat area`)
}

console.log(`
Row cost is charged against the same budget the live region spends, so a
separator row is not free: at 10 rows it is a tenth of the screen. Alignment
and glyph weight cost nothing and survive NO_COLOR, which is why D and E carry
the separation without spending rows on it.`)
