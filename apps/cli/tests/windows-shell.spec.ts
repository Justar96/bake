/**
 * The shipped shell composition: the base bundle gates both shell stacks by
 * platform on its own rows (`disabled: !!js process.platform`), so exactly
 * one shell stack mounts per host and no separate platform layer exists —
 * the launcher applies nothing beyond the bundle layers. The spec composes
 * the REAL shipped bundle layers of every shipped profile template (resolved
 * from the app installation anchor) through the boot's patch algorithm and
 * pins the effective per-platform roster, the preset-level gates that keep
 * tool-bash out of win32 sessions and tool-pwsh out of POSIX sessions, and
 * the cold-start resolution closure for the pwsh rows' bare plugin names. A
 * profile that mounts agent presets leaves both shell tool rows to them, so
 * its host plane keeps only the executors, gated the same way.
 */

import { afterEach, describe, expect, it } from 'vitest'
import { mkdtempSync, rmSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import yaml from 'js-yaml'
import { entryListSchema } from '@deepseek-ai/cordis-plugin-include'
import { evaluate } from '@deepseek-ai/cordis-plugin-loader'
import { SHIPPED_PRESET_ROOT } from 'bake-agent-presets'
import { composeEntries, initProfile, loadProfile, PROFILE_TEMPLATES, PROFILES_DIR } from 'bake-app-boot'

/**
 * The effective disabled state of one row on one platform: a `!!js` expression
 * evaluates with a platform-scoped `process` so both outcomes pin on any host.
 */
function disabledOn(row: { disabled?: unknown }, platform: 'win32' | 'linux'): boolean {
  const value = row.disabled
  if (value !== null && typeof value === 'object' && '__jsExpr' in value) {
    return Boolean(evaluate({ process: { platform } }, (value as { __jsExpr: string }).__jsExpr))
  }
  return value === true
}

describe('the shipped shell composition (real bundle layers)', () => {
  let home: string
  afterEach(() => { if (home !== undefined) rmSync(home, { recursive: true, force: true }) })
  // The app installation anchor, mirroring profile-boot.ts: the bundle layers
  // resolve from the REAL shipped bundle packages through it, so this suite
  // composes the shipped patch files, not test fixtures.
  const anchor = fileURLToPath(new URL('../package.json', import.meta.url))

  it('ships the tui, headless, and desktop profile templates', () => {
    // Guards the roster below: a template added without shell coverage fails here.
    expect(Object.keys(PROFILE_TEMPLATES).sort()).toEqual(['desktop', 'headless', 'tui'])
  })

  it.each(Object.keys(PROFILE_TEMPLATES))(
    'composes the confined pwsh roster on win32 and the bash roster on POSIX from the same rows (%s)',
    async (name) => {
      home = mkdtempSync(join(tmpdir(), 'dsh-windows-home-'))
      initProfile(join(home, PROFILES_DIR, name), [...PROFILE_TEMPLATES[name]!.bundles])
      const profile = await loadProfile('dsh', name, anchor, home)
      const warnings: string[] = []
      const rows = composeEntries(
        profile.layers.map(layer => layer.patches),
        message => warnings.push(message),
      )
      const byId = new Map(rows.map(row => [row.id, row]))
      // The terminal profile composes every session from an agent preset, and
      // the presets mount the shell tools, gated as below; its host copies stay off.
      const roster = byId.get('agent-presets')
      const presetHosted = roster !== undefined && roster.disabled !== true
      expect(presetHosted, 'mounts agent presets').toBe(name === 'tui')
      // One shared patch set, two rosters: the shell stacks gate themselves,
      // and no surface layer overrides that gating.
      for (const [id, win32] of [
        ['bash-sandbox', true], ['tool-bash', true], ['pwsh-sandbox', false], ['tool-pwsh', false],
      ] as const) {
        const row = byId.get(id)
        if (row === undefined) throw new TypeError(`profile ${name} must compose ${id}`)
        const presetOwned = presetHosted && id.startsWith('tool-')
        expect(disabledOn(row, 'win32'), `${id} on win32`).toBe(presetOwned || win32)
        expect(disabledOn(row, 'linux'), `${id} on linux`).toBe(presetOwned || !win32)
      }
      // The permission surface never moves: the sandbox/policy rows,
      // fs-sandbox, and the approval service stay enabled exactly as on
      // POSIX — the confined pwsh executor is what changes.
      for (const id of ['permission', 'sandbox', 'sandbox-policy', 'fs-sandbox', 'approval']) {
        expect(byId.has(id), `row ${id}`).toBe(true)
        expect(byId.get(id)?.disabled, `row ${id}`).not.toBe(true)
      }
      // The launcher's cold-start module fallback BFS-links the apps/cli
      // dependency closure into the profile's node_modules, so every bare
      // plugin name in the base patch must resolve from there.
      const cliManifest = JSON.parse(readFileSync(anchor, 'utf8')) as { dependencies?: Record<string, string> }
      for (const bundle of [...PROFILE_TEMPLATES[name]!.bundles, 'bake-pwsh-sandbox', 'bake-tool-pwsh']) {
        expect(cliManifest.dependencies?.[bundle], `cold-start closure must reach ${bundle}`).toBeDefined()
      }
      expect(warnings).toEqual([])
    },
  )

  it('base-only profiles carry both stacks with the same platform gating', async () => {
    home = mkdtempSync(join(tmpdir(), 'dsh-windows-home-'))
    initProfile(join(home, PROFILES_DIR, 'base-only'), ['bake-base'])
    const profile = await loadProfile('dsh', 'base-only', anchor, home)
    const warnings: string[] = []
    const rows = composeEntries(
      profile.layers.map(layer => layer.patches),
      message => warnings.push(message),
    )
    const byId = new Map(rows.map(row => [row.id, row]))
    for (const id of ['bash-sandbox', 'tool-bash', 'pwsh-sandbox', 'tool-pwsh']) {
      expect(byId.has(id), `row ${id}`).toBe(true)
    }
    // No surface overlay: the tool rows keep their own gating too.
    expect(disabledOn(byId.get('tool-bash')!, 'win32'), 'tool-bash on win32').toBe(true)
    expect(disabledOn(byId.get('tool-bash')!, 'linux'), 'tool-bash on linux').toBe(false)
    expect(disabledOn(byId.get('tool-pwsh')!, 'win32'), 'tool-pwsh on win32').toBe(false)
    expect(disabledOn(byId.get('tool-pwsh')!, 'linux'), 'tool-pwsh on linux').toBe(true)
    expect(warnings).toEqual([])
  })
})

describe('shipped agent presets gate both shell tools by platform', () => {
  const presetRoot = SHIPPED_PRESET_ROOT

  it.each(['standard', 'ptc', 'cordis'])('preset %s gates its shell tool rows by platform', (preset) => {
    const entries: unknown = yaml.load(
      readFileSync(join(presetRoot, preset, 'agent.cordis.yml'), 'utf8'),
      { schema: entryListSchema },
    )
    if (!Array.isArray(entries)) throw new TypeError(`preset ${preset} must parse to an entry array`)
    for (const [id, win32] of [['tool-bash', true], ['tool-pwsh', false]] as const) {
      const row = entries.find((entry): entry is Record<string, unknown> => (
        typeof entry === 'object' && entry !== null && (entry as Record<string, unknown>).id === id
      ))
      if (row === undefined) throw new TypeError(`preset ${preset} must mount ${id}`)
      expect(row.disabled).toMatchObject({ __jsExpr: expect.any(String) as string })
      // A platform-scoped context pins both outcomes on every host.
      const expression = (row.disabled as { __jsExpr: string }).__jsExpr
      expect(Boolean(evaluate({ process: { platform: 'win32' } }, expression)), `${id} on win32`).toBe(win32)
      expect(Boolean(evaluate({ process: { platform: 'linux' } }, expression)), `${id} on linux`).toBe(!win32)
    }
  })

  it('minimal mounts no shell tool row and gates its persistent shell stack by platform', () => {
    const entries: unknown = yaml.load(
      readFileSync(join(presetRoot, 'minimal', 'agent.cordis.yml'), 'utf8'),
      { schema: entryListSchema },
    )
    if (!Array.isArray(entries)) throw new TypeError('minimal preset must parse to an entry array')
    for (const id of ['tool-bash', 'tool-pwsh']) {
      expect(entries.some(entry => (
        typeof entry === 'object' && entry !== null && (entry as Record<string, unknown>).id === id
      )), `${id} must be absent from minimal`).toBe(false)
    }
    const group = entries.find((entry): entry is Record<string, unknown> => (
      typeof entry === 'object' && entry !== null && (entry as Record<string, unknown>).id === 'persistent-shell'
    ))
    if (group === undefined) throw new TypeError('minimal preset must mount persistent-shell')
    const rows = group.config as unknown[]
    if (!Array.isArray(rows)) throw new TypeError('persistent-shell must carry a row list')
    const byId = new Map(rows
      .filter((entry): entry is Record<string, unknown> => typeof entry === 'object' && entry !== null)
      .map(entry => [entry.id, entry]))
    // The bash stack (terminal-bash + persistent-bash) mounts on POSIX only; the
    // pwsh twin (terminal-bash with shellDialect pwsh + persistent-pwsh) mounts on
    // win32 only — exactly one persistent shell per host.
    for (const id of ['terminal-bash', 'persistent-bash']) {
      expect(disabledOn(byId.get(id)!, 'win32'), `${id} on win32`).toBe(true)
      expect(disabledOn(byId.get(id)!, 'linux'), `${id} on linux`).toBe(false)
    }
    for (const id of ['terminal-pwsh', 'persistent-pwsh']) {
      expect(disabledOn(byId.get(id)!, 'win32'), `${id} on win32`).toBe(false)
      expect(disabledOn(byId.get(id)!, 'linux'), `${id} on linux`).toBe(true)
    }
    expect(byId.get('terminal-pwsh')?.config).toMatchObject({ shellDialect: 'pwsh' })
  })
})
