/**
 * How the agent-loop eval composes a Bake arm's headless run, read from that
 * arm's own checkout so an older revision is measured as it shipped.
 *
 * A Cordis patch replaces the targeted row's whole `config`, so an overlay row
 * restates every key the row needs: the `system-prompt` overlay starts from the
 * arm's headless bundle row and swaps in only the standard preset's persona
 * prefix, and each `tui` roster row copies the config the terminal profile
 * gives that row. Rows are matched by id, so both package namings
 * (`@deepseek-ai/dsh-*` before the rename, `bake-*` after) resolve alike.
 */
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { isJsExpr, loadCordisYaml } from '../../scripts/cordis-yaml.ts'

export const BASE_BUNDLE = 'packages/bundle/base/cordis.patch.yml'
export const HEADLESS_BUNDLE = 'packages/bundle/headless/cordis.patch.yml'
export const STANDARD_PRESET = 'packages/preset/agent-presets/presets/standard/agent.cordis.yml'
/** The patch the shipped `tui` profile applies over the base bundle. */
export const TUI_PATCH = 'apps/tui/packages/app/cordis.built.patch.yml'

/** Tool rows the `tui` roster copies from the standard preset, which owns them in the terminal. */
const PRESET_TOOL_ROWS = ['tool-fs', 'tool-fs-search', 'tool-result-pruner']

/** Which tool configuration a Bake arm runs with: the headless bundle's own, or the terminal's standard preset. */
export type Roster = 'headless' | 'tui'
export const ROSTERS: readonly Roster[] = ['headless', 'tui']

type Row = Record<string, unknown>
const plain = (value: unknown): value is Row => typeof value === 'object' && value !== null && !Array.isArray(value)

/** Every row in a parsed composition, in file order, including rows inside `insert` lists and group configs. */
function* rowsOf(entries: unknown): Generator<Row> {
  if (!Array.isArray(entries)) return
  for (const entry of entries) {
    if (!plain(entry)) continue
    yield entry
    yield* rowsOf(entry.insert)
    if (Array.isArray(entry.config)) yield* rowsOf(entry.config)
  }
}

/** Fail on a `!!js` value: an overlay is JSON, so a computed config cannot be restated. */
function data(value: unknown, where: string): unknown {
  if (isJsExpr(value)) throw new Error(`${where} computes its value with !!js, which an eval overlay cannot restate`)
  if (Array.isArray(value)) return value.map((item, index) => data(item, `${where}[${index}]`))
  if (plain(value)) return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, data(item, `${where}.${key}`)]))
  return value
}

/**
 * The config a row ends up with across composition layers applied in order:
 * the last layer whose row of that id sets `config`, since each replaces the
 * one before. Missing layer files are skipped.
 * @returns the config, or undefined when no layer configures the row.
 */
export function layeredConfig(root: string, layers: readonly string[], id: string): Row | undefined {
  let found: Row | undefined
  for (const layer of layers) {
    const path = join(root, layer)
    if (!existsSync(path)) continue
    for (const row of rowsOf(loadCordisYaml(readFileSync(path, 'utf8')))) {
      if (row.id !== id || !('config' in row)) continue
      if (!plain(row.config)) throw new Error(`${path}: row ${id} has a non-object config`)
      found = data(row.config, `${path}: row ${id} config`) as Row
    }
  }
  return found
}

/** The standard preset's persona prefix, the text the terminal sends as its identity. */
export function presetPersonaPrefix(root: string): string {
  const prefix = layeredConfig(root, [STANDARD_PRESET], 'persona')?.prefix
  if (typeof prefix !== 'string' || prefix.trim() === '') throw new Error(`${join(root, STANDARD_PRESET)}: no persona row with a prefix`)
  return prefix
}

/**
 * The `system-prompt` overlay row for one arm: its headless composition's own
 * config (harness opener off, the working-directory suffix kept), with the
 * persona prefix replaced by its standard preset's.
 */
export function systemPromptOverlay(root: string): Row {
  const config = layeredConfig(root, [BASE_BUNDLE, HEADLESS_BUNDLE], 'system-prompt')
  if (config === undefined) throw new Error(`${root}: neither the base nor the headless bundle configures system-prompt`)
  return { id: 'system-prompt', config: { ...config, personaPrefix: presetPersonaPrefix(root) } }
}

/**
 * The overlay rows that give one arm a roster's tool configuration. `headless`
 * adds none. `tui` copies the standard preset's `tool-fs`, `tool-fs-search`,
 * and `tool-result-pruner` configs and the terminal profile's `spill-policy`
 * inline cap, falling back to the base bundle's where an older terminal patch
 * sets none. `ask_user_question` is not among them: headless has no one to
 * answer it.
 */
export function rosterOverlay(root: string, roster: Roster): Row[] {
  if (roster === 'headless') return []
  const rows: Row[] = []
  for (const id of PRESET_TOOL_ROWS) {
    const config = layeredConfig(root, [STANDARD_PRESET], id)
    if (config === undefined) {
      if (id === 'tool-fs') throw new Error(`${join(root, STANDARD_PRESET)}: no tool-fs config`)
      continue
    }
    rows.push({ id, config })
  }
  const spill = layeredConfig(root, [BASE_BUNDLE, TUI_PATCH], 'spill-policy')
  if (spill !== undefined) rows.push({ id: 'spill-policy', config: spill })
  return rows
}

/** Text of a system or developer message's content: a string, or text blocks. */
function contentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content.map(block => plain(block) && typeof block.text === 'string' ? block.text : '').join('')
}

/**
 * The system prompt one captured request carries, whichever wire it used:
 * Anthropic Messages `system`, OpenAI Responses `instructions`, or system and
 * developer messages in `input` or `messages`.
 * @returns the prompt text, or null when the request carries none.
 */
export function systemPromptOf(payload: unknown): string | null {
  if (!plain(payload)) return null
  const parts: string[] = []
  if (payload.system !== undefined) parts.push(contentText(payload.system))
  if (typeof payload.instructions === 'string') parts.push(payload.instructions)
  for (const list of [payload.input, payload.messages]) {
    if (!Array.isArray(list)) continue
    for (const item of list) if (plain(item) && (item.role === 'system' || item.role === 'developer')) parts.push(contentText(item.content))
  }
  const text = parts.filter(Boolean).join('\n\n')
  return text === '' ? null : text
}

/** The harness identity opener the persona replaces. */
export const HARNESS_OPENER = 'powered by DeepSeek Harness'

/** Whether a rendered system prompt is the composition Bake ships: no harness opener, and the working-directory line. */
export function compositionCheck(prompt: string, cwds: readonly string[]) {
  const harnessOpener = prompt.includes(HARNESS_OPENER)
  const cwdLine = cwds.some(cwd => prompt.includes(`Your working directory is ${cwd}.`))
  return { harnessOpener, cwdLine, ok: !harnessOpener && cwdLine }
}
