/**
 * Visual prototype for the TUI layout.
 *
 * Renders every planned state to a string with Ink's own layout engine, so the
 * frames below are what the terminal would actually draw, not hand-drawn
 * mockups. Each scene is also measured against invariant L1 from
 * DESIGN-LAYOUT.md. The dynamic region must fit in `rows - 1` lines.
 *
 * Standalone by design. It resolves Ink and React out of packages/ui so it can
 * run while that package is being edited, and it imports no fork source.
 *
 *   node tui/prototype/frames.mjs [--width 80] [--rows 24] [--plain]
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Static, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const argv = process.argv.slice(2)
const flag = (name, fallback) => {
  const at = argv.indexOf(`--${name}`)
  return at === -1 ? fallback : Number(argv[at + 1])
}
const WIDTH = flag('width', 80)
const ROWS = flag('rows', 24)
const PLAIN = argv.includes('--plain')

/** Left rail. One glyph plus one space, constant across row kinds (DESIGN-LAYOUT §4). */
const GUTTER = {
  user: { glyph: '\u203a', color: undefined, bold: true },
  assistant: { glyph: ' ', color: undefined },
  tool: { glyph: '\u2699', color: 'gray' },
  result: { glyph: ' ', color: 'gray' },
  error: { glyph: '\u2717', color: 'red' },
  notice: { glyph: '\u2022', color: 'yellow' },
  ask: { glyph: '?', color: 'cyan' },
}

/**
 * One transcript or live row with its rail.
 *
 * The rail is a fixed two-column box and the content is a separate growing box,
 * so wrapped continuation lines hang at the text column instead of sliding back
 * under the glyph. Relying on flex shrink instead misaligns every wrapped line.
 */
const Row = ({ kind, text, dim, glyph }) => {
  const style = GUTTER[kind]
  return h(Box, { flexDirection: 'row' },
    h(Box, { width: 2, flexShrink: 0 },
      h(Text, { color: style.color, bold: style.bold ?? false }, glyph ?? style.glyph)),
    h(Box, { flexGrow: 1 },
      h(Text, { color: style.color, dimColor: dim ?? false, wrap: 'wrap' }, text)))
}

/** Committed history. Written once, never re-rendered, owned by the terminal. */
const Transcript = ({ rows }) =>
  h(Static, { items: rows }, (row, index) => h(Row, { key: index, ...row }))

/**
 * In-flight turn, tail-windowed. Clipped lines are not lost. They land in the
 * transcript when the turn commits (DESIGN-LAYOUT §2.2).
 */
const Live = ({ rows, budget }) => {
  if (rows.length === 0) return null
  const shown = rows.slice(-budget)
  return h(Box, { flexDirection: 'column' }, shown.map((row, index) => h(Row, { key: index, ...row })))
}

/** Modal question. Outranks the live region; never yields (DESIGN-LAYOUT §3). */
const Interaction = ({ title, detail, choices }) =>
  h(Box, { flexDirection: 'column', borderStyle: 'round', borderColor: 'cyan', paddingX: 1 },
    h(Text, { color: 'cyan', bold: true }, title),
    detail.map((line, index) => h(Text, { key: index, dimColor: true, wrap: 'truncate-end' }, line)),
    h(Text, null, choices.map((choice, index) =>
      h(Text, { key: index }, index === 0 ? '' : '   ',
        h(Text, { bold: true, color: 'cyan' }, choice.key),
        h(Text, null, ` ${choice.label}`)))))

/**
 * Short feedback only. A list of harness-owned length is bounded here and
 * committed to the transcript in full (DESIGN-LAYOUT §5).
 */
const Notice = ({ lines, budget, tone }) => {
  if (lines.length === 0) return null
  const fits = lines.length <= budget
  const shown = fits ? lines : lines.slice(0, budget - 1)
  const rest = lines.length - shown.length
  // One block. The glyph marks its start and does not repeat on every line.
  return h(Box, { flexDirection: 'column' },
    shown.map((line, index) =>
      h(Row, { key: index, kind: tone ?? 'notice', text: line, ...index === 0 ? {} : { glyph: ' ' } })),
    fits ? null : h(Row, { kind: 'notice', glyph: ' ', text: `+${rest} more \u2014 full list in scrollback`, dim: true }))
}

/** One line, fields dropped right-to-left as width shrinks (DESIGN-LAYOUT §2.5). */
const Status = ({ fields, width }) => {
  const kept = [...fields]
  const render = () => kept.map(field => field.text).join('  \u00b7  ')
  while (kept.length > 1 && render().length + 2 > width) kept.pop()
  return h(Box, { flexDirection: 'row' },
    h(Text, { color: kept[0].color ?? 'green' }, ` ${kept[0].text}`),
    kept.slice(1).map((field, index) =>
      h(Text, { key: index, dimColor: true }, `  \u00b7  ${field.text}`)))
}

/** Input. Grows to five rows, then scrolls internally. */
const Composer = ({ text, placeholder }) =>
  h(Box, { flexDirection: 'row' },
    h(Text, { color: 'cyan', bold: true }, '\u203a '),
    text === ''
      ? h(Text, { dimColor: true }, placeholder)
      : h(Text, null, text, h(Text, { inverse: true }, ' ')))

