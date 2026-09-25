/**
 * Reasoning stream and tool-use presentation.
 *
 * Models the real harness stream. `agent/assistant-stream` delivers
 * `AssistantStreamFrame`s carrying `StreamChunk`s, which separate
 * `reasoning-delta` from `text-delta` and stream tool arguments as partial JSON
 * in `tool-call-delta`. Three properties of that stream decide the rendering,
 * and all three fail only after a frame has already been drawn.
 *
 *   1. `revision` restarts at 1 when a stream is replaced, so a retry must
 *      clear what the previous attempt drew instead of appending to it
 *   2. `end.outcome.kind === 'committed'` means the durable session event now
 *      owns that text, so the live copy must go or it draws twice
 *   3. `end.outcome.kind === 'abandoned'` commits nothing, so the live copy
 *      must go and nothing may reach the transcript
 *
 *   node tui/prototype/stream.mjs
 */
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const require = createRequire(new URL('../packages/ui/package.json', import.meta.url))
const { Box, Text, renderToString } = await import(pathToFileURL(require.resolve('ink')).href)
const React = (await import(pathToFileURL(require.resolve('react')).href)).default

const h = React.createElement
const WIDTH = 80

/** Live view derived from the stream. Nothing here is durable. */
const empty = () => ({ revision: 0, reasoning: '', text: '', calls: new Map(), committed: [] })

/**
 * Fold one stream frame into the live view.
 *
 * Pure and total. The same frames in the same order always give the same view.
 * That is what makes the retry and abandon paths testable without a model.
 *
 * @param view - current live view.
 * @param frame - one `AssistantStreamFrame`.
 * @returns the next live view.
 */
export function reduce(view, frame) {
  switch (frame.type) {
    case 'start':
      // A replacement restarts the revision; drop whatever the last attempt drew.
      return { ...empty(), revision: frame.revision, committed: view.committed }

    case 'chunk': {
      const chunk = frame.chunk
      if (frame.revision !== view.revision) return view
      switch (chunk.type) {
        case 'reasoning-delta':
          return { ...view, reasoning: view.reasoning + chunk.text }
        case 'text-delta':
          return { ...view, text: view.text + chunk.text }
        case 'tool-call-delta': {
          // Arguments arrive as partial JSON. Keep the name and the fact of the
          // call; never render a half-parsed object at the user.
          const calls = new Map(view.calls)
          const existing = calls.get(chunk.id) ?? { name: undefined, state: 'streaming', summary: undefined }
          calls.set(chunk.id, { ...existing, name: chunk.name ?? existing.name })
          return { ...view, calls }
        }
        case 'block-end': {
          if (chunk.block?.type !== 'tool-call') return view
          const calls = new Map(view.calls)
          const existing = calls.get(chunk.block.id) ?? { name: chunk.block.name, state: 'streaming' }
          calls.set(chunk.block.id, {
            ...existing, name: chunk.block.name, state: 'ready', summary: summarize(chunk.block),
          })
          return { ...view, calls }
        }
        default:
          return view
      }
    }

    case 'end':
      // Committed text belongs to the session log now; abandoned text belongs
      // to no one. Either way the live copy goes.
      return frame.outcome.kind === 'committed'
        ? { ...empty(), committed: [...view.committed, summaryOf(view)] }
        : { ...empty(), committed: view.committed }

    default:
      return view
  }
}

/**
 * One-line argument summary for a completed tool call.
 *
 * Presenters stay pure and tool-specific. `bash` is shown as its command, and
 * a file tool as its path. A generic JSON dump is the fallback, not the design.
 *
 * @param block - the completed tool-call content block.
 * @returns a single display line, or undefined when nothing reads better than the name.
 */
function summarize(block) {
  const args = block.arguments ?? {}
  if (typeof args.command === 'string') return args.command
  if (typeof args.pattern === 'string') return args.pattern
  if (typeof args.path === 'string') return args.path
  const keys = Object.keys(args)
  return keys.length === 0 ? undefined : `${keys.length} argument${keys.length === 1 ? '' : 's'}`
}

/** What the transcript keeps once a stream commits. */
const summaryOf = view => ({
  reasoningChars: view.reasoning.length,
  text: view.text,
  calls: [...view.calls.values()].filter(call => call.state === 'ready'),
})

/**
 * Rail plus content.
 *
 * `width` carries the distinction. Reasoning sits at column 4 and the answer
 * at column 2, so the two remain distinguishable when dim is unavailable.
 * Relying on `dimColor` alone would merge them under NO_COLOR and for a screen reader.
 */
const Rail = ({ glyph, color, width = 2, children }) =>
  h(Box, { flexDirection: 'row' },
    h(Box, { width, flexShrink: 0 }, h(Text, { color }, glyph)),
    h(Box, { flexGrow: 1 }, children))

/**
 * The live region during a streaming step.
 *
 * Reasoning is dim and tail-windowed. It is worth watching while it happens
 * and not worth re-reading afterwards. The answer text below it is the thing the
 * user is waiting for, so it is undimmed and never clipped from the top.
 */
