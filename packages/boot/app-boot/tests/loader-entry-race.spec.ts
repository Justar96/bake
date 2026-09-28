/**
 * Loader entry-mount race: disabling or removing an entry while its plugin
 * import is still in flight must not let that import, once it resolves,
 * mount a plugin the caller already tore down. `Entry.update()` and
 * `EntryGroup.remove()` can only dispose `entry.fiber`, which does not exist
 * yet during import — see vendor/README.md "Local modifications" for the fix.
 */

import { describe, expect, it, onTestFinished } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader, { type EntryOptions } from '@deepseek-ai/cordis-plugin-loader'

function context(): Context {
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  return ctx
}

async function runtimeContext(): Promise<Context> {
  const ctx = context()
  await ctx.plugin(Loader)
  return ctx
}

describe('Entry mounting races disable/remove against an in-flight import', () => {
  it('does not mount a plugin disabled while its import was pending', async () => {
    const ctx = await runtimeContext()
    let resolveImport!: (exports: unknown) => void
    ctx.loader.builtins.slow = new Promise((resolve) => { resolveImport = resolve })
    let applied = false

    const options: Partial<EntryOptions> = { name: 'cordis:slow', config: {} }
    const created = ctx.loader.create(options)
    // `create()`'s synchronous prefix has already stored the entry and
    // started (but not settled) the import by the time this line runs.
    const id = options.id!
    const entry = ctx.loader.store[id]!
    expect(entry.fiber).toBeUndefined()

    await entry.update({ disabled: true })
    expect(entry.options.disabled).toBe(true)

    resolveImport({ apply() { applied = true } })
    await created
    await ctx.loader.await()

    expect(entry.fiber).toBeUndefined()
    expect(applied).toBe(false)
  })

  it('does not mount a plugin removed while its import was pending', async () => {
    const ctx = await runtimeContext()
    let resolveImport!: (exports: unknown) => void
    ctx.loader.builtins.slow = new Promise((resolve) => { resolveImport = resolve })
    let applied = false

    const options: Partial<EntryOptions> = { name: 'cordis:slow', config: {} }
    const created = ctx.loader.create(options)
    const id = options.id!
    const entry = ctx.loader.store[id]!
    expect(entry.fiber).toBeUndefined()

    ctx.loader.remove(id)
    expect(ctx.loader.store[id]).toBeUndefined()

    resolveImport({ apply() { applied = true } })
    await created
    await ctx.loader.await()

    expect(entry.fiber).toBeUndefined()
    expect(applied).toBe(false)
  })
})
