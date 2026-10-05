/**
 * Model-surface snapshots for the terminal profile: the exact system prompt,
 * context messages, and tool declarations each shipped agent preset sends on
 * its first model request.
 *
 * Each case boots the composed terminal profile (see `composed-profile.ts`),
 * opens one preset as the runner does, submits one task, and renders the
 * request the scripted model received, normalized for paths, homes, the model
 * id, and dates, against `expected/model-surface/<preset>.md`.
 *
 * Update after an intended change with
 * `node node_modules/vitest/vitest.mjs run --config apps/tui/vitest.config.ts apps/tui/packages/app/tests/model-surface.spec.ts -u`.
 * Any diff here is a model-visible change and needs an eval record.
 *
 * Windows ships `pwsh` in place of `bash`, a different tool, so the snapshots
 * are recorded on POSIX and the cases skip on Windows.
 */
import { homedir, tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  MODEL_SURFACE_TASK,
  normalizeModelSurface,
  pathPlaceholders,
  plantModelSurfaceWorkspace,
  renderModelSurface,
} from 'bake-loader-smoke'
import { composedProfile, type ShippedPreset } from './composed-profile.ts'

/** The default model id the overlay selects; the persona may name it, so it is normalized too. */
const MODEL_ID = 'surface-model'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const dispose of cleanup.splice(0).reverse()) await dispose()
  vi.unstubAllEnvs()
})

describe.skipIf(process.platform === 'win32')('terminal preset model surface', () => {
  it.each<ShippedPreset>(['standard', 'ptc', 'cordis', 'minimal'])('pins the %s preset', async (preset) => {
    // Settings that change what the model is sent, which a developer shell may export.
    for (const name of ['PERMISSION_MODE', 'TOOLS_MODE']) {
      vi.stubEnv(`BAKE_${name}`, undefined)
      vi.stubEnv(`DSH_${name}`, undefined)
    }
    const { model, open, workspace, root, home, agentsHome } = await composedProfile(cleanup, {
      plant: plantModelSurfaceWorkspace,
      model: MODEL_ID,
    })
    const session = await open(preset)
    await session.turn(MODEL_SURFACE_TASK)
    const request = model.requests.find(candidate => candidate.purpose === undefined)
    if (request === undefined) throw new Error('the turn made no agent-loop request')

    const rendered = renderModelSurface(`tui, ${preset} preset (bake-base + bake-tui-app)`, normalizeModelSurface(request, [
      ...pathPlaceholders(workspace, '<cwd>'),
      ...pathPlaceholders(home, '<bake-home>'),
      ...pathPlaceholders(agentsHome, '<agents-home>'),
      ...pathPlaceholders(root, '<root>'),
      ...pathPlaceholders(tmpdir(), '<tmp>'),
      ...pathPlaceholders(homedir(), '<home>'),
      [MODEL_ID, '<model>'],
    ]))
    await expect(rendered).toMatchFileSnapshot(fileURLToPath(new URL(`./expected/model-surface/${preset}.md`, import.meta.url)))
  })
})
