/**
 * `dsh --self-check`: load what launching this release loads, without
 * starting anything.
 *
 * `bake update` runs it on a downloaded release before switching to it, and
 * a rollback on the release it returns to; `release:pack` runs it on the
 * staged install. A release whose launch would fail to load a package in the
 * platform's installed layout therefore never ships, and never replaces a
 * working install.
 *
 * The check composes the shipped `tui` and `headless` profiles in a private
 * temporary home, never the caller's, and imports every row their trees name
 * through the module resolution a boot uses, as `--dump-config-schema` does.
 * It imports the rows of the shipped agent presets, which the terminal mounts
 * for its sessions, the same way, then the terminal runner's modules, which
 * the terminal loads only once it starts. No plugin is applied, so the check
 * needs no TTY, network, or model key, and it ends once the imports settle.
 * @module @deepseek-ai/dsh/self-check
 */

import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { pathToFileURL } from 'node:url'
import * as yaml from 'js-yaml'
import { entryListSchema, type PatchOptions } from '@deepseek-ai/cordis-plugin-include'
import type { EntryOptions } from '@deepseek-ai/cordis-plugin-loader'
import { generateConfigSchema, type Profile } from 'bake-app-boot'
import { collectConfigDumpLayers } from './dump-config.ts'
import { INSTALL_ANCHOR, prepareProfile } from './profile-boot.ts'

const NAME = 'dsh'

/** The shipped profiles a release must be able to launch. */
export const SELF_CHECK_PROFILES = ['tui', 'headless'] as const

/**
 * The terminal runner's modules under `@dsh-tui/app/lib/`: its entry imports
 * `runner-loader` once the terminal starts, and the runner imports the other
 * two beside itself.
 */
export const TERMINAL_MODULES = ['runner-loader', 'ui-loader', 'syntax-loader'] as const

/** Where the check reports. */
export interface SelfCheckOutput {
  /** The one success line, naming the version. */
  out(line: string): void
  /** One line per problem; the first is the one `bake update` quotes. */
  err(line: string): void
}

/**
 * Run the check.
 * @param version - this release's version, printed on success.
 * @param output - where the report goes.
 * @returns the exit status: 0 when everything loaded, 1 otherwise.
 */
export async function runSelfCheck(version: string, output: SelfCheckOutput = {
  out: line => void process.stdout.write(`${line}\n`),
  err: line => void process.stderr.write(`${line}\n`),
}): Promise<number> {
  const home = mkdtempSync(join(tmpdir(), 'bake-self-check-'))
  const callerHome = process.env.DSH_HOME
  process.env.DSH_HOME = home
  // oxlint-disable-next-line typescript/unbound-method -- Saved only for exact restoration, never called unbound.
  const stdoutWrite = process.stdout.write
  // Imported modules may print; the success line stays the only stdout.
  process.stdout.write = process.stderr.write.bind(process.stderr)
  const problems: string[] = []
  let presets = 0
  try {
    const terminal = await check(problems, 'terminal package', async () => createRequire(INSTALL_ANCHOR).resolve('@dsh-tui/app/package.json'))
    for (const name of SELF_CHECK_PROFILES) {
      const profile = await check(problems, `${name} profile`, async () => prepareProfile(name))
      if (profile === undefined) continue
      await check(problems, `${name} profile`, () => loadTree(problems, `${name} profile`, profile,
        collectConfigDumpLayers(profile, false, []).map(layer => layer.patches)))
      if (name !== 'tui' || terminal === undefined) continue
      const layer = await check(problems, 'agent presets', async () => presetLayer(terminal))
      if (layer === undefined) continue
      presets = layer[0]?.insert?.length ?? 0
      await check(problems, 'agent presets', () => loadTree(problems, 'agent presets', profile, [layer]))
    }
    if (terminal !== undefined) {
      for (const module of TERMINAL_MODULES) {
        await check(problems, `terminal runner ${module}`, () => import(pathToFileURL(join(dirname(terminal), 'lib', `${module}.js`)).href))
      }
    }
  } finally {
    process.stdout.write = stdoutWrite
    if (callerHome === undefined) delete process.env.DSH_HOME
    else process.env.DSH_HOME = callerHome
    rmSync(home, { recursive: true, force: true, maxRetries: 3 })
  }
  if (problems.length > 0) {
    for (const problem of problems) output.err(`${NAME}: self-check: ${problem}`)
    return 1
  }
  output.out(`Bake ${version} self-check passed: the ${SELF_CHECK_PROFILES.join(' and ')} profiles, ${presets} agent presets, and the terminal runner load`)
  return 0
}

