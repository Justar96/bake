/** The `compaction-basic` settings section layered over the composition entry. */

import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import type { Fiber } from '@deepseek-ai/cordis'
import LlmRuntime from 'bake-llm'
import SessionStore from 'bake-session'
import SessionProjectionRegistry from 'bake-session-projection'
import TokenMeter from 'bake-token-meter'
import { FileSettingsProvider } from 'bake-settings-file'
import BasicCompactionEngine, { COMPACTION_BASIC_SETTINGS_NAMESPACE } from 'bake-compaction-basic'
import type { BasicCompactionConfig } from 'bake-compaction-basic'

const NS = COMPACTION_BASIC_SETTINGS_NAMESPACE
const WINDOW = 200_000
const ROUTE = { provider: 'cliproxyapi', model: 'gpt-5-codex' }
const OTHER_MODEL = { provider: 'cliproxyapi', model: 'claude-sonnet-4-5' }
const OTHER_ROUTE = { provider: 'deepseek', model: 'deepseek-chat' }

const cleanups: Array<() => Promise<void>> = []

afterEach(async () => {
  while (cleanups.length > 0) await cleanups.pop()!()
})

interface Bench {
  ctx: Context
  settingsFiber: Fiber
  engineFiber: Fiber
  path: string
  engine: () => BasicCompactionEngine
}

/** Boot a real settings-file provider over a temp home, then the engine. */
async function boot(
  config: BasicCompactionConfig,
  options: { stored?: unknown; watch?: boolean } = {},
): Promise<Bench> {
  const dir = await mkdtemp(join(tmpdir(), 'dsh-compaction-settings-'))
  cleanups.push(() => rm(dir, { recursive: true, force: true }))
  const path = join(dir, 'settings.yaml')
  if (options.stored !== undefined) await writeFile(path, JSON.stringify(options.stored))
  const ctx = new Context()
  cleanups.push(async () => {
    await ctx.fiber.dispose()
  })
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(SessionStore)
  await ctx.plugin(SessionProjectionRegistry)
  await ctx.plugin(TokenMeter)
  const settingsFiber = ctx.plugin(FileSettingsProvider, { path, watch: options.watch ?? false })
  await settingsFiber.await()
  const engineFiber = ctx.plugin(BasicCompactionEngine, config)
  await engineFiber.await()
  return {
    ctx,
    settingsFiber,
    engineFiber,
    path,
    engine: () => ctx.get('compaction') as unknown as BasicCompactionEngine,
  }
}

const threshold = (bench: Bench, route: typeof ROUTE): number | undefined => (
  bench.engine().pressureThreshold(route, WINDOW)
)