const LiveStream = ({ view, reasoningRows }) => {
  const reasoningLines = view.reasoning.split('\n').filter(line => line !== '')
  const tail = reasoningLines.slice(-reasoningRows)
  return h(Box, { flexDirection: 'column', width: WIDTH },
    tail.length === 0 ? null : h(Rail, { glyph: '\u2234', color: 'gray', width: 4 },
      h(Box, { flexDirection: 'column' },
        tail.map((line, index) => h(Text, { key: index, dimColor: true, wrap: 'wrap' }, line)))),
    view.text === '' ? null : h(Rail, { glyph: ' ' }, h(Text, { wrap: 'wrap' }, view.text)),
    [...view.calls.entries()].map(([id, call]) =>
      h(Rail, { key: id, glyph: '\u2699', color: 'gray' },
        h(Text, { dimColor: true },
          call.state === 'streaming'
            ? `${call.name ?? 'tool'} \u00b7 \u2026`
            : `${call.name} \u00b7 ${call.summary ?? ''}`))))
}

/** What lands in scrollback. The answer, a reasoning summary, and each call. */
const Committed = ({ entry, seconds }) =>
  h(Box, { flexDirection: 'column', width: WIDTH },
    entry.reasoningChars === 0 ? null : h(Rail, { glyph: '\u2234', color: 'gray', width: 4 },
      h(Text, { dimColor: true }, `thought for ${seconds}s`)),
    entry.text === '' ? null : h(Rail, { glyph: ' ' }, h(Text, { wrap: 'wrap' }, entry.text)),
    entry.calls.map((call, index) =>
      h(Rail, { key: index, glyph: '\u2699', color: 'gray' },
        h(Text, { dimColor: true }, `${call.name} \u00b7 ${call.summary ?? ''}`))))

const strip = text => text.replace(/\u001B\[[0-9;]*m/g, '')
const draw = (label, node) => {
  console.log(`\n\u250c\u2500 ${label} ${'\u2500'.repeat(Math.max(0, WIDTH - label.length - 4))}`)
  console.log(strip(renderToString(node, { columns: WIDTH })).replace(/^/gm, '\u2502 '))
}

const chunk = (revision, body) => ({ type: 'chunk', revision, chunk: body })
const REASONING = [
  'The registry is the list, so discovery should read it.',
  'Checking whether anything registers commands late.',
]

// A normal step. Reasoning, then answer text, then a tool call.
let view = reduce(empty(), { type: 'start', revision: 1 })
for (const line of REASONING) view = reduce(view, chunk(1, { type: 'reasoning-delta', text: `${line}\n` }))
view = reduce(view, chunk(1, { type: 'text-delta', text: 'Two plugins register commands. Checking for a third.' }))
view = reduce(view, chunk(1, { type: 'tool-call-delta', id: 'call-1', name: 'bash', argumentsDelta: '{"comm' }))
draw('streaming \u2014 reasoning dim, answer plain, tool arguments still partial',
  h(LiveStream, { view, reasoningRows: 3 }))

view = reduce(view, chunk(1, {
  type: 'block-end',
  block: { type: 'tool-call', id: 'call-1', name: 'bash', arguments: { command: 'rg -n "commands.register"' } },
}))
draw('arguments complete \u2014 the call reads as its command',
  h(LiveStream, { view, reasoningRows: 3 }))

// Commit. The live copy goes, and the transcript keeps a summary.
const committedView = reduce(view, {
  type: 'end', revision: 1, outcome: { kind: 'committed', eventType: 'assistant/message', seq: 42 },
})
draw('committed \u2014 transcript keeps the answer and a reasoning summary',
  h(Committed, { entry: committedView.committed[0], seconds: 8 }))

const checks = []
const record = (name, pass) => { checks.push({ name, pass }) }

record('committed text appears once, not in both live and transcript',
  committedView.text === '' && committedView.committed[0].text !== '')
record('reasoning is summarized, not replayed into the transcript',
  committedView.committed[0].reasoningChars > 0
  && !strip(renderToString(h(Committed, { entry: committedView.committed[0], seconds: 8 }), { columns: WIDTH }))
    .includes(REASONING[0]))

// A retry. Revision 2 replaces revision 1's text instead of appending to it.
let retry = reduce(empty(), { type: 'start', revision: 1 })
retry = reduce(retry, chunk(1, { type: 'text-delta', text: 'FIRST ATTEMPT TEXT' }))
retry = reduce(retry, { type: 'start', revision: 2 })
retry = reduce(retry, chunk(2, { type: 'text-delta', text: 'Second attempt.' }))
record('a replacement stream clears the previous attempt', retry.text === 'Second attempt.')
record('a stale-revision chunk is ignored',
  reduce(retry, chunk(1, { type: 'text-delta', text: ' STALE' })).text === 'Second attempt.')

// Abandonment. Nothing commits, and nothing lingers.
const abandoned = reduce(retry, { type: 'end', revision: 2, outcome: { kind: 'abandoned' } })
record('an abandoned stream leaves nothing live and nothing committed',
  abandoned.text === '' && abandoned.committed.length === 0)

console.log('\nstream reducer:')
for (const check of checks) console.log(`  ${check.pass ? 'ok  ' : 'FAIL'} ${check.name}`)
const failed = checks.filter(check => !check.pass).length
console.log(failed === 0 ? '\nAll stream rules hold.' : `\n${failed} rule(s) broken.`)
process.exit(failed === 0 ? 0 : 1)