/** Compose the dynamic region in priority order. */
const Dynamic = ({ scene, width }) =>
  h(Box, { flexDirection: 'column', width },
    h(Live, { rows: scene.live ?? [], budget: scene.liveBudget ?? 10 }),
    scene.interaction ? h(Interaction, scene.interaction) : null,
    h(Notice, { lines: scene.notice ?? [], budget: scene.noticeBudget ?? 6, tone: scene.noticeTone }),
    h(Status, { fields: scene.status, width }),
    h(Composer, { text: scene.composer ?? '', placeholder: scene.placeholder ?? 'Ask anything, / for commands' }))

const status = (extra = {}) => [
  { text: extra.state ?? '\u25b8 ready', color: extra.color ?? 'green' },
  { text: 'deepseek/chat' },
  { text: extra.context ?? 'ctx 12%' },
  { text: extra.turn ?? 'turn 3' },
  { text: '~/projects/bake' },
  { text: '0f3a9c' },
]

const HISTORY = [
  { kind: 'user', text: 'Find where the session controller registers commands' },
  { kind: 'assistant', text: 'Reading the controller now.' },
  { kind: 'tool', text: 'bash \u00b7 rg -n "commands.register" -g \'*.ts\'' },
  { kind: 'result', text: 'packages/app/src/controller.ts:45', dim: true },
  { kind: 'result', text: 'packages/app/src/controller.ts:52', dim: true },
  { kind: 'assistant', text: 'Two registrations: /login and /help. Both go through ctx.effect so the disposer unwinds them.' },
]

const SCENES = [
  {
    name: 'idle \u2014 history committed, nothing in flight',
    transcript: HISTORY,
    status: status(),
  },
  {
    name: 'streaming \u2014 live region tail-windowed',
    transcript: HISTORY,
    live: [
      { kind: 'assistant', text: 'The registry is the list, so a command contributed by any plugin shows up without' },
      { kind: 'assistant', text: 'the terminal knowing it exists. That is why /help reads ctx.commands.list rather' },
      { kind: 'assistant', text: 'than a table this surface maintains by hand.' },
    ],
    status: status({ state: '\u25cf working', color: 'yellow', turn: 'turn 4 \u00b7 step 2' }),
    composer: '',
    placeholder: 'esc to interrupt',
  },
  {
    name: 'tool running',
    transcript: HISTORY,
    live: [
      { kind: 'tool', text: 'bash \u00b7 pnpm run build' },
      { kind: 'result', text: 'tsc -b packages/core/agent', dim: true },
      { kind: 'result', text: 'tsc -b packages/session/session', dim: true },
    ],
    status: status({ state: '\u25cf working', color: 'yellow', turn: 'turn 4 \u00b7 step 3' }),
    placeholder: 'esc to interrupt',
  },
  {
    name: 'approval \u2014 modal, live region collapsed to one line',
    transcript: HISTORY,
    live: [{ kind: 'assistant', text: 'Removing the generated output\u2026', dim: true }],
    liveBudget: 1,
    interaction: {
      title: 'Run a command?',
      detail: ['rm -rf packages/app/lib', 'in ~/projects/bake'],
      choices: [{ key: 'y', label: 'once' }, { key: 'a', label: 'always' }, { key: 'n', label: 'no' }],
    },
    status: status({ state: '\u25c6 waiting', color: 'cyan', turn: 'turn 4 \u00b7 step 4' }),
    placeholder: 'y / a / n',
  },
  {
    name: 'bounded notice \u2014 /model against a large catalog',
    transcript: HISTORY,
    notice: [
      '* deepseek/deepseek-chat   [low medium high]',
      '  deepseek/deepseek-reasoner   [medium high]',
      '  anthropic/claude-sonnet-4',
      '  anthropic/claude-opus-4',
      '  openai/gpt-5',
      '  openai/gpt-5-mini',
      '  google/gemini-2.5-pro',
      '  google/gemini-2.5-flash',
      '  mistral/mistral-large',
      '  meta/llama-4-maverick',
    ],
    status: status(),
  },
  {
    name: 'error',
    transcript: HISTORY,
    notice: ['tool call failed: UNKNOWN: Cannot read properties of undefined (reading \'prepare\')'],
    noticeTone: 'error',
    status: status({ state: '\u2717 failed', color: 'red' }),
  },
]

const NARROW = { name: 'narrow \u2014 40 columns, status drops fields', width: 40, ...SCENES[1] }

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')
const heightOf = text => (text === '' ? 0 : strip(text).split('\n').length)

let failures = 0
for (const scene of [...SCENES, NARROW]) {
  const width = scene.width ?? WIDTH
  const dynamic = renderToString(h(Dynamic, { scene, width }), { columns: width })
  const full = renderToString(
    h(Box, { flexDirection: 'column' },
      h(Transcript, { rows: scene.transcript }),
      h(Dynamic, { scene, width })),
    { columns: width })

  const dynamicHeight = heightOf(dynamic)
  const budget = ROWS - 1
  const ok = dynamicHeight <= budget
  if (!ok) failures += 1

  const rule = '\u2500'.repeat(width)
  console.log(`\n\u250c${rule}\u2510`)
  console.log(`  ${scene.name}`)
  console.log(`  ${width} cols \u00b7 dynamic ${dynamicHeight}/${budget} rows \u00b7 L1 ${ok ? 'ok' : 'VIOLATED'}`)
  console.log(`\u251c${rule}\u2524`)
  console.log(PLAIN ? strip(full) : full)
  console.log(`\u2514${rule}\u2518`)
}

console.log(failures === 0
  ? `\nL1 holds for every scene: dynamic region fits in ${ROWS - 1} rows`
  : `\n${failures} scene(s) violate L1`)
process.exit(failures === 0 ? 0 : 1)