describe('compaction-basic settings section', () => {
  it('layers a stored route-wide policy over the composition entry', async () => {
    const bench = await boot({ thresholdRatio: 0.7 }, {
      stored: { [NS]: { modelPolicies: [{ provider: 'cliproxyapi', thresholdTokens: 150_000, retainTokens: 30_000 }] } },
    })

    expect(threshold(bench, ROUTE)).toBe(150_000)
    expect(threshold(bench, OTHER_MODEL)).toBe(150_000)
    expect(threshold(bench, OTHER_ROUTE)).toBe(140_000)
    expect(bench.engine().config.thresholdRatio).toBe(0.7)
  })

  it('applies a committed update at the next pressure check without a restart', async () => {
    const bench = await boot({})
    expect(threshold(bench, ROUTE)).toBe(160_000)

    await bench.ctx.settings.update(NS, {
      modelPolicies: [
        { provider: 'cliproxyapi', thresholdTokens: 120_000 },
        { provider: 'cliproxyapi', model: ROUTE.model, thresholdRatio: 0.5 },
      ],
    })

    expect(threshold(bench, ROUTE)).toBe(100_000)
    expect(threshold(bench, OTHER_MODEL)).toBe(120_000)
    expect(threshold(bench, OTHER_ROUTE)).toBe(160_000)
  })

  it('refuses an invalid write with an actionable message and keeps the last valid policy', async () => {
    const bench = await boot({}, { stored: { [NS]: { thresholdTokens: 150_000 } } })
    expect(threshold(bench, ROUTE)).toBe(150_000)

    await expect(bench.ctx.settings.update(NS, { thresholdRatio: 0.5 }))
      .rejects.toThrow(/settings "compaction-basic": thresholdRatio and thresholdTokens are mutually exclusive/)
    await expect(bench.ctx.settings.update(NS, { retainTokens: 150_000 }))
      .rejects.toThrow(/retainTokens \(150000\) must be less than the resolved thresholdTokens \(150000\)/)
    await expect(bench.ctx.settings.update(NS, {
      modelPolicies: [{ provider: 'cliproxyapi' }, { provider: 'cliproxyapi' }],
    })).rejects.toThrow(/duplicate provider-wide model policy for cliproxyapi/)

    expect(threshold(bench, ROUTE)).toBe(150_000)
  })

  it('switches automatic compaction off from the section, but never on over a manual-only composition', async () => {
    const bench = await boot({})
    expect(bench.engine().config.auto).toBe(true)

    await bench.ctx.settings.update(NS, { auto: false })
    expect(bench.engine().config.auto).toBe(false)
    expect(threshold(bench, ROUTE)).toBeUndefined()

    await bench.ctx.settings.mutate(NS, [{ op: 'unset', path: ['auto'] }])
    expect(threshold(bench, ROUTE)).toBe(160_000)

    const manual = await boot({ auto: false }, { stored: { [NS]: { auto: true } } })
    expect(manual.engine().config.auto).toBe(false)
  })

  it('serves every engine in the process from the section the first one registered', async () => {
    const bench = await boot({})
    // A preset's own engine beside the host's, in a realm of its own.
    const preset = bench.ctx.isolate('compaction')
    await preset.plugin(BasicCompactionEngine, { thresholdRatio: 0.6 }).await()
    const follower = (): BasicCompactionEngine => preset.get('compaction') as unknown as BasicCompactionEngine
    expect(follower()).not.toBe(bench.engine())
    expect(follower().pressureThreshold(ROUTE, WINDOW)).toBe(120_000)

    await bench.ctx.settings.update(NS, { thresholdTokens: 100_000 })
    // The user's form replaces the follower's own composed ratio, as it does the owner's.
    expect(follower().pressureThreshold(ROUTE, WINDOW)).toBe(100_000)
    expect(threshold(bench, ROUTE)).toBe(100_000)

    await bench.ctx.settings.update(NS, { auto: false })
    expect(follower().pressureThreshold(ROUTE, WINDOW)).toBeUndefined()

    await bench.settingsFiber.dispose()
    expect(follower().pressureThreshold(ROUTE, WINDOW)).toBe(120_000)
  })

  it('keeps the composition policy over a section invalid at startup, and accepts its repair', async () => {
    const bench = await boot({ thresholdRatio: 0.6 }, {
      stored: { [NS]: { thresholdRatio: 0.5, thresholdTokens: 100_000 } },
    })

    expect(threshold(bench, ROUTE)).toBe(120_000)
    // Still registered, so the section stays editable without a restart.
    expect(bench.ctx.settings.describe().map(row => String(row.ns))).toContain(NS)

    await bench.ctx.settings.replace(NS, { thresholdTokens: 100_000 })

    expect(threshold(bench, ROUTE)).toBe(100_000)
  })

  it('warns with the actionable reason when it keeps the composition policy', async () => {
    const bench = await boot({ auto: false }, {
      stored: { [NS]: { modelPolicies: [{ provider: 'cliproxyapi', thresholdTokens: 1_000, retainTokens: 2_000 }] } },
    })
    const warnings: string[] = []
    bench.ctx.logger.warn = ((message: string) => void warnings.push(message)) as typeof bench.ctx.logger.warn
    await bench.engineFiber.dispose()

    const engine = new BasicCompactionEngine(bench.ctx, { thresholdRatio: 0.6 })

    // The optional settings injection attaches after the constructor returns.
    await expect.poll(() => warnings).toEqual([
      'compaction-basic: keeping the previous compaction policy: settings "compaction-basic": modelPolicies[0]: '
      + 'retainTokens (2000) must be less than the resolved thresholdTokens (1000)',
    ])
    expect(engine.pressureThreshold(ROUTE, WINDOW)).toBe(120_000)
  })

  it('falls back to the composition entry when the settings provider detaches', async () => {
    const bench = await boot({ thresholdRatio: 0.6 }, { stored: { [NS]: { thresholdTokens: 50_000 } } })
    expect(threshold(bench, ROUTE)).toBe(50_000)

    await bench.settingsFiber.dispose()

    expect(threshold(bench, ROUTE)).toBe(120_000)
  })

  it('releases the namespace when the engine unloads', async () => {
    const bench = await boot({})
    expect(bench.ctx.settings.describe().map(row => String(row.ns))).toContain(NS)

    await bench.engineFiber.dispose()

    expect(bench.ctx.settings.describe().map(row => String(row.ns))).not.toContain(NS)
  })

  // Real filesystem notifications can lag behind chokidar's stability window on busy hosts.
  it('follows external edits, keeping the last valid policy across a bad one', { timeout: 30_000 }, async () => {
    const bench = await boot({}, { watch: true, stored: { [NS]: { thresholdTokens: 150_000 } } })
    expect(threshold(bench, ROUTE)).toBe(150_000)

    const bad = { modelPolicies: [{ provider: 'cliproxyapi', model: '', thresholdTokens: 90_000 }] }
    await writeFile(bench.path, JSON.stringify({ [NS]: bad }))
    // The raw section proves the watcher processed the edit even though validation kept the old resolved value.
    await expect.poll(() => bench.ctx.settings.describe().find(row => row.ns === NS)?.user, { timeout: 10_000 })
      .toEqual(bad)
    expect(threshold(bench, ROUTE)).toBe(150_000)

    await writeFile(bench.path, JSON.stringify({ [NS]: { modelPolicies: [{ provider: 'cliproxyapi', thresholdTokens: 90_000 }] } }))
    await expect.poll(() => threshold(bench, ROUTE), { timeout: 10_000 }).toBe(90_000)
    expect(threshold(bench, OTHER_ROUTE)).toBe(160_000)
  })
})