/** Run one step, recording its failure as a problem instead of stopping the check. */
async function check<T>(problems: string[], label: string, step: () => Promise<T>): Promise<T | undefined> {
  try {
    return await step()
  } catch (error) {
    problems.push(`${label}: ${firstLine(error)}`)
    return undefined
  }
}

/**
 * Import every row the composed layers name, as a boot of `profile` resolves them.
 * @param problems - where each row that failed to load is recorded.
 * @param label - what the problems name as their source.
 * @param profile - the prepared profile, anchoring module resolution.
 * @param layers - patch lists in application order.
 */
async function loadTree(problems: string[], label: string, profile: Profile, layers: PatchOptions[][]): Promise<void> {
  const dump = await generateConfigSchema(NAME, profile, layers, INSTALL_ANCHOR)
  const { entries, diagnostics } = dump['x-cordis']
  for (const diagnostic of diagnostics) {
    if (diagnostic.level !== 'error') continue
    const entry = diagnostic.path === undefined ? undefined : entries.find(candidate => candidate.path === diagnostic.path)
    const source = entry?.name ?? entry?.id ?? diagnostic.path
    problems.push(`${label}: ${source === undefined ? '' : `${source}: `}${firstLine(diagnostic.message)}`)
  }
}

/**
 * The shipped agent presets as one patch layer of disabled groups, which the
 * schema walk enters without the profile's own rows.
 *
 * A mounted preset resolves a package row from the host composition, as these
 * groups at the profile's root do, and a relative row from its own directory,
 * which this layer writes into the row as a file URL.
 * @param terminal - `@dsh-tui/app`'s manifest, which depends on the presets package.
 * @returns the layer: one group per preset.
 */
function presetLayer(terminal: string): PatchOptions[] {
  const root = join(dirname(createRequire(terminal).resolve('bake-agent-presets/package.json')), 'presets')
  const insert: EntryOptions[] = []
  for (const id of readdirSync(root).sort()) {
    const composition = join(root, id, 'agent.cordis.yml')
    if (!existsSync(composition)) continue
    const rows = yaml.load(readFileSync(composition, 'utf8'), { schema: entryListSchema })
    insert.push({ id: `self-check:preset:${id}`, name: 'cordis:group', disabled: true, config: anchorRows(rows, pathToFileURL(composition)) })
  }
  if (insert.length === 0) throw new Error(`no agent preset under ${root}`)
  return [{ insert }]
}

/** Rewrite relative row names, through nested groups, against the composition that holds them. */
function anchorRows(rows: unknown, base: URL): unknown {
  if (!Array.isArray(rows)) return rows
  return rows.map((row: unknown) => {
    if (row === null || typeof row !== 'object' || Array.isArray(row)) return row
    const { name, config } = row as { name?: unknown; config?: unknown }
    return {
      ...row,
      ...typeof name === 'string' && name.startsWith('.') ? { name: new URL(name, base).href } : {},
      ...name === 'cordis:group' ? { config: anchorRows(config, base) } : {},
    }
  })
}

/** An error's first line, which names the module and what went wrong without its stack. */
function firstLine(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  return message.split('\n', 1)[0]?.trim() ?? ''
}
