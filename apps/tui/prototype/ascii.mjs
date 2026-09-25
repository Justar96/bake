/**
 * ASCII-only render vocabulary.
 *
 * Symbol glyphs are a measurement risk, not just a style choice. `string-width`
 * reports U+2699 as one cell and U+2699 U+FE0F as two, and a terminal with an
 * emoji font may draw the bare codepoint double-width anyway. Ink measures with
 * the same library, so when the terminal disagrees every column after the glyph
 * shifts and nothing in the layout engine can see it. Characters below 0x80
 * cannot disagree.
 *
 * Actions are named, not drawn as icons. A fixed verb column holds the name.
 * Arguments and output align under it. Spacing does the work an icon would have done.
 *
 *   node tui/prototype/ascii.mjs
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const WIDTH = 78
/** Verb column width, including its trailing gap. Output aligns to this. */
const VERB = 7
const PROSE = 2

/**
 * One verb row. A named action, its argument, and any output beneath it.
 *
 * The verb is the marker. `run`, `read`, `think`, and `error` are readable at
 * a glance, and they survive every font, every locale, and a paste into a bug report.
 */
const Verb = ({ verb, arg, output = [], tone }) =>
  h(Box, { flexDirection: 'column' },
    h(Box, { flexDirection: 'row' },
      h(Box, { width: PROSE, flexShrink: 0 }, h(Text, null, ' ')),
      h(Box, { width: VERB, flexShrink: 0 },
        h(Text, { color: tone, dimColor: tone === undefined }, verb)),
      h(Box, { flexGrow: 1 }, h(Text, { color: tone, wrap: 'wrap' }, arg))),
    output.map((line, index) =>
      h(Box, { key: index, flexDirection: 'row' },
        h(Box, { width: PROSE + VERB, flexShrink: 0 }, h(Text, null, ' ')),
        h(Box, { flexGrow: 1 },
          h(Text, { dimColor: tone === undefined, color: tone, wrap: 'wrap' }, line)))))

const Said = ({ text }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: PROSE, flexShrink: 0 }, h(Text, { bold: true }, '>')),
    h(Box, { flexGrow: 1 }, h(Text, { bold: true, wrap: 'wrap' }, text)))

const Answer = ({ text }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: PROSE, flexShrink: 0 }, h(Text, null, ' ')),
    h(Box, { flexGrow: 1 }, h(Text, { wrap: 'wrap' }, text)))

/** Left cluster and right cluster, separated by spacing, not by punctuation. */
const StatusBar = ({ width, state, color }) => {
  const left = `${state}   deepseek/chat`
  const right = 'ctx 12%   turn 3   0f3a9c'
  return h(Text, null,
    h(Text, { color }, left),
    h(Text, null, ' '.repeat(Math.max(1, width - left.length - right.length))),
    h(Text, { dimColor: true }, right))
}

const Composer = ({ text, hint }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: PROSE, flexShrink: 0 }, h(Text, { bold: true, color: 'cyan' }, '>')),
    h(Box, { flexGrow: 1 }, h(Text, { dimColor: text === undefined }, text ?? 'Ask anything, / for commands')),
    hint === undefined ? null : h(Box, { flexShrink: 0 }, h(Text, { dimColor: true }, hint)))

/**
 * One completion row.
 *
 * A list is not a verb row. The marker column holds the selection, and the name
 * column aligns the descriptions. Reusing the verb column here would indent
 * every item twice for no reason.
 */
const Item = ({ name, description, selected }) =>
  h(Box, { flexDirection: 'row' },
    // `*` marks the selection, never `>`. The composer prompt owns that glyph,
    // and two identical markers a row apart look like one list.
    h(Box, { width: PROSE, flexShrink: 0 },
      h(Text, { color: 'cyan', bold: true }, selected === true ? '*' : ' ')),
    h(Box, { width: 10, flexShrink: 0 },
      h(Text, { bold: selected === true, color: selected === true ? 'cyan' : undefined }, name)),
    description === undefined ? null : h(Box, { flexGrow: 1 }, h(Text, { dimColor: true }, description)))

/** Full-width footer under a list; not an item, so it does not take the name column. */
const More = ({ text }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width: PROSE, flexShrink: 0 }, h(Text, null, ' ')),
    h(Box, { flexGrow: 1 }, h(Text, { dimColor: true }, text)))

const Frame = ({ width, children }) => h(Box, { flexDirection: 'column', width }, children)

const scenes = {
  'a turn: said, thought, acted, answered': ({ width }) =>
    h(Frame, { width },
      h(Text, null, ' '),
      h(Said, { text: 'Find where the session controller registers commands' }),
      h(Verb, { verb: 'think', arg: 'The registry is the list, so discovery should read it.' }),
      h(Verb, {
        verb: 'run', arg: 'rg -n "commands.register" -g \'*.ts\'',
        output: ['packages/app/src/controller.ts:45', 'packages/app/src/controller.ts:52'],
      }),
      h(Verb, { verb: 'read', arg: 'packages/app/src/controller.ts' }),
      h(Answer, { text: 'Two registrations, both through ctx.effect.' }),
      h(StatusBar, { width, state: 'ready', color: 'green' }),
      h(Composer, {})),

  'working, with a failure and a pending question': ({ width }) =>
    h(Frame, { width },
      h(Verb, {
        verb: 'run', arg: 'pnpm run typecheck', tone: 'red',
        output: ['tui/packages/app/src/model.ts(13,15): error TS2614: no exported member'],
      }),
      h(Verb, { verb: 'ask', arg: 'Run rm -rf packages/app/lib?', tone: 'cyan', output: ['y once   a always   n no'] }),
      h(StatusBar, { width, state: 'waiting', color: 'cyan' }),
      h(Composer, { text: 'y', hint: 'enter to send' })),

  'completion below the composer': ({ width }) =>
    h(Frame, { width },
      h(StatusBar, { width, state: 'ready', color: 'green' }),
      h(Composer, { text: '/mo' }),
      h(Item, { name: '/model', description: 'List or select the model', selected: true }),
      h(Item, { name: '/compact', description: 'Compact the conversation' }),
      h(More, { text: '+6 more, keep typing to narrow' })),
}

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')
let nonAscii = []

for (const [name, Scene] of Object.entries(scenes)) {
  const rendered = strip(renderToString(h(Scene, { width: WIDTH }), { columns: WIDTH }))
  console.log(`\n+- ${name} ${'-'.repeat(Math.max(0, WIDTH - name.length - 4))}`)
  console.log(rendered.replace(/^/gm, '| '))
  for (const char of rendered) {
    if (char.codePointAt(0) > 0x7f && !nonAscii.includes(char)) nonAscii.push(char)
  }
}

console.log(`\nevery rendered character below 0x80: ${nonAscii.length === 0 ? 'yes' : `no - found ${JSON.stringify(nonAscii)}`}`)
console.log(`verb column ${VERB} wide, prose at ${PROSE}, output aligned under the argument`)
process.exit(nonAscii.length === 0 ? 0 : 1)
