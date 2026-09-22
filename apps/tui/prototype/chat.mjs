/**
 * Spacing, zones, and render area for the chat transcript.
 *
 * Two rules do most of the work here. Prose gets a measured column even when
 * the terminal is wider, because a 160-character line is hard to track back to
 * its start; tool output does not, because wrapping a log or a diff to a narrow
 * measure destroys the alignment that makes it readable. A tool call and its
 * result are one zone, so they indent together and are not separated by
 * whitespace.
 *
 *   node tui/prototype/chat.mjs [--width 100]
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const argv = process.argv.slice(2)
const at = argv.indexOf('--width')

/**
 * Widest comfortable prose column.
 *
 * Long monospace lines are hard to scan back to the start of. Terminals get
 * arbitrarily wide, so prose is capped and tool output is not.
 */
const MEASURE = 88

/** Text column for a row kind: prose is measured, output takes what it needs. */
const measureFor = (width, kind) =>
  kind === 'output' ? width - 4 : Math.min(MEASURE, width - 2)

const Rail = ({ glyph, color, width = 2, children }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width, flexShrink: 0 }, h(Text, { color }, glyph)),
    h(Box, { flexShrink: 0 }, children))

/** A user turn opens with a blank line: the eye's landing place when scrolling back. */
const UserTurn = ({ text, width }) =>
  h(Box, { flexDirection: 'column' },
    h(Text, null, ' '),
    h(Rail, { glyph: '\u203a', color: 'cyan' },
      h(Box, { width: measureFor(width, 'prose') },
        h(Text, { bold: true, wrap: 'wrap' }, text))))

const Answer = ({ text, width }) =>
  h(Rail, { glyph: ' ' },
    h(Box, { width: measureFor(width, 'prose') }, h(Text, { wrap: 'wrap' }, text)))

/**
 * A call and its result are one zone.
 *
 * The result indents under the call rather than sitting beside it, so the group
 * reads as one unit without a separator row. Output keeps the full width.
 */
const ToolZone = ({ name, summary, output, failed, width }) =>
  h(Box, { flexDirection: 'column' },
    h(Rail, { glyph: '\u2699', color: failed ? 'red' : 'gray' },
      h(Text, { dimColor: !failed, color: failed ? 'red' : undefined }, `${name} \u00b7 ${summary}`)),
    output.map((line, index) =>
      h(Box, { key: index, flexDirection: 'row' },
        h(Box, { width: 4, flexShrink: 0 }, h(Text, null, ' ')),
        h(Box, { width: measureFor(width, 'output') },
          // Wrapped, never truncated: a result the user cannot finish reading is
          // worse than a ragged one. Failed output is not dimmed - it is the
          // thing they need.
          h(Text, { dimColor: !failed, wrap: 'wrap' }, line)))))

const PROSE = 'The registry is the list, so a command contributed by any plugin appears without'
  + ' the terminal knowing it exists. That is why discovery reads ctx.commands.list rather than'
  + ' a table this surface maintains by hand, which would drift the moment a plugin changed.'

const Turn = ({ width }) =>
  h(Box, { flexDirection: 'column', width },
    h(UserTurn, { text: 'Find where the session controller registers commands', width }),
    h(Answer, { text: PROSE, width }),
    h(ToolZone, {
      name: 'bash', summary: 'rg -n "commands.register" -g \'*.ts\'', width,
      output: [
        'packages/app/src/controller.ts:45      commands.register({ name: \'login\', description: copy.signIn })',
        'packages/app/src/controller.ts:52      commands.register({ name: \'help\', description: copy.listCommands })',
      ],
    }),
    h(ToolZone, {
      name: 'bash', summary: 'pnpm run typecheck', failed: true, width,
      output: ['tui/packages/app/src/model.ts(13,15): error TS2614: no exported member'],
    }),
    h(Answer, { text: 'Two registrations, both through ctx.effect.', width }))

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')

for (const width of at === -1 ? [80, 160] : [Number(argv[at + 1])]) {
  const rendered = strip(renderToString(h(Turn, { width }), { columns: width }))
  const lines = rendered.split('\n')
  const prose = lines.filter(line => /^[\u203a ] {0,1}\S/.test(line) && !line.startsWith('\u2699'))
  const longest = Math.max(...prose.map(line => line.trimEnd().length))
  console.log(`\n\u250c\u2500 ${width} columns ${'\u2500'.repeat(Math.max(0, width - 16))}`)
  console.log(rendered.replace(/^/gm, '\u2502 '))
  // The rail occupies two columns before the text box, so a full prose line is
  // the measure plus the rail.
  const limit = Math.min(MEASURE, width - 2) + 2
  console.log(`\u2514\u2500 rows ${lines.length} \u00b7 longest prose line ${longest} `
    + `(limit ${limit}) \u00b7 ${longest <= limit ? 'ok' : 'OVER'}`)
}

console.log(`
Prose stays inside ${MEASURE} columns however wide the terminal gets; tool output
takes the full width, because wrapping a log to a narrow measure breaks the
alignment that makes it scannable. A call and its result share one zone, so no
whitespace divides them and the indent does the grouping.`)
